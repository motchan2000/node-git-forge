//! GitHub REST v3 under /api/v3: routing, the store calls, and GitHub-shaped replies.

use crate::http::{Request, Response};
use crate::jobj;
use crate::json::{self, Json};
use crate::objects::{is_tree_mode, Kind, ZERO_OID};
use crate::present::*;
use crate::store::{ferr, FResult, Forge, ForgeError};
use crate::util::{b64decode, b64encode, is_b64, is_oid, percent_decode};

pub struct Ctx<'a> {
    pub req: &'a Request,
    pub base: String,
    pub path: String,
}

pub fn body_json(req: &Request) -> FResult<Json> {
    if req.body.iter().all(|b| b.is_ascii_whitespace()) {
        return Ok(Json::Obj(vec![]));
    }
    let v = json::parse_bytes(&req.body).map_err(|_| ForgeError { status: 400, message: "Problems parsing JSON".into() })?;
    if !v.is_object() {
        return ferr(400, "Body should be a JSON object");
    }
    Ok(v)
}

fn int_param(ctx: &Ctx, name: &str, fallback: i64, min: i64, max: i64) -> i64 {
    match ctx.req.param(name).and_then(|v| v.parse::<i64>().ok()) {
        Some(n) => n.clamp(min, max),
        None => fallback,
    }
}

/// Slice a list by per_page/page and answer with Link relations.
fn page(ctx: &Ctx, items: Vec<Json>) -> Response {
    let per_page = int_param(ctx, "per_page", 30, 1, 100) as usize;
    let page = int_param(ctx, "page", 1, 1, 1_000_000) as usize;
    let start = (page - 1).saturating_mul(per_page);
    let total = items.len();
    let slice: Vec<Json> = items.into_iter().skip(start).take(per_page).collect();
    let last = ((total + per_page - 1) / per_page).max(1);
    let link = |p: usize| -> String {
        let mut pairs: Vec<String> = ctx
            .req
            .query
            .split('&')
            .filter(|kv| !kv.is_empty())
            .filter(|kv| {
                let k = kv.split('=').next().unwrap_or("");
                k != "per_page" && k != "page"
            })
            .map(|s| s.to_string())
            .collect();
        pairs.push(format!("per_page={}", per_page));
        pairs.push(format!("page={}", p));
        format!("<{}{}?{}>", ctx.base, ctx.path, pairs.join("&"))
    };
    let mut rels = Vec::new();
    if page < last {
        rels.push(format!("{}; rel=\"next\"", link(page + 1)));
        rels.push(format!("{}; rel=\"last\"", link(last)));
    }
    if page > 1 {
        rels.push(format!("{}; rel=\"first\"", link(1)));
        rels.push(format!("{}; rel=\"prev\"", link(page - 1)));
    }
    let res = Response::json(200, &Json::Arr(slice));
    if rels.is_empty() {
        res
    } else {
        res.with_header("link", &rels.join(", "))
    }
}

fn not_found<T>() -> FResult<T> {
    ferr(404, "Not Found")
}

fn number_of(seg: Option<&String>) -> FResult<u64> {
    match seg.and_then(|s| s.parse::<u64>().ok()) {
        Some(n) if n >= 1 => Ok(n),
        _ => not_found(),
    }
}

pub fn handle(f: &mut Forge, req: &Request, base: &str) -> Response {
    let rest = &req.path["/api/v3".len()..];
    let segs: Vec<String> = rest.split('/').filter(|s| !s.is_empty()).map(percent_decode).collect();
    let ctx = Ctx { req, base: base.to_string(), path: req.path.clone() };
    match route(f, &ctx, &segs) {
        Ok(r) => r,
        Err(e) => Response::error(e.status, &e.message),
    }
}

fn route(f: &mut Forge, ctx: &Ctx, segs: &[String]) -> FResult<Response> {
    let m = ctx.req.method.as_str();
    let base = ctx.base.as_str();
    let s0 = segs.first().map(|s| s.as_str());
    if segs.is_empty() && m == "GET" {
        return Ok(Response::json(
            200,
            &jobj! {
                "current_user_url" => format!("{}/api/v3/user", base),
                "repository_url" => format!("{}/api/v3/repos/{{owner}}/{{repo}}", base),
                "rate_limit_url" => format!("{}/api/v3/rate_limit", base),
            },
        )
        .with_header("x-oauth-scopes", "repo, read:org, workflow")
        .with_header("x-accepted-oauth-scopes", ""));
    }
    if s0 == Some("meta") && segs.len() == 1 && m == "GET" {
        return Ok(Response::json(200, &jobj! { "verifiable_password_authentication" => false, "installed_version" => "3.14.0" }));
    }
    if s0 == Some("rate_limit") && segs.len() == 1 && m == "GET" {
        let core = jobj! { "limit" => 5000, "remaining" => 5000, "reset" => 0, "used" => 0 };
        return Ok(Response::json(200, &jobj! { "resources" => jobj! { "core" => core.clone(), "graphql" => core.clone() }, "rate" => core }));
    }
    if s0 == Some("user") && segs.len() == 1 && m == "GET" {
        let mut u = user_json(f, base, &f.viewer.login);
        u.set("name", Json::from(f.viewer.name.as_str()));
        u.set("email", Json::from(f.viewer.email.as_str()));
        return Ok(Response::json(200, &u));
    }
    if s0 == Some("user") && segs.len() == 2 && segs[1] == "repos" {
        if m == "GET" {
            let ids = f.list_repos(&f.viewer.login.clone());
            return Ok(page(ctx, ids.into_iter().map(|id| repo_json(f, base, id)).collect()));
        }
        if m == "POST" {
            let body = body_json(ctx.req)?;
            let id = f.create_repo(&body)?;
            if body.at("auto_init").as_bool() == Some(true) {
                let branch = f.repo(id).default_branch.clone();
                let readme = format!("# {}\n", f.repo(id).name).into_bytes();
                f.commit_files(id, &branch, vec![("README.md".into(), Some(readme))], "Initial commit\n".into());
            }
            return Ok(Response::json(201, &repo_json(f, base, id)));
        }
    }
    if s0 == Some("users") && segs.len() >= 2 {
        let login = &segs[1];
        if segs.len() == 2 && m == "GET" {
            if !login.eq_ignore_ascii_case(&f.viewer.login) {
                return not_found();
            }
            return Ok(Response::json(200, &user_json(f, base, &f.viewer.login)));
        }
        if segs.len() == 3 && segs[2] == "repos" && m == "GET" {
            let ids = f.list_repos(login);
            return Ok(page(ctx, ids.into_iter().map(|id| repo_json(f, base, id)).collect()));
        }
    }
    if s0 == Some("repos") && segs.len() >= 3 {
        let id = f.require_repo(&segs[1], &segs[2])?;
        return route_repo(f, ctx, id, &segs[3..]);
    }
    not_found()
}

fn route_repo(f: &mut Forge, ctx: &Ctx, id: u64, tail: &[String]) -> FResult<Response> {
    let m = ctx.req.method.as_str();
    let base = ctx.base.as_str();
    if tail.is_empty() {
        return match m {
            "GET" => Ok(Response::json(200, &repo_json(f, base, id))),
            "PATCH" => {
                let body = body_json(ctx.req)?;
                if let Some(n) = body.at("name").as_str() {
                    if n != f.repo(id).name {
                        return ferr(422, "renaming is not supported");
                    }
                }
                f.update_repo(id, &body)?;
                Ok(Response::json(200, &repo_json(f, base, id)))
            }
            "DELETE" => {
                f.delete_repo(id);
                Ok(Response::json(204, &Json::Null))
            }
            _ => not_found(),
        };
    }
    match tail[0].as_str() {
        "branches" => branches(f, ctx, id, tail),
        "commits" => commits(f, ctx, id, tail),
        "git" => git_data(f, ctx, id, tail),
        "contents" => contents(f, ctx, id, &tail[1..].join("/")),
        "readme" if m == "GET" && tail.len() == 1 => contents(f, ctx, id, "README.md"),
        "labels" => labels(f, ctx, id, tail),
        "issues" => issues(f, ctx, id, tail),
        "pulls" => pulls(f, ctx, id, tail),
        "tags" if m == "GET" && tail.len() == 1 => {
            let tags: Vec<(String, String)> = f.repo(id).refs.iter().filter(|(r, _)| r.starts_with("refs/tags/")).map(|(r, o)| (r.clone(), o.clone())).collect();
            let items = tags.into_iter().map(|(r, oid)| jobj! { "name" => &r[10..], "commit" => jobj! { "sha" => f.objects.peel_to_commit(&oid).unwrap_or(oid.clone()) } }).collect();
            Ok(page(ctx, items))
        }
        _ => not_found(),
    }
}

// ---- branches, commits, refs

fn branch_json(ctx: &Ctx, f: &Forge, id: u64, name: &str, oid: &str) -> Json {
    jobj! {
        "name" => name,
        "commit" => jobj! { "sha" => oid, "url" => format!("{}/api/v3/repos/{}/commits/{}", ctx.base, f.repo(id).full_name(), oid) },
        "protected" => false,
    }
}

fn branches(f: &mut Forge, ctx: &Ctx, id: u64, tail: &[String]) -> FResult<Response> {
    if ctx.req.method != "GET" {
        return not_found();
    }
    if tail.len() == 1 {
        let list: Vec<(String, String)> = f.repo(id).refs.iter().filter_map(|(r, o)| r.strip_prefix("refs/heads/").map(|b| (b.to_string(), o.clone()))).collect();
        let items = list.iter().map(|(b, o)| branch_json(ctx, f, id, b, o)).collect();
        return Ok(page(ctx, items));
    }
    let name = tail[1..].join("/");
    let Some(oid) = f.branch_tip(id, &name) else { return ferr(404, "Branch not found") };
    let mut b = branch_json(ctx, f, id, &name, &oid);
    b.set("commit", commit_json(f, &ctx.base, id, &oid).unwrap_or(Json::Null));
    Ok(Response::json(200, &b))
}

fn commits(f: &mut Forge, ctx: &Ctx, id: u64, tail: &[String]) -> FResult<Response> {
    if ctx.req.method != "GET" {
        return not_found();
    }
    if tail.len() == 1 {
        let sha = ctx.req.param("sha");
        if f.repo(id).refs.is_empty() {
            return ferr(409, "Git Repository is empty.");
        }
        let Some(tip) = f.resolve(id, sha.as_deref()) else { return ferr(404, format!("No commit found for SHA: {}", sha.unwrap_or_default())) };
        let per_page = int_param(ctx, "per_page", 30, 1, 100) as usize;
        let pg = int_param(ctx, "page", 1, 1, 1_000_000) as usize;
        let mut list = f.objects.ancestors(&tip, per_page * pg + 1);
        if let Some(path) = ctx.req.param("path").filter(|p| !p.is_empty()) {
            let all = f.objects.ancestors(&tip, usize::MAX);
            list = all
                .into_iter()
                .filter(|oid| {
                    let here = f.objects.lookup(oid, &path).map(|x| x.2);
                    let parent = f.objects.commit(oid).and_then(|c| c.parents.first().cloned());
                    let before = parent.and_then(|p| f.objects.lookup(&p, &path)).map(|x| x.2);
                    here != before
                })
                .collect();
        }
        let items = list.iter().filter_map(|oid| commit_json(f, &ctx.base, id, oid)).collect();
        return Ok(page(ctx, items));
    }
    let r = tail[1..].join("/");
    let Some(oid) = f.resolve(id, Some(&r)) else { return ferr(404, format!("No commit found for SHA: {}", r)) };
    let mut body = commit_json(f, &ctx.base, id, &oid).unwrap_or(Json::Null);
    if let Some(c) = f.objects.commit(&oid) {
        let before = match c.parents.first() {
            Some(p) => f.objects.flat_at(&p.clone()),
            None => Default::default(),
        };
        let after = f.objects.flat_at(&oid);
        let files: Vec<Json> = crate::objects::diff_flat(&before, &after)
            .into_iter()
            .map(|ch| jobj! { "filename" => ch.path, "status" => ch.status, "sha" => ch.after.or(ch.before).map(|e| e.1).unwrap_or_default() })
            .collect();
        body.set("files", Json::Arr(files));
    }
    Ok(Response::json(200, &body))
}

fn ref_json(ctx: &Ctx, f: &Forge, id: u64, r: &str, oid: &str) -> Json {
    let kind = f.objects.get(oid).map(|o| o.kind.name()).unwrap_or("commit");
    let full = f.repo(id).full_name();
    jobj! {
        "ref" => r,
        "node_id" => b64encode(format!("03:Ref{}:{}", id, r).as_bytes()),
        "url" => format!("{}/api/v3/repos/{}/git/{}", ctx.base, full, r),
        "object" => jobj! { "sha" => oid, "type" => kind, "url" => format!("{}/api/v3/repos/{}/git/{}s/{}", ctx.base, full, kind, oid) },
    }
}

fn git_data(f: &mut Forge, ctx: &Ctx, id: u64, tail: &[String]) -> FResult<Response> {
    let m = ctx.req.method.as_str();
    let kind = tail.get(1).map(|s| s.as_str()).unwrap_or("");
    if kind == "refs" || kind == "ref" {
        if m == "POST" && tail.len() == 2 && kind == "refs" {
            let body = body_json(ctx.req)?;
            let r = body.at("ref").as_str().unwrap_or("").to_string();
            let sha = body.at("sha").as_str().unwrap_or("").to_lowercase();
            if !r.starts_with("refs/") || r.split('/').count() < 3 || r.contains(char::is_whitespace) {
                return ferr(422, "Reference name is invalid");
            }
            if !is_oid(&sha) || !f.objects.has(&sha) {
                return ferr(422, "Object does not exist");
            }
            if f.repo(id).refs.contains_key(&r) {
                return ferr(422, "Reference already exists");
            }
            f.set_ref(id, &r, Some(&sha));
            return Ok(Response::json(201, &ref_json(ctx, f, id, &r, &sha)));
        }
        let name = tail[2..].join("/");
        let full = format!("refs/{}", name);
        match m {
            "GET" => {
                if name.is_empty() || name == "heads" || name == "tags" {
                    let prefix = if name.is_empty() { "refs/".to_string() } else { format!("{}/", full) };
                    let list: Vec<(String, String)> = f.repo(id).refs.iter().filter(|(r, _)| r.starts_with(&prefix)).map(|(a, b)| (a.clone(), b.clone())).collect();
                    return Ok(Response::json(200, &Json::Arr(list.iter().map(|(r, o)| ref_json(ctx, f, id, r, o)).collect())));
                }
                match f.repo(id).refs.get(&full).cloned() {
                    Some(oid) => return Ok(Response::json(200, &ref_json(ctx, f, id, &full, &oid))),
                    None => {
                        let prefix = format!("{}/", full);
                        let list: Vec<(String, String)> = f.repo(id).refs.iter().filter(|(r, _)| r.starts_with(&prefix)).map(|(a, b)| (a.clone(), b.clone())).collect();
                        if kind == "refs" && !list.is_empty() {
                            return Ok(Response::json(200, &Json::Arr(list.iter().map(|(r, o)| ref_json(ctx, f, id, r, o)).collect())));
                        }
                        return not_found();
                    }
                }
            }
            "PATCH" => {
                let body = body_json(ctx.req)?;
                let Some(old) = f.repo(id).refs.get(&full).cloned() else { return ferr(422, "Reference does not exist") };
                let sha = body.at("sha").as_str().unwrap_or("").to_lowercase();
                if !is_oid(&sha) || !f.objects.has(&sha) {
                    return ferr(422, "Object does not exist");
                }
                if body.at("force").as_bool() != Some(true) && full.starts_with("refs/heads/") && !f.objects.is_ancestor(&old, &sha) {
                    return ferr(422, "Update is not a fast forward");
                }
                f.set_ref(id, &full, Some(&sha));
                return Ok(Response::json(200, &ref_json(ctx, f, id, &full, &sha)));
            }
            "DELETE" => {
                if !f.repo(id).refs.contains_key(&full) {
                    return ferr(422, "Reference does not exist");
                }
                f.set_ref(id, &full, None);
                return Ok(Response::json(204, &Json::Null));
            }
            _ => return not_found(),
        }
    }
    if m == "GET" && tail.len() == 3 && is_oid(&tail[2].to_lowercase()) {
        let oid = tail[2].to_lowercase();
        let k = f.objects.get(&oid).map(|o| o.kind);
        match (kind, k) {
            ("commits", Some(Kind::Commit)) => {
                let c = commit_json(f, &ctx.base, id, &oid).unwrap_or(Json::Null);
                let mut out = jobj! { "sha" => oid.as_str() };
                if let Json::Obj(fields) = c.at("commit").clone() {
                    for (k, v) in fields {
                        out.set(&k, v);
                    }
                }
                out.set("parents", c.at("parents").clone());
                return Ok(Response::json(200, &out));
            }
            ("trees", Some(Kind::Tree)) => {
                let entries = f.objects.tree(&oid).unwrap_or_default();
                let tree: Vec<Json> = entries
                    .iter()
                    .map(|e| {
                        let t = if is_tree_mode(&e.mode) { "tree" } else if e.mode == "160000" { "commit" } else { "blob" };
                        jobj! { "path" => e.name.as_str(), "mode" => if is_tree_mode(&e.mode) { "040000".to_string() } else { e.mode.clone() }, "type" => t, "sha" => e.oid.as_str() }
                    })
                    .collect();
                return Ok(Response::json(200, &jobj! { "sha" => oid.as_str(), "tree" => tree, "truncated" => false }));
            }
            ("blobs", Some(Kind::Blob)) => {
                let data = f.objects.get(&oid).unwrap().data.clone();
                return Ok(Response::json(200, &jobj! { "sha" => oid.as_str(), "size" => data.len(), "encoding" => "base64", "content" => b64encode(&data) }));
            }
            _ => {}
        }
    }
    not_found()
}

// ---- contents

fn content_entry(ctx: &Ctx, f: &Forge, id: u64, path: &str, kind: &str, oid: &str, size: usize) -> Json {
    let r = f.repo(id);
    let api = format!("{}/api/v3/repos/{}", ctx.base, r.full_name());
    let name = path.rsplit('/').next().unwrap_or(path);
    jobj! {
        "type" => kind,
        "name" => name,
        "path" => path,
        "sha" => oid,
        "size" => size,
        "url" => format!("{}/contents/{}", api, path),
        "html_url" => format!("{}/{}/blob/{}/{}", ctx.base, r.full_name(), r.default_branch, path),
        "git_url" => format!("{}/git/{}/{}", api, if kind == "dir" { "trees" } else { "blobs" }, oid),
        "download_url" => if kind == "file" { Json::from(format!("{}/{}/raw/{}/{}", ctx.base, r.full_name(), r.default_branch, path)) } else { Json::Null },
    }
}

fn wrap60(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + s.len() / 60 + 1);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && i % 60 == 0 {
            out.push('\n');
        }
        out.push(c);
    }
    out
}

fn contents(f: &mut Forge, ctx: &Ctx, id: u64, raw: &str) -> FResult<Response> {
    let m = ctx.req.method.as_str();
    let path: String = raw.split('/').filter(|p| !p.is_empty() && *p != ".").collect::<Vec<_>>().join("/");
    if path.split('/').any(|p| p == "..") {
        return ferr(422, "path is invalid");
    }
    if m == "GET" {
        if f.repo(id).refs.is_empty() {
            return ferr(404, "This repository is empty.");
        }
        let r = ctx.req.param("ref");
        let Some(tip) = f.resolve(id, r.as_deref()) else { return ferr(404, format!("No commit found for the ref {}", r.unwrap_or_default())) };
        let Some((is_tree, _mode, oid)) = f.objects.lookup(&tip, &path) else { return not_found() };
        if is_tree {
            let entries = f.objects.tree(&oid).unwrap_or_default();
            let mut list: Vec<(String, Json)> = Vec::new();
            for e in entries.iter() {
                let child = if path.is_empty() { e.name.clone() } else { format!("{}/{}", path, e.name) };
                let j = if is_tree_mode(&e.mode) {
                    content_entry(ctx, f, id, &child, "dir", &e.oid, 0)
                } else if e.mode == "160000" {
                    content_entry(ctx, f, id, &child, "submodule", &e.oid, 0)
                } else {
                    let size = f.objects.get(&e.oid).map(|o| o.data.len()).unwrap_or(0);
                    content_entry(ctx, f, id, &child, if e.mode == "120000" { "symlink" } else { "file" }, &e.oid, size)
                };
                list.push((e.name.clone(), j));
            }
            list.sort_by(|a, b| a.0.cmp(&b.0));
            return Ok(Response::json(200, &Json::Arr(list.into_iter().map(|x| x.1).collect())));
        }
        let data = f.objects.get(&oid).map(|o| o.data.clone()).unwrap_or_default();
        let mut e = content_entry(ctx, f, id, &path, "file", &oid, data.len());
        e.set("encoding", Json::from("base64"));
        e.set("content", Json::from(wrap60(&b64encode(&data))));
        return Ok(Response::json(200, &e));
    }
    if m == "PUT" || m == "DELETE" {
        if path.is_empty() {
            return ferr(422, "path is required");
        }
        let body = body_json(ctx.req)?;
        let branch = match body.at("branch").as_str() {
            Some(b) if !b.is_empty() => b.strip_prefix("refs/heads/").unwrap_or(b).to_string(),
            _ => f.head_branch(id),
        };
        let tip = f.branch_tip(id, &branch);
        if tip.is_none() && !f.repo(id).refs.is_empty() && body.at("branch").as_str().is_some() {
            return ferr(404, format!("Branch {} not found", branch));
        }
        let existing = tip.as_ref().and_then(|t| f.objects.lookup(t, &path));
        if let Some((true, _, _)) = existing {
            return ferr(422, "path is a directory");
        }
        let message = match body.at("message").as_str() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => format!("{} {}", if m == "PUT" { "Update" } else { "Delete" }, path),
        };
        let message = if message.ends_with('\n') { message } else { format!("{}\n", message) };
        let sha = body.at("sha").as_str().map(|s| s.to_lowercase());
        if m == "PUT" {
            let Some(content) = body.at("content").as_str() else { return ferr(422, "Invalid request.\n\n\"content\" wasn't supplied.") };
            if !is_b64(content) {
                return ferr(422, "content is not valid Base64");
            }
            if let Some((_, _, cur)) = &existing {
                match &sha {
                    None => return ferr(422, "Invalid request.\n\n\"sha\" wasn't supplied."),
                    Some(s) if s != cur => return ferr(409, format!("{} does not match {}", path, s)),
                    _ => {}
                }
            }
            let data = b64decode(content).unwrap_or_default();
            let size = data.len();
            let commit = f.commit_files(id, &branch, vec![(path.clone(), Some(data))], message);
            let blob = f.objects.lookup(&commit, &path).map(|x| x.2).unwrap_or_else(|| ZERO_OID.into());
            let mut entry = content_entry(ctx, f, id, &path, "file", &blob, size);
            entry.set("encoding", Json::from("base64"));
            let c = commit_json(f, &ctx.base, id, &commit).unwrap_or(Json::Null);
            let mut commit_out = jobj! { "sha" => commit.as_str() };
            if let Json::Obj(fields) = c.at("commit").clone() {
                for (k, v) in fields {
                    commit_out.set(&k, v);
                }
            }
            commit_out.set("parents", c.at("parents").clone());
            return Ok(Response::json(if existing.is_none() { 201 } else { 200 }, &jobj! { "content" => entry, "commit" => commit_out }));
        }
        let Some((_, _, cur)) = existing else { return not_found() };
        match &sha {
            None => return ferr(422, "Invalid request.\n\n\"sha\" wasn't supplied."),
            Some(s) if *s != cur => return ferr(409, format!("{} does not match {}", path, s)),
            _ => {}
        }
        let commit = f.commit_files(id, &branch, vec![(path.clone(), None)], message);
        let c = commit_json(f, &ctx.base, id, &commit).unwrap_or(Json::Null);
        let mut commit_out = jobj! { "sha" => commit.as_str() };
        if let Json::Obj(fields) = c.at("commit").clone() {
            for (k, v) in fields {
                commit_out.set(&k, v);
            }
        }
        commit_out.set("parents", c.at("parents").clone());
        return Ok(Response::json(200, &jobj! { "content" => Json::Null, "commit" => commit_out }));
    }
    not_found()
}

// ---- labels, issues, comments

fn labels(f: &mut Forge, ctx: &Ctx, id: u64, tail: &[String]) -> FResult<Response> {
    let m = ctx.req.method.as_str();
    let (owner, name) = (f.repo(id).owner.clone(), f.repo(id).name.clone());
    if tail.len() == 1 {
        if m == "GET" {
            let items = f.repo(id).labels.iter().map(|l| label_json(&ctx.base, &owner, &name, l)).collect();
            return Ok(page(ctx, items));
        }
        if m == "POST" {
            let body = body_json(ctx.req)?;
            let l = f.create_label(id, &body)?;
            return Ok(Response::json(201, &label_json(&ctx.base, &owner, &name, &l)));
        }
    }
    if tail.len() == 2 {
        let Some(l) = f.repo(id).label(&tail[1]).cloned() else { return not_found() };
        if m == "GET" {
            return Ok(Response::json(200, &label_json(&ctx.base, &owner, &name, &l)));
        }
        if m == "DELETE" {
            let lower = l.name.to_lowercase();
            let repo = f.repo_mut(id);
            repo.labels.retain(|x| x.name.to_lowercase() != lower);
            for i in repo.issues.values_mut() {
                i.labels.retain(|x| x.to_lowercase() != lower);
                i.version += 1;
            }
            return Ok(Response::json(204, &Json::Null));
        }
    }
    not_found()
}

/// A single issue's JSON text, reused until the issue changes.
fn issue_text(f: &mut Forge, base: &str, id: u64, number: u64) -> String {
    let version = f.repo(id).issues[&number].version;
    if let Some((v, b, text)) = f.issue_cache.get(&(id, number)) {
        if *v == version && b == base {
            return text.clone();
        }
    }
    let text = issue_json(f, base, id, number).to_string();
    f.issue_cache.insert((id, number), (version, base.to_string(), text.clone()));
    text
}

fn issues(f: &mut Forge, ctx: &Ctx, id: u64, tail: &[String]) -> FResult<Response> {
    let m = ctx.req.method.as_str();
    let base = ctx.base.clone();
    if tail.len() == 1 {
        if m == "GET" {
            let asc = ctx.req.param("direction").as_deref() == Some("asc");
            let labels: Vec<String> = ctx.req.param("labels").filter(|s| !s.is_empty()).map(|s| s.split(',').map(|x| x.to_string()).collect()).unwrap_or_default();
            let state = ctx.req.param("state").unwrap_or_else(|| "open".into());
            let mut list = f.list_issues(id, &state, asc, &labels);
            if ctx.req.param("sort").as_deref() == Some("updated") {
                let repo = f.repo(id);
                list.sort_by(|a, b| {
                    let (ia, ib) = (&repo.issues[a], &repo.issues[b]);
                    let ord = ia.updated_at.cmp(&ib.updated_at).then(a.cmp(b));
                    if asc {
                        ord
                    } else {
                        ord.reverse()
                    }
                });
            }
            let items = list.iter().map(|n| issue_json(f, &base, id, *n)).collect();
            return Ok(page(ctx, items));
        }
        if m == "POST" {
            let body = body_json(ctx.req)?;
            let n = f.create_issue(id, body.at("title"), body.at("body"), body.at("labels"))?;
            let text = issue_text(f, &base, id, n);
            return Ok(Response::json_text(201, text));
        }
    }
    if tail.len() == 2 && tail[1] == "comments" && m == "GET" {
        let mut all: Vec<_> = f.repo(id).comments.values().flatten().cloned().collect();
        all.sort_by_key(|c| c.id);
        let items = all.iter().map(|c| comment_json(f, &base, id, c)).collect();
        return Ok(page(ctx, items));
    }
    if tail.len() >= 2 {
        let number = number_of(tail.get(1))?;
        if tail.len() == 2 {
            if m == "GET" && f.repo(id).pulls.contains_key(&number) {
                let p = f.repo(id).pulls[&number].clone();
                let r = f.repo(id);
                let api = format!("{}/api/v3/repos/{}", base, r.full_name());
                let view = jobj! {
                    "id" => p.id, "number" => p.number, "title" => p.title.as_str(), "body" => p.body.as_str(),
                    "state" => if p.open { "open" } else { "closed" }, "labels" => Json::Arr(vec![]),
                    "user" => user_json(f, &base, &p.user), "comments" => r.comment_count(number),
                    "created_at" => p.created_at.as_str(), "updated_at" => p.updated_at.as_str(), "closed_at" => p.closed_at.clone(),
                    "pull_request" => jobj! { "url" => format!("{}/pulls/{}", api, number) },
                };
                return Ok(Response::json(200, &view));
            }
            f.require_issue(id, number)?;
            if m == "GET" {
                return Ok(Response::json_text(200, issue_text(f, &base, id, number)));
            }
            if m == "PATCH" {
                let body = body_json(ctx.req)?;
                f.update_issue(id, number, &body)?;
                return Ok(Response::json_text(200, issue_text(f, &base, id, number)));
            }
        }
        if tail.len() == 3 && tail[2] == "comments" {
            if m == "GET" {
                let list = f.list_comments(id, number)?;
                let items = list.iter().map(|c| comment_json(f, &base, id, c)).collect();
                return Ok(page(ctx, items));
            }
            if m == "POST" {
                let body = body_json(ctx.req)?;
                let c = f.add_comment(id, number, body.at("body"))?;
                return Ok(Response::json(201, &comment_json(f, &base, id, &c)));
            }
        }
        if tail.len() == 3 && tail[2] == "labels" {
            f.require_issue(id, number)?;
            let (owner, name) = (f.repo(id).owner.clone(), f.repo(id).name.clone());
            let render = |f: &Forge| -> Json {
                let r = f.repo(id);
                Json::Arr(r.issues[&number].labels.iter().filter_map(|n| r.label(n)).map(|l| label_json(&base, &owner, &name, l)).collect())
            };
            if m == "GET" {
                return Ok(Response::json(200, &render(f)));
            }
            if m == "POST" || m == "PUT" {
                let body = if ctx.req.body.first() == Some(&b'[') {
                    json::parse_bytes(&ctx.req.body).map_err(|_| ForgeError { status: 400, message: "Problems parsing JSON".into() })?
                } else {
                    body_json(ctx.req)?.at("labels").clone()
                };
                let add = f.resolve_labels(id, &body)?;
                let issue = f.repo_mut(id).issues.get_mut(&number).unwrap();
                if m == "PUT" {
                    issue.labels = add;
                } else {
                    for l in add {
                        if !issue.labels.contains(&l) {
                            issue.labels.push(l);
                        }
                    }
                }
                issue.version += 1;
                return Ok(Response::json(200, &render(f)));
            }
        }
    }
    not_found()
}

// ---- pulls

fn pulls(f: &mut Forge, ctx: &Ctx, id: u64, tail: &[String]) -> FResult<Response> {
    let m = ctx.req.method.as_str();
    let base = ctx.base.clone();
    if tail.len() == 1 {
        if m == "GET" {
            let asc = ctx.req.param("direction").as_deref() == Some("asc");
            let state = ctx.req.param("state").unwrap_or_else(|| "open".into());
            let mut list = f.list_pulls(id, &state, asc);
            if let Some(h) = ctx.req.param("head").filter(|s| !s.is_empty()) {
                let h = h.rsplit_once(':').map(|x| x.1.to_string()).unwrap_or(h);
                list.retain(|n| f.repo(id).pulls[n].head == h);
            }
            if let Some(b) = ctx.req.param("base").filter(|s| !s.is_empty()) {
                list.retain(|n| f.repo(id).pulls[n].base == b);
            }
            let items = list.iter().map(|n| pull_json(f, &base, id, *n)).collect();
            return Ok(page(ctx, items));
        }
        if m == "POST" {
            let body = body_json(ctx.req)?;
            let n = f.create_pull(id, body.at("title"), body.at("body"), body.at("head"), body.at("base"))?;
            return Ok(Response::json(201, &pull_json(f, &base, id, n)));
        }
    }
    if tail.len() >= 2 {
        let number = number_of(tail.get(1))?;
        f.require_pull(id, number)?;
        if tail.len() == 2 {
            if m == "GET" {
                return Ok(Response::json(200, &pull_json(f, &base, id, number)));
            }
            if m == "PATCH" {
                let body = body_json(ctx.req)?;
                f.update_pull(id, number, &body)?;
                return Ok(Response::json(200, &pull_json(f, &base, id, number)));
            }
        }
        if tail.len() == 3 && tail[2] == "files" && m == "GET" {
            let full = f.repo(id).full_name();
            let files = f.pull_files(id, number);
            let items = files
                .into_iter()
                .map(|x| {
                    jobj! {
                        "sha" => x.sha.as_str(),
                        "filename" => x.path.as_str(),
                        "status" => x.status,
                        "additions" => x.additions,
                        "deletions" => x.deletions,
                        "changes" => x.additions + x.deletions,
                        "blob_url" => format!("{}/{}/blob/{}/{}", base, full, x.sha, x.path),
                        "raw_url" => format!("{}/{}/raw/{}/{}", base, full, x.sha, x.path),
                        "contents_url" => format!("{}/api/v3/repos/{}/contents/{}", base, full, x.path),
                        "patch" => "",
                    }
                })
                .collect();
            return Ok(page(ctx, items));
        }
        if tail.len() == 3 && tail[2] == "commits" && m == "GET" {
            let (head, b) = f.pull_shas(id, number);
            let base_set: std::collections::HashSet<String> = f.objects.ancestors(&b, usize::MAX).into_iter().collect();
            let mut list: Vec<String> = f.objects.ancestors(&head, usize::MAX).into_iter().filter(|o| !base_set.contains(o)).collect();
            list.reverse();
            let items = list.iter().filter_map(|o| commit_json(f, &base, id, o)).collect();
            return Ok(page(ctx, items));
        }
        if tail.len() == 3 && tail[2] == "merge" {
            if m == "GET" {
                return if f.repo(id).pulls[&number].merged { Ok(Response::json(204, &Json::Null)) } else { not_found() };
            }
            if m == "PUT" {
                let body = body_json(ctx.req)?;
                let method = body.at("merge_method");
                if !method.is_null() && !matches!(method.as_str(), Some("squash") | Some("merge") | Some("rebase")) {
                    return ferr(422, "merge_method is invalid");
                }
                let sha = f.merge_pull(id, number, body.at("commit_title").as_str(), body.at("commit_message").as_str(), body.at("sha").as_str())?;
                return Ok(Response::json(200, &jobj! { "sha" => sha, "merged" => true, "message" => "Pull Request successfully merged" }));
            }
        }
    }
    not_found()
}
