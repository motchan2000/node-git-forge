//! git smart HTTP: info/refs, git-upload-pack (protocol v2 with a v0 fallback) and git-receive-pack
//! (v0). Objects go into the forge's store; refs move through Forge::set_ref.

use crate::http::{Request, Response};
use crate::objects::*;
use crate::pack::*;
use crate::store::Forge;
use crate::util::{is_oid, percent_decode};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, OnceLock, RwLock};

const AGENT: &str = "agent=forge/1.0";

fn plain(status: u16, text: &str) -> Response {
    Response::bytes(status, "text/plain; charset=utf-8", text.as_bytes().to_vec())
}

fn git(content_type: &str, body: Vec<u8>) -> Response {
    // Only the content type: every extra header is parsing work for a CPU-starved client.
    Response::bytes(200, content_type, body)
}

fn wants_v2(req: &Request) -> bool {
    req.header("git-protocol").map(|v| v.split(':').any(|p| p.trim() == "version=2")).unwrap_or(false)
}

fn v2_cache() -> &'static RwLock<HashMap<Vec<u8>, Arc<Vec<u8>>>> {
    static C: OnceLock<RwLock<HashMap<Vec<u8>, Arc<Vec<u8>>>>> = OnceLock::new();
    C.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Parse `/{owner}/{repo}.git/{action}`.
pub fn route(path: &str) -> Option<(String, String, &str)> {
    let rest = path.strip_prefix('/')?;
    let (owner, rest) = rest.split_once('/')?;
    let dot = rest.find(".git/")?;
    let name = &rest[..dot];
    let action = &rest[dot + 5..];
    if owner.is_empty() || name.is_empty() || !matches!(action, "info/refs" | "git-upload-pack" | "git-receive-pack") {
        return None;
    }
    Some((percent_decode(owner), percent_decode(name), action))
}

fn cache_key(id: u64, ref_version: u64, body: &[u8]) -> Option<Vec<u8>> {
    if body.len() > 8192 {
        return None;
    }
    let mut k = format!("{}:{}:", id, ref_version).into_bytes();
    k.extend_from_slice(body);
    Some(k)
}

pub fn handle(forge: &RwLock<Forge>, req: &Request, owner: &str, name: &str, action: &str, authorized: bool) -> Response {
    if !authorized {
        return plain(401, "Authentication required\n").with_header("www-authenticate", "Basic realm=\"forge\"");
    }
    // Reads first, under a shared lock: advertisements and cached v2 answers never wait on each other.
    {
        let f = forge.read().unwrap_or_else(|e| e.into_inner());
        let Some(id) = f.repo_id(owner, name) else { return plain(404, "Repository not found\n") };
        if action == "info/refs" {
            if req.method != "GET" && req.method != "HEAD" {
                return plain(405, "Method Not Allowed\n");
            }
            let service = req.param("service").unwrap_or_default();
            if service != "git-upload-pack" && service != "git-receive-pack" {
                return plain(403, "dumb HTTP is not supported; pass ?service=\n");
            }
            return advertise(&f, id, &service, wants_v2(req));
        }
        if req.method == "POST" && action == "git-upload-pack" && wants_v2(req) {
            if let Some(k) = cache_key(id, f.repo(id).ref_version, &req.body) {
                if let Some(hit) = v2_cache().read().unwrap_or_else(|e| e.into_inner()).get(&k) {
                    return git("application/x-git-upload-pack-result", hit.as_ref().clone());
                }
            }
        }
    }
    let mut f = forge.write().unwrap_or_else(|e| e.into_inner());
    let Some(id) = f.repo_id(owner, name) else { return plain(404, "Repository not found\n") };
    if req.method != "POST" {
        return plain(405, "Method Not Allowed\n");
    }
    if req.header("content-encoding").map(|e| e.contains("gzip")).unwrap_or(false) {
        return plain(415, "gzip request bodies are not supported\n");
    }
    let result = if action == "git-upload-pack" {
        if wants_v2(req) {
            upload_v2(&mut f, id, &req.body)
        } else {
            upload_v0(&mut f, id, &req.body)
        }
    } else {
        receive(&mut f, id, &req.body)
    };
    match result {
        Ok(r) => r,
        Err(e) => plain(400, &format!("{}\n", e)),
    }
}

// ---------------------------------------------------------------------------------------------
// advertisements

fn advertise(f: &Forge, id: u64, service: &str, v2: bool) -> Response {
    let ct = format!("application/x-{}-advertisement", service);
    if service == "git-upload-pack" && v2 {
        let mut out = Vec::new();
        for l in ["version 2\n", &format!("{}\n", AGENT), "ls-refs=unborn\n", "fetch=shallow\n", "server-option\n", "object-format=sha1\n"] {
            out.extend_from_slice(&pkt_str(l));
        }
        out.extend_from_slice(FLUSH);
        return git(&ct, out);
    }
    let mut out = pkt_str(&format!("# service={}\n", service));
    out.extend_from_slice(FLUSH);
    let head = f.head_branch(id);
    let caps = if service == "git-upload-pack" {
        format!("multi_ack_detailed side-band-64k side-band ofs-delta shallow deepen-since deepen-not deepen-relative no-progress include-tag thin-pack allow-tip-sha1-in-want allow-reachable-sha1-in-want symref=HEAD:refs/heads/{} object-format=sha1 {}", head, AGENT)
    } else {
        format!("report-status delete-refs side-band-64k quiet ofs-delta object-format=sha1 {}", AGENT)
    };
    let repo = f.repo(id);
    let mut lines: Vec<String> = Vec::new();
    if service == "git-upload-pack" {
        if let Some(h) = repo.refs.get(&format!("refs/heads/{}", head)) {
            lines.push(format!("{} HEAD", h));
        }
    }
    for (r, oid) in &repo.refs {
        lines.push(format!("{} {}", oid, r));
        if service == "git-upload-pack" && r.starts_with("refs/tags/") {
            if let Some(p) = f.objects.peel_to_commit(oid) {
                if p != *oid {
                    lines.push(format!("{} {}^{{}}", p, r));
                }
            }
        }
    }
    if lines.is_empty() {
        lines.push(format!("{} capabilities^{{}}", ZERO_OID));
    }
    for (i, l) in lines.iter().enumerate() {
        out.extend_from_slice(&pkt_str(&if i == 0 { format!("{}\0{}\n", l, caps) } else { format!("{}\n", l) }));
    }
    out.extend_from_slice(FLUSH);
    git(&ct, out)
}

// ---------------------------------------------------------------------------------------------
// upload-pack v2

fn upload_v2(f: &mut Forge, id: u64, body: &[u8]) -> Result<Response, String> {
    let ct = "application/x-git-upload-pack-result";
    let key = cache_key(id, f.repo(id).ref_version, body);
    if let Some(k) = &key {
        if let Some(hit) = v2_cache().read().unwrap_or_else(|e| e.into_inner()).get(k) {
            return Ok(git(ct, hit.as_ref().clone()));
        }
    }
    let (packets, _) = parse_packets(body, 0, 1)?;
    let mut command = String::new();
    let mut args: Vec<String> = Vec::new();
    let mut in_args = false;
    for p in &packets {
        match p {
            Packet::Flush => break,
            Packet::Delim => in_args = true,
            _ => {
                if let Some(line) = line_of(p) {
                    if !in_args {
                        if let Some(c) = line.strip_prefix("command=") {
                            command = c.to_string();
                        }
                    } else {
                        args.push(line);
                    }
                }
            }
        }
    }
    let out = match command.as_str() {
        "ls-refs" => ls_refs(f, id, &args),
        "fetch" => fetch_v2(f, id, &args)?,
        _ => return Err(format!("unknown command {:?}", command)),
    };
    if let Some(k) = key {
        let mut c = v2_cache().write().unwrap_or_else(|e| e.into_inner());
        if c.len() >= 256 {
            c.clear();
        }
        c.insert(k, Arc::new(out.clone()));
    }
    Ok(git(ct, out))
}

fn ls_refs(f: &Forge, id: u64, args: &[String]) -> Vec<u8> {
    let symrefs = args.iter().any(|a| a == "symrefs");
    let peel = args.iter().any(|a| a == "peel");
    let unborn = args.iter().any(|a| a == "unborn");
    let prefixes: Vec<&str> = args.iter().filter_map(|a| a.strip_prefix("ref-prefix ")).collect();
    let keep = |n: &str| prefixes.is_empty() || prefixes.iter().any(|p| n.starts_with(p));
    let mut out = Vec::new();
    let head = f.head_branch(id);
    let repo = f.repo(id);
    if keep("HEAD") {
        let target = format!("refs/heads/{}", head);
        let sym = if symrefs { format!(" symref-target:{}", target) } else { String::new() };
        match repo.refs.get(&target) {
            Some(oid) => out.extend_from_slice(&pkt_str(&format!("{} HEAD{}\n", oid, sym))),
            None if unborn => out.extend_from_slice(&pkt_str(&format!("unborn HEAD{}\n", sym))),
            None => {}
        }
    }
    for (r, oid) in &repo.refs {
        if !keep(r) {
            continue;
        }
        let mut line = format!("{} {}", oid, r);
        if peel && r.starts_with("refs/tags/") {
            if let Some(p) = f.objects.peel_to_commit(oid) {
                if p != *oid {
                    line.push_str(&format!(" peeled:{}", p));
                }
            }
        }
        line.push('\n');
        out.extend_from_slice(&pkt_str(&line));
    }
    out.extend_from_slice(FLUSH);
    out
}

#[derive(Default)]
struct FetchArgs {
    wants: Vec<String>,
    haves: Vec<String>,
    shallow: HashSet<String>,
    depth: Option<usize>,
    done: bool,
    include_tag: bool,
}

fn fetch_v2(f: &mut Forge, id: u64, args: &[String]) -> Result<Vec<u8>, String> {
    let mut a = FetchArgs::default();
    for line in args {
        if let Some(v) = line.strip_prefix("want ") {
            a.wants.push(v.trim().to_lowercase());
        } else if let Some(v) = line.strip_prefix("want-ref ") {
            let oid = f.repo(id).refs.get(v.trim()).cloned().ok_or_else(|| format!("unknown ref {}", v))?;
            a.wants.push(oid);
        } else if let Some(v) = line.strip_prefix("have ") {
            a.haves.push(v.trim().to_lowercase());
        } else if let Some(v) = line.strip_prefix("shallow ") {
            a.shallow.insert(v.trim().to_lowercase());
        } else if let Some(v) = line.strip_prefix("deepen ") {
            a.depth = Some(v.trim().parse().map_err(|_| "bad deepen value")?);
        } else if line == "done" {
            a.done = true;
        } else if line == "include-tag" {
            a.include_tag = true;
        }
    }
    if a.depth == Some(0) {
        return Err("bad deepen value".into());
    }
    for w in &a.wants {
        if !is_oid(w) {
            return Err(format!("bad want {}", w));
        }
    }
    let mut out = Vec::new();
    if !a.done {
        let common: Vec<&String> = a.haves.iter().filter(|h| f.objects.has(h)).collect();
        out.extend_from_slice(&pkt_str("acknowledgments\n"));
        if common.is_empty() {
            out.extend_from_slice(&pkt_str("NAK\n"));
        }
        for h in common {
            out.extend_from_slice(&pkt_str(&format!("ACK {}\n", h)));
        }
        out.extend_from_slice(&pkt_str("ready\n"));
        out.extend_from_slice(DELIM);
    }
    let (shallow_lines, pack) = plan(f, id, &a)?;
    if !shallow_lines.is_empty() {
        out.extend_from_slice(&pkt_str("shallow-info\n"));
        for l in &shallow_lines {
            out.extend_from_slice(&pkt_str(&format!("{}\n", l)));
        }
        out.extend_from_slice(DELIM);
    }
    out.extend_from_slice(&pkt_str("packfile\n"));
    sideband(1, &pack, 65515, &mut out);
    out.extend_from_slice(FLUSH);
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// upload-pack v0

fn upload_v0(f: &mut Forge, id: u64, body: &[u8]) -> Result<Response, String> {
    let ct = "application/x-git-upload-pack-result";
    let (packets, _) = parse_packets(body, 0, usize::MAX)?;
    let mut a = FetchArgs::default();
    let mut band_mode = 0; // 2 = side-band-64k
    let mut first = true;
    for p in &packets {
        let Some(line) = line_of(p) else { continue };
        if let Some(v) = line.strip_prefix("want ") {
            let mut parts = v.split(' ');
            a.wants.push(parts.next().unwrap_or("").to_lowercase());
            if first {
                let caps: Vec<&str> = parts.collect();
                band_mode = if caps.contains(&"side-band-64k") { 2 } else if caps.contains(&"side-band") { 1 } else { 0 };
                a.include_tag = caps.contains(&"include-tag");
                first = false;
            }
        } else if let Some(v) = line.strip_prefix("have ") {
            a.haves.push(v.trim().to_lowercase());
        } else if let Some(v) = line.strip_prefix("shallow ") {
            a.shallow.insert(v.trim().to_lowercase());
        } else if let Some(v) = line.strip_prefix("deepen ") {
            a.depth = Some(v.trim().parse().map_err(|_| "bad deepen value")?);
        } else if line == "done" {
            a.done = true;
        }
    }
    if a.wants.is_empty() {
        return Ok(git(ct, FLUSH.to_vec()));
    }
    for w in &a.wants {
        if !is_oid(w) {
            return Err(format!("bad want {}", w));
        }
    }
    if a.depth == Some(0) {
        return Err("bad deepen value".into());
    }
    let (shallow_lines, pack) = plan(f, id, &a)?;
    let mut out = Vec::new();
    if a.depth.is_some() || !a.shallow.is_empty() {
        for l in &shallow_lines {
            out.extend_from_slice(&pkt_str(&format!("{}\n", l)));
        }
        out.extend_from_slice(FLUSH);
    }
    match a.haves.iter().find(|h| f.objects.has(h)) {
        Some(h) => out.extend_from_slice(&pkt_str(&format!("ACK {}\n", h))),
        None => out.extend_from_slice(&pkt_str("NAK\n")),
    }
    if band_mode == 0 {
        out.extend_from_slice(&pack);
    } else {
        sideband(1, &pack, if band_mode == 2 { 65515 } else { 995 }, &mut out);
        out.extend_from_slice(FLUSH);
    }
    Ok(git(ct, out))
}

// ---------------------------------------------------------------------------------------------
// what to send

fn servable(f: &mut Forge, id: u64, wants: &[String]) -> bool {
    let mut tips: HashSet<String> = HashSet::new();
    for oid in f.repo(id).refs.values() {
        tips.insert(oid.clone());
        if let Some(p) = f.objects.peel_to_commit(oid) {
            tips.insert(p);
        }
    }
    let rest: Vec<&String> = wants.iter().filter(|w| !tips.contains(*w)).collect();
    if rest.is_empty() {
        return true;
    }
    let mut reach = HashSet::new();
    for t in tips.iter() {
        for c in f.objects.ancestors(t, usize::MAX) {
            reach.insert(c);
        }
    }
    rest.iter().all(|w| reach.contains(*w))
}

fn add_tree(store: &mut ObjectStore, tree: &str, send: &mut Vec<(String, Obj)>, sent: &mut HashSet<String>, skip: &HashSet<String>) {
    if skip.contains(tree) || sent.contains(tree) {
        return;
    }
    let Some(obj) = store.get(tree).cloned() else { return };
    sent.insert(tree.to_string());
    send.push((tree.to_string(), obj));
    let Some(entries) = store.tree(tree) else { return };
    for e in entries.iter() {
        if e.mode == "160000" {
            continue;
        }
        if is_tree_mode(&e.mode) {
            add_tree(store, &e.oid, send, sent, skip);
        } else if !skip.contains(&e.oid) && !sent.contains(&e.oid) {
            if let Some(b) = store.get(&e.oid).cloned() {
                sent.insert(e.oid.clone());
                send.push((e.oid.clone(), b));
            }
        }
    }
}

fn mark_tree(store: &mut ObjectStore, tree: &str, into: &mut HashSet<String>) {
    if !into.insert(tree.to_string()) {
        return;
    }
    let Some(entries) = store.tree(tree) else { return };
    for e in entries.iter() {
        if is_tree_mode(&e.mode) {
            mark_tree(store, &e.oid, into);
        } else {
            into.insert(e.oid.clone());
        }
    }
}

fn plan(f: &mut Forge, id: u64, a: &FetchArgs) -> Result<(Vec<String>, Vec<u8>), String> {
    for w in &a.wants {
        if !f.objects.has(w) {
            return Err(format!("not our ref {}", w));
        }
    }
    if !servable(f, id, &a.wants) {
        return Err("want is not reachable from any ref".into());
    }
    let store = &mut f.objects;
    // The client's commits: from its haves, not past its shallow boundary.
    let mut have_commits: HashSet<String> = HashSet::new();
    let mut stack: Vec<String> = a.haves.iter().filter(|h| store.commit(h).is_some()).cloned().collect();
    while let Some(oid) = stack.pop() {
        if !have_commits.insert(oid.clone()) {
            continue;
        }
        if a.shallow.contains(&oid) {
            continue;
        }
        if let Some(c) = store.commit(&oid) {
            for p in &c.parents {
                if !have_commits.contains(p) {
                    stack.push(p.clone());
                }
            }
        }
    }
    let mut send: Vec<(String, Obj)> = Vec::new();
    let mut sent: HashSet<String> = HashSet::new();
    let mut want_commits: Vec<String> = Vec::new();
    for w in &a.wants {
        let mut oid = Some(w.clone());
        while let Some(o) = oid.take() {
            let Some(obj) = store.get(&o).cloned() else { break };
            match obj.kind {
                Kind::Commit => want_commits.push(o),
                Kind::Tag => {
                    if !have_commits.contains(&o) && sent.insert(o.clone()) {
                        send.push((o.clone(), obj.clone()));
                    }
                    oid = tag_target(&obj.data);
                }
                Kind::Tree => add_tree(store, &o, &mut send, &mut sent, &HashSet::new()),
                Kind::Blob => {
                    if sent.insert(o.clone()) {
                        send.push((o, obj));
                    }
                }
            }
        }
    }
    let mut shallow_lines = Vec::new();
    let mut commits: Vec<String> = Vec::new();
    if let Some(depth) = a.depth {
        let mut depth_of: HashMap<String, usize> = HashMap::new();
        let mut queue: VecDeque<String> = VecDeque::new();
        let mut order: Vec<String> = Vec::new();
        for c in &want_commits {
            if !depth_of.contains_key(c) {
                depth_of.insert(c.clone(), 1);
                queue.push_back(c.clone());
            }
        }
        while let Some(oid) = queue.pop_front() {
            order.push(oid.clone());
            let d = depth_of[&oid];
            let parents = store.commit(&oid).map(|c| c.parents.clone()).unwrap_or_default();
            if d >= depth {
                if !parents.is_empty() && !a.shallow.contains(&oid) {
                    shallow_lines.push(format!("shallow {}", oid));
                }
                continue;
            }
            if a.shallow.contains(&oid) && !parents.is_empty() {
                shallow_lines.push(format!("unshallow {}", oid));
            }
            for p in parents {
                if !depth_of.contains_key(&p) && store.commit(&p).is_some() {
                    depth_of.insert(p.clone(), d + 1);
                    queue.push_back(p);
                }
            }
        }
        for oid in order {
            if !have_commits.contains(&oid) {
                commits.push(oid);
            }
        }
    } else {
        let mut seen: HashSet<String> = HashSet::new();
        let mut walk = want_commits.clone();
        while let Some(oid) = walk.pop() {
            if seen.contains(&oid) || have_commits.contains(&oid) {
                continue;
            }
            seen.insert(oid.clone());
            commits.push(oid.clone());
            if a.shallow.contains(&oid) {
                continue;
            }
            if let Some(c) = store.commit(&oid) {
                for p in &c.parents {
                    walk.push(p.clone());
                }
            }
        }
    }
    let mut have_objects: HashSet<String> = HashSet::new();
    if !commits.is_empty() && !have_commits.is_empty() {
        for c in &have_commits {
            if let Some(t) = store.commit(c).map(|c| c.tree.clone()) {
                mark_tree(store, &t, &mut have_objects);
            }
        }
    }
    for c in &commits {
        let Some(obj) = store.get(c).cloned() else { continue };
        if sent.insert(c.clone()) {
            send.push((c.clone(), obj));
        }
        if let Some(t) = store.commit(c).map(|x| x.tree.clone()) {
            add_tree(store, &t, &mut send, &mut sent, &have_objects);
        }
    }
    if a.include_tag {
        let sent_commits: HashSet<&String> = commits.iter().collect();
        let tags: Vec<String> = f.repo(id).refs.iter().filter(|(r, _)| r.starts_with("refs/tags/")).map(|(_, o)| o.clone()).collect();
        for oid in tags {
            if sent.contains(&oid) {
                continue;
            }
            if let Some(obj) = f.objects.get(&oid).cloned() {
                if obj.kind == Kind::Tag {
                    if let Some(t) = tag_target(&obj.data) {
                        if sent_commits.contains(&t) {
                            sent.insert(oid.clone());
                            send.push((oid, obj));
                        }
                    }
                }
            }
        }
    }
    let rank = |k: Kind| match k {
        Kind::Commit => 0,
        Kind::Tag => 1,
        Kind::Tree => 2,
        Kind::Blob => 3,
    };
    send.sort_by_key(|(_, o)| rank(o.kind));
    Ok((shallow_lines, write_pack(&send)))
}

// ---------------------------------------------------------------------------------------------
// receive-pack

fn receive(f: &mut Forge, id: u64, body: &[u8]) -> Result<Response, String> {
    let ct = "application/x-git-receive-pack-result";
    let (packets, offset) = parse_packets(body, 0, 1)?;
    let mut commands: Vec<(String, String, String)> = Vec::new();
    let mut caps: Vec<String> = Vec::new();
    for p in &packets {
        let Packet::Data(d) = p else { continue };
        let mut text = String::from_utf8_lossy(d).into_owned();
        if let Some(nul) = text.find('\0') {
            caps = text[nul + 1..].trim().split(' ').map(|s| s.to_string()).collect();
            text.truncate(nul);
        }
        let text = text.trim_end_matches('\n');
        let parts: Vec<&str> = text.split(' ').collect();
        if parts.len() != 3 || !is_oid(parts[0]) || !is_oid(parts[1]) {
            return Err(format!("bad command line {:?}", &text[..text.len().min(100)]));
        }
        commands.push((parts[0].to_string(), parts[1].to_string(), parts[2].to_string()));
    }
    if commands.is_empty() {
        return Ok(git(ct, FLUSH.to_vec()));
    }
    let use_band = caps.iter().any(|c| c == "side-band-64k");
    let report = caps.iter().any(|c| c == "report-status" || c == "report-status-v2");
    let pack_bytes = &body[offset..];
    let mut lines: Vec<String> = Vec::new();
    let mut unpack_error: Option<String> = None;
    let mut received: Vec<(String, Obj)> = Vec::new();
    if !pack_bytes.is_empty() {
        let store = &f.objects;
        match read_pack(pack_bytes, &|oid: &str| store.get(oid).cloned()) {
            Ok(objs) => received = objs,
            Err(e) => unpack_error = Some(e),
        }
    }
    if let Some(e) = unpack_error {
        lines.push(format!("unpack {}", e.replace('\n', " ")));
        for (_, _, r) in &commands {
            lines.push(format!("ng {} unpacker error", r));
        }
    } else {
        let new_oids: Vec<String> = received.iter().map(|(o, _)| o.clone()).collect();
        for (oid, obj) in received {
            f.objects.put_known(oid, obj);
        }
        let incomplete = missing_links(f, &new_oids);
        lines.push("unpack ok".into());
        for (old, new, r) in &commands {
            match apply_command(f, id, old, new, r, incomplete) {
                None => lines.push(format!("ok {}", r)),
                Some(reason) => lines.push(format!("ng {} {}", r, reason)),
            }
        }
    }
    let mut out = Vec::new();
    if report {
        let mut inner = Vec::new();
        for l in &lines {
            inner.extend_from_slice(&pkt_str(&format!("{}\n", l)));
        }
        inner.extend_from_slice(FLUSH);
        if use_band {
            sideband(1, &inner, 65515, &mut out);
            out.extend_from_slice(FLUSH);
        } else {
            out = inner;
        }
    } else if use_band {
        out.extend_from_slice(FLUSH);
    }
    Ok(git(ct, out))
}

fn missing_links(f: &mut Forge, oids: &[String]) -> bool {
    for oid in oids {
        let Some(kind) = f.objects.get(oid).map(|o| o.kind) else { continue };
        match kind {
            Kind::Commit => {
                if let Some(c) = f.objects.commit(oid) {
                    if !f.objects.has(&c.tree) || c.parents.iter().any(|p| !f.objects.has(p)) {
                        return true;
                    }
                }
            }
            Kind::Tree => {
                if let Some(entries) = f.objects.tree(oid) {
                    if entries.iter().any(|e| e.mode != "160000" && !f.objects.has(&e.oid)) {
                        return true;
                    }
                }
            }
            Kind::Tag => {
                if let Some(t) = f.objects.get(oid).and_then(|o| tag_target(&o.data)) {
                    if !f.objects.has(&t) {
                        return true;
                    }
                }
            }
            Kind::Blob => {}
        }
    }
    false
}

fn apply_command(f: &mut Forge, id: u64, old: &str, new: &str, r: &str, incomplete: bool) -> Option<&'static str> {
    if !r.starts_with("refs/") || r.contains("..") || r.ends_with('/') || r.ends_with(".lock") || r.chars().any(|c| c.is_whitespace() || "~^:?*[\\".contains(c)) {
        return Some("funny refname");
    }
    let current = f.repo(id).refs.get(r).cloned().unwrap_or_else(|| ZERO_OID.to_string());
    if old != current {
        return Some(if current == ZERO_OID { "stale info" } else { "fetch first" });
    }
    if new == ZERO_OID {
        if current == ZERO_OID {
            return Some("no such ref");
        }
        f.set_ref(id, r, None);
        return None;
    }
    let Some(kind) = f.objects.get(new).map(|o| o.kind) else { return Some("missing necessary objects") };
    if incomplete {
        return Some("missing necessary objects");
    }
    if r.starts_with("refs/heads/") && kind != Kind::Commit {
        return Some("branch must point at a commit");
    }
    if current != ZERO_OID {
        if r.starts_with("refs/tags/") {
            return Some("already exists");
        }
        let (oc, nc) = (f.objects.peel_to_commit(&current), f.objects.peel_to_commit(new));
        match (oc, nc) {
            (Some(o), Some(n)) if f.objects.is_ancestor(&o, &n) => {}
            _ => return Some("non-fast-forward"),
        }
    }
    f.set_ref(id, r, Some(new));
    None
}
