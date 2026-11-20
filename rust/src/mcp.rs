//! Model Context Protocol at POST /mcp: JSON-RPC 2.0 over Streamable HTTP, answering application/json.
//! Speaks revision 2026-07-28 (server/discover) and the handshake revisions (initialize).

use crate::http::Response;
use crate::jobj;
use crate::json::{self, Json};
use crate::present::*;
use crate::store::{Forge, ForgeError};
use std::sync::OnceLock;

const SUPPORTED: [&str; 5] = ["2026-07-28", "2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const INSTRUCTIONS: &str = "A GitHub-compatible forge: repositories, issues and pull requests.";

fn server_info() -> Json {
    jobj! { "name" => "forge-mcp", "title" => "Forge MCP", "version" => "1.0.0" }
}
fn capabilities() -> Json {
    jobj! { "tools" => jobj! { "listChanged" => false } }
}

struct Tool {
    name: &'static str,
    title: &'static str,
    description: &'static str,
    read_only: bool,
    /// (property, JSON type, description, required)
    props: &'static [(&'static str, &'static str, &'static str, bool)],
}

const OWNER: (&str, &str, &str, bool) = ("owner", "string", "Repository owner (the account login)", true);
const REPO: (&str, &str, &str, bool) = ("repo", "string", "Repository name", true);

const TOOLS: &[Tool] = &[
    Tool { name: "create_repo", title: "Create repository", description: "Create a new repository owned by the authenticated user. Returns the repository object.", read_only: false, props: &[("name", "string", "Repository name", true), ("description", "string", "Repository description", false), ("private", "boolean", "Whether the repository is private", false)] },
    Tool { name: "get_repo", title: "Get repository", description: "Get a repository by owner and name. Returns the repository object.", read_only: true, props: &[OWNER, REPO] },
    Tool { name: "list_repos", title: "List repositories", description: "List the repositories the authenticated user owns.", read_only: true, props: &[] },
    Tool { name: "create_issue", title: "Create issue", description: "Open a new issue in a repository. Labels must already exist in the repository.", read_only: false, props: &[OWNER, REPO, ("title", "string", "Issue title", true), ("body", "string", "Issue body (markdown)", false), ("labels", "array", "Existing label names", false)] },
    Tool { name: "get_issue", title: "Get issue", description: "Get one issue of a repository by its number.", read_only: true, props: &[OWNER, REPO, ("issue_number", "integer", "Issue number", true)] },
    Tool { name: "list_issues", title: "List issues", description: "List the issues of a repository, newest first. state is open (default), closed or all.", read_only: true, props: &[OWNER, REPO, ("state", "state", "Issue state filter", false)] },
    Tool { name: "update_issue", title: "Update issue", description: "Update an issue: its title, body or state (open or closed).", read_only: false, props: &[OWNER, REPO, ("issue_number", "integer", "Issue number", true), ("title", "string", "New title", false), ("body", "string", "New body", false), ("state", "state2", "New state", false)] },
    Tool { name: "add_issue_comment", title: "Comment on issue", description: "Add a comment to an issue or pull request.", read_only: false, props: &[OWNER, REPO, ("issue_number", "integer", "Issue or pull request number", true), ("body", "string", "Comment body", true)] },
    Tool { name: "create_pull_request", title: "Create pull request", description: "Open a pull request from a head branch into a base branch of the same repository.", read_only: false, props: &[OWNER, REPO, ("title", "string", "Pull request title", true), ("body", "string", "Pull request description", false), ("head", "string", "Branch with the changes", true), ("base", "string", "Branch to merge into", true)] },
    Tool { name: "get_pull_request", title: "Get pull request", description: "Get one pull request by its number.", read_only: true, props: &[OWNER, REPO, ("pull_number", "integer", "Pull request number", true)] },
    Tool { name: "list_pull_requests", title: "List pull requests", description: "List the pull requests of a repository, newest first. state is open (default), closed or all.", read_only: true, props: &[OWNER, REPO, ("state", "state", "Pull request state filter", false)] },
    Tool { name: "merge_pull_request", title: "Merge pull request", description: "Squash-merge an open pull request into its base branch. Fails when the pull request conflicts.", read_only: false, props: &[OWNER, REPO, ("pull_number", "integer", "Pull request number", true), ("commit_title", "string", "Title of the squash commit", false), ("commit_message", "string", "Extra detail for the squash commit", false), ("merge_method", "method", "Merge method (squash is used)", false)] },
    Tool { name: "get_file_contents", title: "Get file contents", description: "Read a file (UTF-8 text) from a repository at a branch, tag or commit (default branch when ref is omitted).", read_only: true, props: &[OWNER, REPO, ("path", "string", "File path in the repository", true), ("ref", "string", "Branch, tag or commit sha", false)] },
];

fn prop_schema(ty: &str, description: &str) -> Json {
    match ty {
        "integer" => jobj! { "type" => "integer", "minimum" => 1, "description" => description },
        "array" => jobj! { "type" => "array", "items" => jobj! { "type" => "string" }, "description" => description },
        "state" => jobj! { "type" => "string", "enum" => Json::Arr(vec!["open".into(), "closed".into(), "all".into()]), "description" => description },
        "state2" => jobj! { "type" => "string", "enum" => Json::Arr(vec!["open".into(), "closed".into()]), "description" => description },
        "method" => jobj! { "type" => "string", "enum" => Json::Arr(vec!["squash".into(), "merge".into(), "rebase".into()]), "description" => description },
        other => jobj! { "type" => other, "description" => description },
    }
}

/// The tools/list result text, built once so every answer is byte-identical.
fn tools_list() -> &'static Json {
    static T: OnceLock<Json> = OnceLock::new();
    T.get_or_init(|| {
        let tools: Vec<Json> = TOOLS
            .iter()
            .map(|t| {
                let props = Json::Obj(t.props.iter().map(|(n, ty, d, _)| (n.to_string(), prop_schema(ty, d))).collect());
                let required: Vec<Json> = t.props.iter().filter(|p| p.3).map(|p| Json::from(p.0)).collect();
                let mut schema = jobj! { "type" => "object", "properties" => props };
                if !required.is_empty() {
                    schema.set("required", Json::Arr(required));
                }
                schema.set("additionalProperties", Json::Bool(false));
                jobj! {
                    "name" => t.name,
                    "title" => t.title,
                    "description" => t.description,
                    "inputSchema" => schema,
                    "annotations" => jobj! { "title" => t.title, "readOnlyHint" => t.read_only },
                }
            })
            .collect();
        jobj! { "tools" => Json::Arr(tools) }
    })
}

struct RpcError(i64, String);

enum ToolFail {
    Msg(String),
}

impl From<ForgeError> for ToolFail {
    fn from(e: ForgeError) -> ToolFail {
        ToolFail::Msg(e.message)
    }
}

fn req_str<'a>(a: &'a Json, k: &str) -> Result<&'a str, ToolFail> {
    match a.at(k).as_str() {
        Some(s) if !s.is_empty() => Ok(s),
        _ => Err(ToolFail::Msg(format!("missing required parameter: {}", k))),
    }
}
fn req_num(a: &Json, k: &str) -> Result<u64, ToolFail> {
    let v = a.at(k);
    let n = v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse::<f64>().ok()));
    match n {
        Some(n) if n >= 1.0 && n.fract() == 0.0 => Ok(n as u64),
        _ => Err(ToolFail::Msg(format!("missing required parameter: {}", k))),
    }
}
fn repo_of(f: &Forge, a: &Json) -> Result<u64, ToolFail> {
    let (o, n) = (req_str(a, "owner")?, req_str(a, "repo")?);
    f.repo_id(o, n).ok_or_else(|| ToolFail::Msg(format!("repository {}/{} not found", o, n)))
}

fn run_tool(f: &mut Forge, base: &str, name: &str, a: &Json) -> Result<Json, ToolFail> {
    match name {
        "create_repo" => {
            req_str(a, "name")?;
            let mut input = jobj! { "name" => a.at("name").clone() };
            input.set("description", a.at("description").clone());
            input.set("private", a.at("private").clone());
            let id = f.create_repo(&input)?;
            Ok(repo_json(f, base, id))
        }
        "get_repo" => {
            let id = repo_of(f, a)?;
            Ok(repo_json(f, base, id))
        }
        "list_repos" => {
            let ids = f.list_repos(&f.viewer.login.clone());
            Ok(Json::Arr(ids.into_iter().map(|id| repo_json(f, base, id)).collect()))
        }
        "create_issue" => {
            let id = repo_of(f, a)?;
            req_str(a, "title")?;
            let n = f.create_issue(id, a.at("title"), a.at("body"), a.at("labels"))?;
            Ok(issue_json(f, base, id, n))
        }
        "get_issue" => {
            let id = repo_of(f, a)?;
            let n = req_num(a, "issue_number")?;
            f.require_issue(id, n)?;
            Ok(issue_json(f, base, id, n))
        }
        "list_issues" => {
            let id = repo_of(f, a)?;
            let state = a.at("state").as_str().unwrap_or("open").to_lowercase();
            if !["open", "closed", "all"].contains(&state.as_str()) {
                return Err(ToolFail::Msg("state must be open, closed or all".into()));
            }
            let list = f.list_issues(id, &state, false, &[]);
            Ok(Json::Arr(list.into_iter().map(|n| issue_json(f, base, id, n)).collect()))
        }
        "update_issue" => {
            let id = repo_of(f, a)?;
            let n = req_num(a, "issue_number")?;
            let mut patch = Json::Obj(vec![]);
            for k in ["title", "body", "state"] {
                if let Some(v) = a.get(k) {
                    patch.set(k, v.clone());
                }
            }
            f.update_issue(id, n, &patch)?;
            Ok(issue_json(f, base, id, n))
        }
        "add_issue_comment" => {
            let id = repo_of(f, a)?;
            let n = req_num(a, "issue_number")?;
            let c = f.add_comment(id, n, a.at("body"))?;
            Ok(jobj! { "id" => c.id, "body" => c.body.as_str(), "user" => jobj! { "login" => c.user.as_str() }, "created_at" => c.created_at.as_str() })
        }
        "create_pull_request" => {
            let id = repo_of(f, a)?;
            for k in ["title", "head", "base"] {
                req_str(a, k)?;
            }
            let n = f.create_pull(id, a.at("title"), a.at("body"), a.at("head"), a.at("base"))?;
            Ok(pull_json(f, base, id, n))
        }
        "get_pull_request" => {
            let id = repo_of(f, a)?;
            let n = req_num(a, "pull_number")?;
            f.require_pull(id, n)?;
            Ok(pull_json(f, base, id, n))
        }
        "list_pull_requests" => {
            let id = repo_of(f, a)?;
            let state = a.at("state").as_str().unwrap_or("open").to_lowercase();
            let list = f.list_pulls(id, &state, false);
            Ok(Json::Arr(list.into_iter().map(|n| pull_json(f, base, id, n)).collect()))
        }
        "merge_pull_request" => {
            let id = repo_of(f, a)?;
            let n = req_num(a, "pull_number")?;
            f.require_pull(id, n)?;
            let sha = f.merge_pull(id, n, a.at("commit_title").as_str(), a.at("commit_message").as_str(), None)?;
            Ok(jobj! { "sha" => sha, "merged" => true, "message" => "Pull Request successfully merged", "number" => n })
        }
        "get_file_contents" => {
            let id = repo_of(f, a)?;
            let path = req_str(a, "path")?.to_string();
            let tip = f.resolve(id, a.at("ref").as_str()).ok_or_else(|| ToolFail::Msg("ref not found".into()))?;
            match f.objects.lookup(&tip, &path) {
                Some((false, _, oid)) => {
                    let data = f.objects.get(&oid).map(|o| o.data.clone()).unwrap_or_default();
                    Ok(jobj! { "path" => path.as_str(), "sha" => oid.as_str(), "size" => data.len(), "content" => String::from_utf8_lossy(&data).into_owned() })
                }
                _ => Err(ToolFail::Msg(format!("{} is not a file at that ref", path))),
            }
        }
        _ => Err(ToolFail::Msg("unknown tool".into())),
    }
}

fn call_tool(f: &mut Forge, base: &str, params: &Json) -> Result<Json, RpcError> {
    let name = params.at("name").as_str().unwrap_or("").to_lowercase();
    let Some(tool) = TOOLS.iter().find(|t| name == t.name || name.ends_with(&format!("__{}", t.name))) else {
        return Err(RpcError(-32602, format!("Unknown tool: {}", params.at("name").as_str().unwrap_or(""))));
    };
    let args = match params.at("arguments") {
        Json::Null => Json::Obj(vec![]),
        a if a.is_object() => a.clone(),
        _ => return Err(RpcError(-32602, "arguments must be an object".into())),
    };
    let missing: Vec<&str> = tool.props.iter().filter(|p| p.3).map(|p| p.0).filter(|k| matches!(args.at(k), Json::Null) || args.at(k).as_str() == Some("")).collect();
    if !missing.is_empty() {
        return Ok(jobj! { "content" => Json::Arr(vec![jobj! { "type" => "text", "text" => format!("missing required parameter(s): {}", missing.join(", ")) }]), "isError" => true });
    }
    match run_tool(f, base, tool.name, &args) {
        Ok(value) => {
            let text = value.to_string();
            let mut result = jobj! { "content" => Json::Arr(vec![jobj! { "type" => "text", "text" => text }]), "isError" => false };
            if value.is_object() {
                result.set("structuredContent", value);
            }
            Ok(result)
        }
        Err(ToolFail::Msg(m)) => Ok(jobj! { "content" => Json::Arr(vec![jobj! { "type" => "text", "text" => m }]), "isError" => true }),
    }
}

fn dispatch(f: &mut Forge, base: &str, msg: &Json) -> Option<Json> {
    if !msg.is_object() {
        return Some(jobj! { "jsonrpc" => "2.0", "id" => Json::Null, "error" => jobj! { "code" => -32600, "message" => "Invalid Request" } });
    }
    let id = msg.get("id").cloned();
    let Some(method) = msg.at("method").as_str() else {
        return id.map(|i| jobj! { "jsonrpc" => "2.0", "id" => i, "error" => jobj! { "code" => -32600, "message" => "Invalid Request: method is required" } });
    };
    let params = if msg.at("params").is_object() { msg.at("params").clone() } else { Json::Obj(vec![]) };
    let result: Result<Json, RpcError> = match method {
        "server/discover" => Ok(jobj! {
            "supportedVersions" => Json::Arr(SUPPORTED.iter().map(|s| Json::from(*s)).collect()),
            "capabilities" => capabilities(),
            "serverInfo" => server_info(),
            "instructions" => INSTRUCTIONS,
            "_meta" => jobj! { "io.modelcontextprotocol/serverInfo" => server_info() },
        }),
        "initialize" => {
            let requested = params.at("protocolVersion").as_str().unwrap_or("");
            let v = if SUPPORTED.contains(&requested) { requested } else { SUPPORTED[1] };
            Ok(jobj! { "protocolVersion" => v, "capabilities" => capabilities(), "serverInfo" => server_info(), "instructions" => INSTRUCTIONS })
        }
        "ping" => Ok(Json::Obj(vec![])),
        "tools/list" => Ok(tools_list().clone()),
        "tools/call" => call_tool(f, base, &params),
        "resources/list" => Ok(jobj! { "resources" => Json::Arr(vec![]) }),
        "prompts/list" => Ok(jobj! { "prompts" => Json::Arr(vec![]) }),
        m if m.starts_with("notifications/") => return None,
        m => Err(RpcError(-32601, format!("Method not found: {}", m))),
    };
    let id = id?;
    Some(match result {
        Ok(r) => jobj! { "jsonrpc" => "2.0", "id" => id, "result" => r },
        Err(RpcError(code, message)) => jobj! { "jsonrpc" => "2.0", "id" => id, "error" => jobj! { "code" => code, "message" => message } },
    })
}

fn rpc(status: u16, body: &Json) -> Response {
    Response::bytes(status, "application/json", body.to_string().into_bytes())
}

pub fn handle(forge: &std::sync::Mutex<Forge>, method: &str, body: &[u8], base: &str, authorized: bool) -> Response {
    if method == "DELETE" {
        return Response::bytes(if authorized { 405 } else { 401 }, "application/json", Vec::new());
    }
    if method != "POST" {
        return rpc(405, &jobj! { "jsonrpc" => "2.0", "id" => Json::Null, "error" => jobj! { "code" => -32600, "message" => "Use POST" } }).with_header("allow", "POST");
    }
    if !authorized {
        return rpc(401, &jobj! { "jsonrpc" => "2.0", "id" => Json::Null, "error" => jobj! { "code" => -32001, "message" => "Unauthorized: a bearer token is required" } }).with_header("www-authenticate", "Bearer realm=\"forge\"");
    }
    let parsed = match json::parse_bytes(body) {
        Ok(v) => v,
        Err(_) => return rpc(400, &jobj! { "jsonrpc" => "2.0", "id" => Json::Null, "error" => jobj! { "code" => -32700, "message" => "Parse error" } }),
    };
    let mut f = forge.lock().unwrap_or_else(|e| e.into_inner());
    match parsed {
        Json::Arr(items) => {
            if items.is_empty() {
                return rpc(400, &jobj! { "jsonrpc" => "2.0", "id" => Json::Null, "error" => jobj! { "code" => -32600, "message" => "Invalid Request: empty batch" } });
            }
            let answers: Vec<Json> = items.iter().filter_map(|m| dispatch(&mut f, base, m)).collect();
            if answers.is_empty() {
                Response::bytes(202, "application/json", Vec::new())
            } else {
                rpc(200, &Json::Arr(answers))
            }
        }
        other => match dispatch(&mut f, base, &other) {
            Some(a) => rpc(200, &a),
            None => Response::bytes(202, "application/json", Vec::new()),
        },
    }
}
