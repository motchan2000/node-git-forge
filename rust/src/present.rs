//! REST v3 representations of the store's objects (MCP reuses them) and Relay node ids.

use crate::jobj;
use crate::json::Json;
use crate::store::{Comment, Forge, Label};
use crate::util::{b64decode, b64encode, iso};

pub fn node_id(kind: &str, prefix: &str, id: impl std::fmt::Display) -> String {
    b64encode(format!("{}:{}{}", prefix, kind, id).as_bytes())
}
pub fn repo_node_id(id: u64) -> String {
    node_id("Repository", "010", id)
}
pub fn issue_node_id(id: u64) -> String {
    node_id("Issue", "05", id)
}
pub fn pull_node_id(id: u64) -> String {
    node_id("PullRequest", "011", id)
}
pub fn comment_node_id(id: u64) -> String {
    node_id("IssueComment", "012", id)
}
pub fn label_node_id(id: u64) -> String {
    node_id("Label", "010", id)
}

/// ("Repository", 5) from a node id.
pub fn decode_node_id(id: &str) -> Option<(String, u64)> {
    let bytes = b64decode(id)?;
    let text = String::from_utf8(bytes).ok()?;
    let (_, rest) = text.split_once(':')?;
    let split = rest.find(|c: char| c.is_ascii_digit())?;
    let (kind, num) = rest.split_at(split);
    if kind.is_empty() || !kind.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    Some((kind.to_string(), num.parse().ok()?))
}

pub fn user_json(f: &Forge, base: &str, login: &str) -> Json {
    let id = if login.eq_ignore_ascii_case(&f.viewer.login) { f.viewer.id } else { 0 };
    jobj! {
        "login" => login,
        "id" => id,
        "node_id" => node_id("User", "04", id),
        "type" => "User",
        "site_admin" => false,
        "url" => format!("{}/api/v3/users/{}", base, login),
        "html_url" => format!("{}/{}", base, login),
    }
}

pub fn repo_json(f: &Forge, base: &str, id: u64) -> Json {
    let r = f.repo(id);
    let full = r.full_name();
    let api = format!("{}/api/v3/repos/{}", base, full);
    jobj! {
        "id" => r.id,
        "node_id" => repo_node_id(r.id),
        "name" => r.name.as_str(),
        "full_name" => full.as_str(),
        "private" => r.private,
        "visibility" => if r.private { "private" } else { "public" },
        "owner" => user_json(f, base, &r.owner),
        "html_url" => format!("{}/{}", base, full),
        "description" => r.description.as_str(),
        "fork" => false,
        "url" => api,
        "clone_url" => format!("{}/{}.git", base, full),
        "git_url" => format!("{}/{}.git", base, full),
        "default_branch" => r.default_branch.as_str(),
        "created_at" => r.created_at.as_str(),
        "updated_at" => r.updated_at.as_str(),
        "pushed_at" => r.pushed_at.as_str(),
        "size" => 0,
        "archived" => false,
        "disabled" => false,
        "has_issues" => true,
        "has_projects" => false,
        "has_wiki" => false,
        "open_issues_count" => r.issues.values().filter(|i| i.open).count(),
        "permissions" => jobj! { "admin" => true, "maintain" => true, "push" => true, "triage" => true, "pull" => true },
    }
}

pub fn label_json(base: &str, owner: &str, name: &str, l: &Label) -> Json {
    jobj! {
        "id" => l.id,
        "node_id" => label_node_id(l.id),
        "url" => format!("{}/api/v3/repos/{}/{}/labels/{}", base, owner, name, crate::util::percent_encode_segment(&l.name)),
        "name" => l.name.as_str(),
        "color" => l.color.as_str(),
        "description" => l.description.as_str(),
        "default" => false,
    }
}

pub fn issue_json(f: &Forge, base: &str, id: u64, number: u64) -> Json {
    let r = f.repo(id);
    let i = &r.issues[&number];
    let api = format!("{}/api/v3/repos/{}", base, r.full_name());
    let labels: Vec<Json> = i
        .labels
        .iter()
        .map(|n| match r.label(n) {
            Some(l) => label_json(base, &r.owner, &r.name, l),
            None => jobj! { "name" => n.as_str() },
        })
        .collect();
    jobj! {
        "id" => i.id,
        "node_id" => issue_node_id(i.id),
        "url" => format!("{}/issues/{}", api, i.number),
        "repository_url" => api.as_str(),
        "html_url" => format!("{}/{}/issues/{}", base, r.full_name(), i.number),
        "number" => i.number,
        "title" => i.title.as_str(),
        "body" => i.body.as_str(),
        "state" => if i.open { "open" } else { "closed" },
        "state_reason" => if i.open { Json::Null } else { Json::from("completed") },
        "user" => user_json(f, base, &i.user),
        "labels" => labels,
        "assignee" => Json::Null,
        "assignees" => Json::Arr(vec![]),
        "milestone" => Json::Null,
        "locked" => false,
        "comments" => r.comment_count(i.number),
        "created_at" => i.created_at.as_str(),
        "updated_at" => i.updated_at.as_str(),
        "closed_at" => i.closed_at.clone(),
        "author_association" => "OWNER",
    }
}

pub fn comment_json(f: &Forge, base: &str, id: u64, c: &Comment) -> Json {
    let r = f.repo(id);
    let api = format!("{}/api/v3/repos/{}", base, r.full_name());
    jobj! {
        "id" => c.id,
        "node_id" => comment_node_id(c.id),
        "url" => format!("{}/issues/comments/{}", api, c.id),
        "issue_url" => format!("{}/issues/{}", api, c.issue_number),
        "html_url" => format!("{}/{}/issues/{}#issuecomment-{}", base, r.full_name(), c.issue_number, c.id),
        "body" => c.body.as_str(),
        "user" => user_json(f, base, &c.user),
        "created_at" => c.created_at.as_str(),
        "updated_at" => c.updated_at.as_str(),
        "author_association" => "OWNER",
    }
}

pub fn pull_json(f: &mut Forge, base: &str, id: u64, number: u64) -> Json {
    let (head_sha, base_sha) = f.pull_shas(id, number);
    let mergeable = f.mergeable(id, number);
    let repo_view = repo_json(f, base, id);
    let r = f.repo(id);
    let p = &r.pulls[&number];
    let api = format!("{}/api/v3/repos/{}", base, r.full_name());
    let owner_json = user_json(f, base, &r.owner);
    jobj! {
        "id" => p.id,
        "node_id" => pull_node_id(p.id),
        "url" => format!("{}/pulls/{}", api, p.number),
        "html_url" => format!("{}/{}/pull/{}", base, r.full_name(), p.number),
        "issue_url" => format!("{}/issues/{}", api, p.number),
        "number" => p.number,
        "state" => if p.open { "open" } else { "closed" },
        "locked" => false,
        "title" => p.title.as_str(),
        "body" => p.body.as_str(),
        "user" => user_json(f, base, &p.user),
        "labels" => Json::Arr(vec![]),
        "created_at" => p.created_at.as_str(),
        "updated_at" => p.updated_at.as_str(),
        "closed_at" => p.closed_at.clone(),
        "merged_at" => p.merged_at.clone(),
        "merge_commit_sha" => p.merge_commit_sha.clone(),
        "head" => jobj! { "label" => format!("{}:{}", r.owner, p.head), "ref" => p.head.as_str(), "sha" => head_sha, "user" => owner_json.clone(), "repo" => repo_view.clone() },
        "base" => jobj! { "label" => format!("{}:{}", r.owner, p.base), "ref" => p.base.as_str(), "sha" => base_sha, "user" => owner_json, "repo" => repo_view },
        "draft" => false,
        "merged" => p.merged,
        "mergeable" => mergeable,
        "mergeable_state" => if p.open { if mergeable == Some(true) { "clean" } else { "dirty" } } else { "unknown" },
        "merged_by" => if p.merged { user_json(f, base, &f.viewer.login) } else { Json::Null },
        "comments" => r.comment_count(p.number),
        "commits" => 1,
        "author_association" => "OWNER",
    }
}

pub fn commit_json(f: &mut Forge, base: &str, id: u64, oid: &str) -> Option<Json> {
    let c = f.objects.commit(oid)?;
    let r = f.repo(id);
    let api = format!("{}/api/v3/repos/{}", base, r.full_name());
    let message = c.message.strip_suffix('\n').unwrap_or(&c.message);
    Some(jobj! {
        "sha" => oid,
        "node_id" => node_id("Commit", "06", oid),
        "url" => format!("{}/commits/{}", api, oid),
        "html_url" => format!("{}/{}/commit/{}", base, r.full_name(), oid),
        "commit" => jobj! {
            "message" => message,
            "author" => jobj! { "name" => c.author.name.as_str(), "email" => c.author.email.as_str(), "date" => iso(c.author.time) },
            "committer" => jobj! { "name" => c.committer.name.as_str(), "email" => c.committer.email.as_str(), "date" => iso(c.committer.time) },
            "tree" => jobj! { "sha" => c.tree.as_str(), "url" => format!("{}/git/trees/{}", api, c.tree) },
            "url" => format!("{}/git/commits/{}", api, oid),
        },
        "author" => Json::Null,
        "committer" => Json::Null,
        "parents" => Json::Arr(c.parents.iter().map(|p| jobj! { "sha" => p.as_str(), "url" => format!("{}/commits/{}", api, p) }).collect()),
    })
}
