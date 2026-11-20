//! forge: a standalone GitHub-compatible git forge on the Rust standard library alone.
//! git smart HTTP, REST v3, GraphQL v4 and MCP over one in-memory store, behind one HTTP server.
//!
//! main.ts starts this binary and registers the port it prints (`FORGE_PORT <n>`) with the env.

mod gql_parse;
mod graphql;
mod http;
mod json;
mod mcp;
mod objects;
mod pack;
mod present;
mod smart;
mod rest;
mod store;
mod util;
mod zlib;

use http::{Request, Response};
use std::io::Write;
use std::net::TcpListener;
use std::sync::{Arc, RwLock};
use store::{Forge, User};

pub struct App {
    pub forge: RwLock<Forge>,
    token: Option<String>,
}

impl App {
    /// The credential a request carries (Bearer, token, or a Basic password).
    fn credential(req: &Request) -> Option<String> {
        let h = req.header("authorization")?.trim();
        let lower = h.to_ascii_lowercase();
        if lower.starts_with("bearer ") || lower.starts_with("token ") {
            let v = h.split_once(' ').map(|x| x.1.trim().to_string())?;
            return if v.is_empty() { None } else { Some(v) };
        }
        if lower.starts_with("basic ") {
            let decoded = util::b64decode(h[6..].trim())?;
            let text = String::from_utf8(decoded).ok()?;
            let (user, pass) = text.split_once(':').unwrap_or((&text, ""));
            let v = if pass.is_empty() { user } else { pass };
            return if v.is_empty() { None } else { Some(v.to_string()) };
        }
        None
    }

    pub fn authorized(&self, req: &Request) -> bool {
        match App::credential(req) {
            None => false,
            Some(c) => match &self.token {
                None => true,
                Some(t) => *t == c,
            },
        }
    }

    pub fn origin(req: &Request) -> String {
        format!("http://{}", req.header("host").filter(|h| !h.is_empty()).unwrap_or("localhost"))
    }

    /// GET /api/v3/repos/{o}/{r}/issues/{n} served from the issue's cached JSON under a shared lock,
    /// when that cache is current; None sends the request down the ordinary (write-locked) path.
    fn cached_issue(&self, path: &str, base: &str) -> Option<String> {
        let segs: Vec<&str> = path.split('/').collect();
        // ["", "api", "v3", "repos", owner, repo, "issues", n]
        if segs.len() != 8 || segs[3] != "repos" || segs[6] != "issues" {
            return None;
        }
        let number: u64 = segs[7].parse().ok()?;
        let f = self.forge.read().unwrap_or_else(|e| e.into_inner());
        let id = f.repo_id(&util::percent_decode(segs[4]), &util::percent_decode(segs[5]))?;
        let issue = f.repo(id).issues.get(&number)?;
        let (version, cached_base, text) = f.issue_cache.get(&(id, number))?;
        if *version == issue.version && cached_base == base {
            Some(text.clone())
        } else {
            None
        }
    }

    pub fn handle(&self, req: &Request) -> Response {
        let path = req.path.as_str();
        let m = req.method.as_str();
        if path == "/healthz" {
            return if m == "GET" || m == "HEAD" { Response::json(200, &jobj! { "ok" => true }) } else { Response::error(404, "Not Found") };
        }
        if path == "/api/v3" || path.starts_with("/api/v3/") {
            if !self.authorized(req) {
                return Response::error(401, if App::credential(req).is_none() { "Requires authentication" } else { "Bad credentials" });
            }
            let base = App::origin(req);
            if m == "GET" {
                if let Some(text) = self.cached_issue(path, &base) {
                    return Response::json_text(200, text);
                }
            }
            let mut f = self.forge.write().unwrap_or_else(|e| e.into_inner());
            return rest::handle(&mut f, req, &base);
        }
        if path == "/api/graphql" || path == "/graphql" {
            if !self.authorized(req) {
                return Response::error(401, "This endpoint requires you to be authenticated.");
            }
            if m != "POST" {
                return Response::error(404, "Not Found");
            }
            let mut f = self.forge.write().unwrap_or_else(|e| e.into_inner());
            return graphql::handle(&mut f, &req.body, &App::origin(req));
        }
        if path == "/mcp" || path == "/mcp/" {
            return mcp::handle(&self.forge, m, &req.body, &App::origin(req), self.authorized(req));
        }
        if path.contains(".git/") {
            if let Some((owner, name, action)) = smart::route(path) {
                return smart::handle(&self.forge, req, &owner, &name, action, self.authorized(req));
            }
        }
        Response::error(404, "Not Found")
    }
}

fn main() {
    let login = std::env::var("FORGE_LOGIN").unwrap_or_else(|_| "adaiversen".into());
    let name = std::env::var("FORGE_NAME").unwrap_or_else(|_| if login == "adaiversen" { "ada iversen".into() } else { login.clone() });
    let viewer = User { id: 1000, email: format!("{}@example.invalid", login), login, name };
    let token = std::env::var("MAKEFUN_SHOP_TOKEN").ok().filter(|t| !t.is_empty());
    let app = Arc::new(App { forge: RwLock::new(Forge::new(viewer)), token });

    let port: u16 = std::env::args().nth(1).and_then(|p| p.parse().ok()).unwrap_or(0);
    let listener = TcpListener::bind(("::", port)).or_else(|_| TcpListener::bind(("0.0.0.0", port))).expect("bind");
    let bound = listener.local_addr().expect("local addr").port();
    {
        let mut out = std::io::stdout();
        let _ = writeln!(out, "FORGE_PORT {}", bound);
        let _ = out.flush();
    }
    let handler: http::Handler = Arc::new(move |req: &Request| app.handle(req));
    http::serve(listener, handler);
}
