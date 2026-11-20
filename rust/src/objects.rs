//! Git objects: hashing, the content-addressed store, tree/commit codecs and tree/history operations.

use crate::util::{hex, sha1, Sha1};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;

pub const ZERO_OID: &str = "0000000000000000000000000000000000000000";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Commit,
    Tree,
    Blob,
    Tag,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Commit => "commit",
            Kind::Tree => "tree",
            Kind::Blob => "blob",
            Kind::Tag => "tag",
        }
    }
    pub fn code(self) -> u8 {
        match self {
            Kind::Commit => 1,
            Kind::Tree => 2,
            Kind::Blob => 3,
            Kind::Tag => 4,
        }
    }
    pub fn from_code(c: u8) -> Option<Kind> {
        match c {
            1 => Some(Kind::Commit),
            2 => Some(Kind::Tree),
            3 => Some(Kind::Blob),
            4 => Some(Kind::Tag),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub struct Obj {
    pub kind: Kind,
    pub data: Arc<Vec<u8>>,
}

pub fn hash_object(kind: Kind, data: &[u8]) -> String {
    let mut h = Sha1::new();
    h.update(format!("{} {}\0", kind.name(), data.len()).as_bytes());
    h.update(data);
    hex(&h.finish())
}

#[derive(Clone, Debug)]
pub struct TreeEntry {
    pub mode: String,
    pub name: String,
    pub oid: String,
}

pub fn is_tree_mode(mode: &str) -> bool {
    mode == "40000" || mode == "040000"
}

#[derive(Clone, Debug)]
pub struct Signature {
    pub name: String,
    pub email: String,
    pub time: i64,
    pub tz: String,
}

#[derive(Clone, Debug)]
pub struct Commit {
    pub tree: String,
    pub parents: Vec<String>,
    pub author: Signature,
    pub committer: Signature,
    pub message: String,
}

pub fn parse_tree(data: &[u8]) -> Result<Vec<TreeEntry>, String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let space = data[i..].iter().position(|&b| b == b' ').map(|p| p + i).ok_or("malformed tree")?;
        let nul = data[space..].iter().position(|&b| b == 0).map(|p| p + space).ok_or("malformed tree")?;
        if nul + 21 > data.len() {
            return Err("malformed tree".into());
        }
        out.push(TreeEntry {
            mode: String::from_utf8_lossy(&data[i..space]).into_owned(),
            name: String::from_utf8_lossy(&data[space + 1..nul]).into_owned(),
            oid: hex(&data[nul + 1..nul + 21]),
        });
        i = nul + 21;
    }
    Ok(out)
}

pub fn serialize_tree(entries: &[TreeEntry]) -> Vec<u8> {
    let mut sorted: Vec<&TreeEntry> = entries.iter().collect();
    let key = |e: &TreeEntry| -> Vec<u8> {
        let mut k = e.name.as_bytes().to_vec();
        if is_tree_mode(&e.mode) {
            k.push(b'/');
        }
        k
    };
    sorted.sort_by(|a, b| key(a).cmp(&key(b)));
    let mut out = Vec::new();
    for e in sorted {
        out.extend_from_slice(if is_tree_mode(&e.mode) { b"40000" } else { e.mode.as_bytes() });
        out.push(b' ');
        out.extend_from_slice(e.name.as_bytes());
        out.push(0);
        out.extend_from_slice(&crate::util::unhex(&e.oid).unwrap_or_else(|| vec![0; 20]));
    }
    out
}

fn parse_signature(text: &str) -> Signature {
    // "Name <email> 123 +0000"
    if let (Some(lt), Some(gt)) = (text.find('<'), text.rfind('>')) {
        if lt < gt {
            let name = text[..lt].trim_end().to_string();
            let email = text[lt + 1..gt].to_string();
            let rest: Vec<&str> = text[gt + 1..].split_whitespace().collect();
            let time = rest.first().and_then(|t| t.parse().ok()).unwrap_or(0);
            let tz = rest.get(1).unwrap_or(&"+0000").to_string();
            return Signature { name, email, time, tz };
        }
    }
    Signature { name: text.to_string(), email: String::new(), time: 0, tz: "+0000".into() }
}

pub fn parse_commit(data: &[u8]) -> Commit {
    let text = String::from_utf8_lossy(data);
    let (header, message) = match text.find("\n\n") {
        Some(i) => (&text[..i], text[i + 2..].to_string()),
        None => (&text[..], String::new()),
    };
    let mut c = Commit {
        tree: String::new(),
        parents: Vec::new(),
        author: parse_signature(""),
        committer: parse_signature(""),
        message,
    };
    for line in header.split('\n') {
        if let Some(v) = line.strip_prefix("tree ") {
            c.tree = v.to_string();
        } else if let Some(v) = line.strip_prefix("parent ") {
            c.parents.push(v.to_string());
        } else if let Some(v) = line.strip_prefix("author ") {
            c.author = parse_signature(v);
        } else if let Some(v) = line.strip_prefix("committer ") {
            c.committer = parse_signature(v);
        }
    }
    c
}

pub fn serialize_commit(c: &Commit) -> Vec<u8> {
    let mut s = format!("tree {}\n", c.tree);
    for p in &c.parents {
        s.push_str(&format!("parent {}\n", p));
    }
    let sig = |g: &Signature| format!("{} <{}> {} {}", g.name, g.email, g.time, g.tz);
    s.push_str(&format!("author {}\ncommitter {}\n\n{}", sig(&c.author), sig(&c.committer), c.message));
    s.into_bytes()
}

/// The object an annotated tag points at.
pub fn tag_target(data: &[u8]) -> Option<String> {
    let head = &data[..data.len().min(60)];
    let s = std::str::from_utf8(head).ok()?;
    let oid = s.strip_prefix("object ")?.get(..40)?;
    if crate::util::is_oid(oid) {
        Some(oid.to_string())
    } else {
        None
    }
}

// ---------------------------------------------------------------------------------------------
// The store

#[derive(Default)]
pub struct ObjectStore {
    pub objects: HashMap<String, Obj>,
    commits: HashMap<String, Arc<Commit>>,
    trees: HashMap<String, Arc<Vec<TreeEntry>>>,
}

impl ObjectStore {
    pub fn put(&mut self, kind: Kind, data: Vec<u8>) -> String {
        let oid = hash_object(kind, &data);
        self.objects.entry(oid.clone()).or_insert_with(|| Obj { kind, data: Arc::new(data) });
        oid
    }
    pub fn put_known(&mut self, oid: String, obj: Obj) {
        self.objects.entry(oid).or_insert(obj);
    }
    pub fn get(&self, oid: &str) -> Option<&Obj> {
        self.objects.get(oid)
    }
    pub fn has(&self, oid: &str) -> bool {
        self.objects.contains_key(oid)
    }
    pub fn commit(&mut self, oid: &str) -> Option<Arc<Commit>> {
        if let Some(c) = self.commits.get(oid) {
            return Some(c.clone());
        }
        let obj = self.objects.get(oid)?;
        if obj.kind != Kind::Commit {
            return None;
        }
        let c = Arc::new(parse_commit(&obj.data));
        self.commits.insert(oid.to_string(), c.clone());
        Some(c)
    }
    pub fn tree(&mut self, oid: &str) -> Option<Arc<Vec<TreeEntry>>> {
        if let Some(t) = self.trees.get(oid) {
            return Some(t.clone());
        }
        let obj = self.objects.get(oid)?;
        if obj.kind != Kind::Tree {
            return None;
        }
        let t = Arc::new(parse_tree(&obj.data).ok()?);
        self.trees.insert(oid.to_string(), t.clone());
        Some(t)
    }

    // ---- tree operations

    /// Every blob path under a tree, path -> (mode, oid), sorted by path.
    pub fn flatten(&mut self, tree: &str) -> BTreeMap<String, (String, String)> {
        let mut out = BTreeMap::new();
        self.flatten_into(tree, "", &mut out);
        out
    }
    fn flatten_into(&mut self, tree: &str, prefix: &str, out: &mut BTreeMap<String, (String, String)>) {
        let Some(entries) = self.tree(tree) else { return };
        for e in entries.iter() {
            let path = if prefix.is_empty() { e.name.clone() } else { format!("{}/{}", prefix, e.name) };
            if is_tree_mode(&e.mode) {
                self.flatten_into(&e.oid, &path, out);
            } else {
                out.insert(path, (e.mode.clone(), e.oid.clone()));
            }
        }
    }
    pub fn flat_at(&mut self, commit: &str) -> BTreeMap<String, (String, String)> {
        match self.commit(commit) {
            Some(c) => self.flatten(&c.tree.clone()),
            None => BTreeMap::new(),
        }
    }

    /// Store the trees for a flat path map; returns the root tree oid.
    pub fn build_tree(&mut self, flat: &BTreeMap<String, (String, String)>) -> String {
        #[derive(Default)]
        struct Dir {
            files: BTreeMap<String, (String, String)>,
            dirs: BTreeMap<String, Dir>,
        }
        let mut root = Dir::default();
        for (path, entry) in flat {
            let parts: Vec<&str> = path.split('/').collect();
            let mut node = &mut root;
            for part in &parts[..parts.len() - 1] {
                node = node.dirs.entry(part.to_string()).or_default();
            }
            node.files.insert(parts[parts.len() - 1].to_string(), entry.clone());
        }
        fn write(store: &mut ObjectStore, d: &Dir) -> String {
            let mut entries = Vec::new();
            for (name, child) in &d.dirs {
                let oid = write(store, child);
                entries.push(TreeEntry { mode: "40000".into(), name: name.clone(), oid });
            }
            for (name, (mode, oid)) in &d.files {
                entries.push(TreeEntry { mode: mode.clone(), name: name.clone(), oid: oid.clone() });
            }
            store.put(Kind::Tree, serialize_tree(&entries))
        }
        write(self, &root)
    }

    /// Resolve a repository path under a commit: Some((is_tree, mode, oid)).
    pub fn lookup(&mut self, commit: &str, path: &str) -> Option<(bool, String, String)> {
        let c = self.commit(commit)?;
        let mut current = c.tree.clone();
        let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        if parts.is_empty() {
            return Some((true, "40000".into(), current));
        }
        for (i, part) in parts.iter().enumerate() {
            let entries = self.tree(&current)?;
            let found = entries.iter().find(|e| e.name == *part)?;
            if i == parts.len() - 1 {
                return Some((is_tree_mode(&found.mode), found.mode.clone(), found.oid.clone()));
            }
            if !is_tree_mode(&found.mode) {
                return None;
            }
            current = found.oid.clone();
        }
        None
    }

    /// Whether `ancestor` is reachable from `descendant` (inclusive).
    pub fn is_ancestor(&mut self, ancestor: &str, descendant: &str) -> bool {
        if ancestor == descendant {
            return true;
        }
        let mut seen = HashSet::new();
        let mut stack = vec![descendant.to_string()];
        while let Some(oid) = stack.pop() {
            if oid == ancestor {
                return true;
            }
            if !seen.insert(oid.clone()) {
                continue;
            }
            if let Some(c) = self.commit(&oid) {
                for p in &c.parents {
                    stack.push(p.clone());
                }
            }
        }
        false
    }

    /// Commits reachable from `tip`, breadth first (newest first on a linear history).
    pub fn ancestors(&mut self, tip: &str, limit: usize) -> Vec<String> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut queue = VecDeque::new();
        seen.insert(tip.to_string());
        queue.push_back(tip.to_string());
        while let Some(oid) = queue.pop_front() {
            if out.len() >= limit {
                break;
            }
            let Some(c) = self.commit(&oid) else { continue };
            out.push(oid);
            for p in &c.parents {
                if seen.insert(p.clone()) {
                    queue.push_back(p.clone());
                }
            }
        }
        out
    }

    pub fn merge_base(&mut self, a: &str, b: &str) -> Option<String> {
        let reach_b: HashSet<String> = self.ancestors(b, usize::MAX).into_iter().collect();
        self.ancestors(a, usize::MAX).into_iter().find(|o| reach_b.contains(o))
    }

    pub fn peel_to_commit(&self, oid: &str) -> Option<String> {
        let mut current = oid.to_string();
        for _ in 0..10 {
            let obj = self.objects.get(&current)?;
            match obj.kind {
                Kind::Commit => return Some(current),
                Kind::Tag => current = tag_target(&obj.data)?,
                _ => return None,
            }
        }
        None
    }
}

pub type Flat = BTreeMap<String, (String, String)>;

pub struct FileChange {
    pub path: String,
    pub status: &'static str,
    pub before: Option<(String, String)>,
    pub after: Option<(String, String)>,
}

pub fn diff_flat(before: &Flat, after: &Flat) -> Vec<FileChange> {
    let mut out = Vec::new();
    for (path, e) in after {
        match before.get(path) {
            None => out.push(FileChange { path: path.clone(), status: "added", before: None, after: Some(e.clone()) }),
            Some(old) if old != e => out.push(FileChange { path: path.clone(), status: "modified", before: Some(old.clone()), after: Some(e.clone()) }),
            _ => {}
        }
    }
    for (path, e) in before {
        if !after.contains_key(path) {
            out.push(FileChange { path: path.clone(), status: "removed", before: Some(e.clone()), after: None });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Line additions and deletions between two texts (a multiset line diff).
pub fn line_counts(before: &str, after: &str) -> (usize, usize) {
    let split = |t: &str| -> Vec<String> {
        if t.is_empty() {
            vec![]
        } else {
            t.strip_suffix('\n').unwrap_or(t).split('\n').map(|s| s.to_string()).collect()
        }
    };
    let mut counts: HashMap<String, usize> = HashMap::new();
    for l in split(before) {
        *counts.entry(l).or_default() += 1;
    }
    let mut additions = 0;
    for l in split(after) {
        match counts.get_mut(&l) {
            Some(n) if *n > 0 => *n -= 1,
            _ => additions += 1,
        }
    }
    (additions, counts.values().sum())
}

/// File-level three-way merge; None on a conflict.
pub fn merge_flat(base: &Flat, ours: &Flat, theirs: &Flat) -> Option<Flat> {
    let mut result = ours.clone();
    let mut paths: Vec<&String> = base.keys().chain(theirs.keys()).collect();
    paths.sort();
    paths.dedup();
    for path in paths {
        let (b, o, t) = (base.get(path), ours.get(path), theirs.get(path));
        if b == t {
            continue;
        }
        if b == o || o == t {
            match t {
                None => {
                    result.remove(path);
                }
                Some(v) => {
                    result.insert(path.clone(), v.clone());
                }
            }
            continue;
        }
        return None;
    }
    Some(result)
}

pub fn sha1_hex(data: &[u8]) -> String {
    hex(&sha1(data))
}
