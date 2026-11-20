//! The forge's one store and its domain services. REST, GraphQL, MCP and git all call these, so
//! ids, numbers and states agree across every surface.

use crate::json::Json;
use crate::objects::*;
use crate::util::iso;
use std::collections::{BTreeMap, HashMap};

#[derive(Debug)]
pub struct ForgeError {
    pub status: u16,
    pub message: String,
}

pub fn ferr<T>(status: u16, message: impl Into<String>) -> Result<T, ForgeError> {
    Err(ForgeError { status, message: message.into() })
}

pub type FResult<T> = Result<T, ForgeError>;

pub struct User {
    pub id: u64,
    pub login: String,
    pub name: String,
    pub email: String,
}

#[derive(Clone)]
pub struct Label {
    pub id: u64,
    pub name: String,
    pub color: String,
    pub description: String,
}

#[derive(Clone)]
pub struct Comment {
    pub id: u64,
    pub issue_number: u64,
    pub body: String,
    pub user: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone)]
pub struct Issue {
    pub id: u64,
    pub number: u64,
    pub title: String,
    pub body: String,
    pub open: bool,
    pub labels: Vec<String>,
    pub user: String,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
    /// Bumped on every change: response caches key on it.
    pub version: u64,
}

#[derive(Clone)]
pub struct Pull {
    pub id: u64,
    pub number: u64,
    pub title: String,
    pub body: String,
    pub open: bool,
    pub head: String,
    pub base: String,
    pub head_sha: String,
    pub base_sha: String,
    pub merged: bool,
    pub merged_at: Option<String>,
    pub merge_commit_sha: Option<String>,
    pub user: String,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
}

pub struct Repo {
    pub id: u64,
    pub owner: String,
    pub name: String,
    pub description: String,
    pub private: bool,
    pub default_branch: String,
    pub created_at: String,
    pub updated_at: String,
    pub pushed_at: String,
    pub refs: BTreeMap<String, String>,
    pub issues: BTreeMap<u64, Issue>,
    pub pulls: BTreeMap<u64, Pull>,
    pub labels: Vec<Label>,
    pub comments: BTreeMap<u64, Vec<Comment>>,
    pub next_number: u64,
    pub ref_version: u64,
}

impl Repo {
    pub fn label(&self, name: &str) -> Option<&Label> {
        let lower = name.to_lowercase();
        self.labels.iter().find(|l| l.name.to_lowercase() == lower)
    }
    pub fn comment_count(&self, number: u64) -> usize {
        self.comments.get(&number).map(|c| c.len()).unwrap_or(0)
    }
    pub fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

pub struct FileChangeOut {
    pub path: String,
    pub status: &'static str,
    pub additions: usize,
    pub deletions: usize,
    pub sha: String,
}

pub struct Forge {
    pub objects: ObjectStore,
    pub viewer: User,
    repos: BTreeMap<u64, Repo>,
    by_key: HashMap<String, u64>,
    next_repo_id: u64,
    next_issue_id: u64,
    next_pull_id: u64,
    next_comment_id: u64,
    next_label_id: u64,
    epoch: i64,
    tick: i64,
    /// Serialized single-issue replies: (repo, number) -> (issue version, origin, JSON text).
    pub issue_cache: HashMap<(u64, u64), (u64, String, String)>,
}

fn key(owner: &str, name: &str) -> String {
    format!("{}/{}", owner.to_lowercase(), name.to_lowercase())
}

fn valid_repo_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 100 && name != "." && name != ".." && name.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

impl Forge {
    pub fn new(viewer: User) -> Forge {
        let epoch = std::env::var("MAKEFUN_TRIAL_NOW").ok().and_then(|s| crate::util::parse_time(&s)).unwrap_or(1_767_225_600); // 2026-01-01
        Forge {
            objects: ObjectStore::default(),
            viewer,
            repos: BTreeMap::new(),
            by_key: HashMap::new(),
            next_repo_id: 1,
            next_issue_id: 1,
            next_pull_id: 1,
            next_comment_id: 1,
            next_label_id: 1,
            epoch,
            tick: 0,
            issue_cache: HashMap::new(),
        }
    }

    /// The deterministic clock: the trial's opening time plus one second per event.
    pub fn now(&mut self) -> (i64, String) {
        self.tick += 1;
        let s = self.epoch + self.tick;
        (s, iso(s))
    }
    pub fn now_iso(&mut self) -> String {
        self.now().1
    }
    pub fn signature(&mut self) -> Signature {
        let (t, _) = self.now();
        Signature { name: self.viewer.name.clone(), email: self.viewer.email.clone(), time: t, tz: "+0000".into() }
    }

    // ---- repositories

    pub fn repo_id(&self, owner: &str, name: &str) -> Option<u64> {
        self.by_key.get(&key(owner, name)).copied()
    }
    pub fn repo(&self, id: u64) -> &Repo {
        &self.repos[&id]
    }
    pub fn repo_mut(&mut self, id: u64) -> &mut Repo {
        self.repos.get_mut(&id).unwrap()
    }
    pub fn require_repo(&self, owner: &str, name: &str) -> FResult<u64> {
        self.repo_id(owner, name).ok_or(ForgeError { status: 404, message: "Not Found".into() })
    }
    pub fn repo_by_id(&self, id: u64) -> Option<u64> {
        if self.repos.contains_key(&id) {
            Some(id)
        } else {
            None
        }
    }
    pub fn list_repos(&self, owner: &str) -> Vec<u64> {
        let lower = owner.to_lowercase();
        self.repos.values().filter(|r| r.owner.to_lowercase() == lower).map(|r| r.id).collect()
    }
    pub fn all_repo_ids(&self) -> Vec<u64> {
        self.repos.keys().copied().collect()
    }

    pub fn create_repo(&mut self, input: &Json) -> FResult<u64> {
        let name = match input.at("name").as_str() {
            Some(n) if valid_repo_name(n) => n.to_string(),
            _ => return ferr(422, "Repository creation failed: name is invalid"),
        };
        if self.repo_id(&self.viewer.login.clone(), &name).is_some() {
            return ferr(422, "Repository creation failed: name already exists on this account");
        }
        let t = self.now_iso();
        let id = self.next_repo_id;
        self.next_repo_id += 1;
        let repo = Repo {
            id,
            owner: self.viewer.login.clone(),
            name: name.clone(),
            description: input.at("description").as_str().unwrap_or("").to_string(),
            private: input.at("private").as_bool() == Some(true),
            default_branch: match input.at("default_branch").as_str() {
                Some(b) if !b.is_empty() => b.to_string(),
                _ => "main".into(),
            },
            created_at: t.clone(),
            updated_at: t.clone(),
            pushed_at: t,
            refs: BTreeMap::new(),
            issues: BTreeMap::new(),
            pulls: BTreeMap::new(),
            labels: Vec::new(),
            comments: BTreeMap::new(),
            next_number: 1,
            ref_version: 0,
        };
        self.by_key.insert(key(&repo.owner, &name), id);
        self.repos.insert(id, repo);
        Ok(id)
    }

    pub fn update_repo(&mut self, id: u64, patch: &Json) -> FResult<()> {
        let d = patch.at("description");
        if !d.is_null() || patch.get("description").is_some() {
            if !d.is_null() && d.as_str().is_none() {
                return ferr(422, "description must be a string");
            }
        }
        if let Some(p) = patch.get("private") {
            if p.as_bool().is_none() {
                return ferr(422, "private must be a boolean");
            }
        }
        if let Some(b) = patch.get("default_branch") {
            if b.as_str().map(|s| s.is_empty()).unwrap_or(true) {
                return ferr(422, "default_branch must be a branch name");
            }
        }
        let t = self.now_iso();
        let repo = self.repo_mut(id);
        if patch.get("description").is_some() {
            repo.description = patch.at("description").as_str().unwrap_or("").to_string();
        }
        if let Some(p) = patch.at("private").as_bool() {
            repo.private = p;
        }
        if let Some(b) = patch.at("default_branch").as_str() {
            repo.default_branch = b.to_string();
        }
        repo.updated_at = t;
        Ok(())
    }

    pub fn delete_repo(&mut self, id: u64) {
        if let Some(r) = self.repos.remove(&id) {
            self.by_key.remove(&key(&r.owner, &r.name));
        }
    }

    // ---- refs

    pub fn set_ref(&mut self, id: u64, refname: &str, oid: Option<&str>) {
        let t = self.now_iso();
        let repo = self.repo_mut(id);
        match oid {
            Some(o) if o != ZERO_OID => {
                repo.refs.insert(refname.to_string(), o.to_string());
            }
            _ => {
                repo.refs.remove(refname);
            }
        }
        repo.ref_version += 1;
        repo.pushed_at = t;
    }

    pub fn branch_tip(&self, id: u64, branch: &str) -> Option<String> {
        self.repo(id).refs.get(&format!("refs/heads/{}", branch)).cloned()
    }

    /// HEAD's branch: the default branch, or (while it is unborn) the first branch that exists.
    pub fn head_branch(&self, id: u64) -> String {
        let repo = self.repo(id);
        if repo.refs.contains_key(&format!("refs/heads/{}", repo.default_branch)) {
            return repo.default_branch.clone();
        }
        for r in repo.refs.keys() {
            if let Some(b) = r.strip_prefix("refs/heads/") {
                return b.to_string();
            }
        }
        repo.default_branch.clone()
    }

    /// A branch, tag, full ref or commit sha, to a commit oid.
    pub fn resolve(&self, id: u64, refish: Option<&str>) -> Option<String> {
        let head;
        let name = match refish {
            Some(r) if !r.is_empty() => r,
            _ => {
                head = self.head_branch(id);
                &head
            }
        };
        let repo = self.repo(id);
        let direct = repo.refs.get(name).or_else(|| repo.refs.get(&format!("refs/heads/{}", name))).or_else(|| repo.refs.get(&format!("refs/tags/{}", name)));
        if let Some(d) = direct {
            return self.objects.peel_to_commit(d);
        }
        let lower = name.to_lowercase();
        if crate::util::is_oid(&lower) {
            return if self.objects.has(&lower) { self.objects.peel_to_commit(&lower) } else { None };
        }
        if lower.len() >= 4 && lower.len() < 40 && lower.bytes().all(|c| c.is_ascii_hexdigit()) {
            for (oid, obj) in &self.objects.objects {
                if oid.starts_with(&lower) && obj.kind == Kind::Commit {
                    return Some(oid.clone());
                }
            }
        }
        None
    }

    // ---- commits on branches

    pub fn make_commit(&mut self, tree: String, parents: Vec<String>, message: String) -> String {
        let sig = self.signature();
        let c = Commit { tree, parents, author: sig.clone(), committer: sig, message };
        self.objects.put(Kind::Commit, serialize_commit(&c))
    }

    /// Commit file edits (None deletes) on a branch, creating it (or the first commit) when absent.
    pub fn commit_files(&mut self, id: u64, branch: &str, edits: Vec<(String, Option<Vec<u8>>)>, message: String) -> String {
        let parent = self.branch_tip(id, branch);
        let mut flat = match &parent {
            Some(p) => self.objects.flat_at(p),
            None => Flat::new(),
        };
        for (path, content) in edits {
            match content {
                None => {
                    flat.remove(&path);
                }
                Some(data) => {
                    let mode = flat.get(&path).map(|e| e.0.clone()).unwrap_or_else(|| "100644".into());
                    let oid = self.objects.put(Kind::Blob, data);
                    flat.insert(path, (mode, oid));
                }
            }
        }
        let tree = self.objects.build_tree(&flat);
        let commit = self.make_commit(tree, parent.into_iter().collect(), message);
        self.set_ref(id, &format!("refs/heads/{}", branch), Some(&commit));
        commit
    }

    // ---- labels, issues, comments

    pub fn create_label(&mut self, id: u64, input: &Json) -> FResult<Label> {
        let name = match input.at("name").as_str() {
            Some(n) if !n.trim().is_empty() => n.to_string(),
            _ => return ferr(422, "Validation Failed: name is required"),
        };
        if self.repo(id).label(&name).is_some() {
            return ferr(422, "Validation Failed: label already exists");
        }
        let label = Label {
            id: self.next_label_id,
            name,
            color: match input.at("color").as_str() {
                Some(c) if !c.is_empty() => c.trim_start_matches('#').to_string(),
                _ => "ededed".into(),
            },
            description: input.at("description").as_str().unwrap_or("").to_string(),
        };
        self.next_label_id += 1;
        self.repo_mut(id).labels.push(label.clone());
        Ok(label)
    }

    pub fn resolve_labels(&self, id: u64, labels: &Json) -> FResult<Vec<String>> {
        if labels.is_null() {
            return Ok(vec![]);
        }
        let Some(items) = labels.as_array() else { return ferr(422, "Validation Failed: labels must be an array") };
        let mut out: Vec<String> = Vec::new();
        for item in items {
            let name = item.as_str().or_else(|| item.at("name").as_str());
            let Some(name) = name else { return ferr(422, "Validation Failed: a label must be a name") };
            let Some(label) = self.repo(id).label(name) else {
                return ferr(422, format!("Validation Failed: label {} does not exist", Json::from(name).to_string()));
            };
            if !out.contains(&label.name) {
                out.push(label.name.clone());
            }
        }
        Ok(out)
    }

    pub fn create_issue(&mut self, id: u64, title: &Json, body: &Json, labels: &Json) -> FResult<u64> {
        let title = match title.as_str() {
            Some(t) if !t.is_empty() => t.to_string(),
            _ => return ferr(422, "Validation Failed: title is required"),
        };
        if !body.is_null() && body.as_str().is_none() {
            return ferr(422, "Validation Failed: body must be a string");
        }
        let labels = self.resolve_labels(id, labels)?;
        let t = self.now_iso();
        let issue_id = self.next_issue_id;
        self.next_issue_id += 1;
        let user = self.viewer.login.clone();
        let repo = self.repo_mut(id);
        let number = repo.next_number;
        repo.next_number += 1;
        repo.issues.insert(
            number,
            Issue {
                id: issue_id,
                number,
                title,
                body: body.as_str().unwrap_or("").to_string(),
                open: true,
                labels,
                user,
                created_at: t.clone(),
                updated_at: t,
                closed_at: None,
                version: 1,
            },
        );
        Ok(number)
    }

    pub fn require_issue(&self, id: u64, number: u64) -> FResult<()> {
        if self.repo(id).issues.contains_key(&number) {
            Ok(())
        } else {
            ferr(404, "Not Found")
        }
    }

    pub fn update_issue(&mut self, id: u64, number: u64, patch: &Json) -> FResult<()> {
        self.require_issue(id, number)?;
        if let Some(t) = patch.get("title") {
            if t.as_str().map(|s| s.is_empty()).unwrap_or(true) {
                return ferr(422, "Validation Failed: title must be a non-empty string");
            }
        }
        if let Some(b) = patch.get("body") {
            if !b.is_null() && b.as_str().is_none() {
                return ferr(422, "Validation Failed: body must be a string");
            }
        }
        let state = patch.at("state");
        if !state.is_null() && state.as_str() != Some("open") && state.as_str() != Some("closed") {
            return ferr(422, "Validation Failed: state must be open or closed");
        }
        let labels = match patch.get("labels") {
            Some(l) => Some(self.resolve_labels(id, l)?),
            None => None,
        };
        let t = self.now_iso();
        let issue = self.repo_mut(id).issues.get_mut(&number).unwrap();
        if let Some(s) = patch.at("title").as_str() {
            issue.title = s.to_string();
        }
        if patch.get("body").is_some() {
            issue.body = patch.at("body").as_str().unwrap_or("").to_string();
        }
        if let Some(l) = labels {
            issue.labels = l;
        }
        match state.as_str() {
            Some("closed") if issue.open => {
                issue.open = false;
                issue.closed_at = Some(t.clone());
            }
            Some("open") if !issue.open => {
                issue.open = true;
                issue.closed_at = None;
            }
            _ => {}
        }
        issue.updated_at = t;
        issue.version += 1;
        Ok(())
    }

    /// Issue numbers filtered by state ("open" default, "closed", "all") and labels, by number.
    pub fn list_issues(&self, id: u64, state: &str, asc: bool, labels: &[String]) -> Vec<u64> {
        let repo = self.repo(id);
        let wanted: Vec<String> = labels.iter().map(|l| l.to_lowercase()).collect();
        let mut out: Vec<u64> = repo
            .issues
            .values()
            .filter(|i| match state {
                "open" => i.open,
                "closed" => !i.open,
                _ => true,
            })
            .filter(|i| wanted.iter().all(|w| i.labels.iter().any(|l| l.to_lowercase() == *w)))
            .map(|i| i.number)
            .collect();
        if !asc {
            out.reverse();
        }
        out
    }

    pub fn add_comment(&mut self, id: u64, number: u64, body: &Json) -> FResult<Comment> {
        {
            let repo = self.repo(id);
            if !repo.issues.contains_key(&number) && !repo.pulls.contains_key(&number) {
                return ferr(404, "Not Found");
            }
        }
        let body = match body.as_str() {
            Some(b) if !b.is_empty() => b.to_string(),
            _ => return ferr(422, "Validation Failed: body is required"),
        };
        let t = self.now_iso();
        let c = Comment { id: self.next_comment_id, issue_number: number, body, user: self.viewer.login.clone(), created_at: t.clone(), updated_at: t.clone() };
        self.next_comment_id += 1;
        let repo = self.repo_mut(id);
        repo.comments.entry(number).or_default().push(c.clone());
        if let Some(issue) = repo.issues.get_mut(&number) {
            issue.updated_at = t;
            issue.version += 1;
        }
        Ok(c)
    }

    pub fn list_comments(&self, id: u64, number: u64) -> FResult<Vec<Comment>> {
        let repo = self.repo(id);
        if !repo.issues.contains_key(&number) && !repo.pulls.contains_key(&number) {
            return ferr(404, "Not Found");
        }
        Ok(repo.comments.get(&number).cloned().unwrap_or_default())
    }

    pub fn find_issue(&self, issue_id: u64) -> Option<(u64, u64)> {
        for r in self.repos.values() {
            for i in r.issues.values() {
                if i.id == issue_id {
                    return Some((r.id, i.number));
                }
            }
        }
        None
    }
    pub fn find_pull(&self, pull_id: u64) -> Option<(u64, u64)> {
        for r in self.repos.values() {
            for p in r.pulls.values() {
                if p.id == pull_id {
                    return Some((r.id, p.number));
                }
            }
        }
        None
    }
    pub fn find_comment(&self, comment_id: u64) -> Option<(u64, Comment)> {
        for r in self.repos.values() {
            for list in r.comments.values() {
                for c in list {
                    if c.id == comment_id {
                        return Some((r.id, c.clone()));
                    }
                }
            }
        }
        None
    }

    // ---- pull requests

    pub fn create_pull(&mut self, id: u64, title: &Json, body: &Json, head: &Json, base: &Json) -> FResult<u64> {
        let title = match title.as_str() {
            Some(t) if !t.is_empty() => t.to_string(),
            _ => return ferr(422, "Validation Failed: title is required"),
        };
        let head = match head.as_str() {
            Some(h) if !h.is_empty() => {
                let h = h.rsplit_once(':').map(|(_, b)| b).unwrap_or(h);
                h.strip_prefix("refs/heads/").unwrap_or(h).to_string()
            }
            _ => return ferr(422, "Validation Failed: head is required"),
        };
        let base = match base.as_str() {
            Some(b) if !b.is_empty() => b.strip_prefix("refs/heads/").unwrap_or(b).to_string(),
            _ => return ferr(422, "Validation Failed: base is required"),
        };
        let Some(head_sha) = self.branch_tip(id, &head) else {
            return ferr(422, format!("Validation Failed: head branch \"{}\" does not exist", head));
        };
        let Some(base_sha) = self.branch_tip(id, &base) else {
            return ferr(422, format!("Validation Failed: base branch \"{}\" does not exist", base));
        };
        if head == base {
            return ferr(422, "Validation Failed: head and base must differ");
        }
        if self.repo(id).pulls.values().any(|p| p.open && p.head == head && p.base == base) {
            return ferr(422, format!("Validation Failed: A pull request already exists for {}:{}.", self.repo(id).owner, head));
        }
        let t = self.now_iso();
        let pull_id = self.next_pull_id;
        self.next_pull_id += 1;
        let user = self.viewer.login.clone();
        let repo = self.repo_mut(id);
        let number = repo.next_number;
        repo.next_number += 1;
        repo.pulls.insert(
            number,
            Pull {
                id: pull_id,
                number,
                title,
                body: body.as_str().unwrap_or("").to_string(),
                open: true,
                head,
                base,
                head_sha,
                base_sha,
                merged: false,
                merged_at: None,
                merge_commit_sha: None,
                user,
                created_at: t.clone(),
                updated_at: t,
                closed_at: None,
            },
        );
        Ok(number)
    }

    pub fn require_pull(&self, id: u64, number: u64) -> FResult<()> {
        if self.repo(id).pulls.contains_key(&number) {
            Ok(())
        } else {
            ferr(404, "Not Found")
        }
    }

    /// Live head/base shas: the branch tips while open, the recorded ones once closed.
    pub fn pull_shas(&mut self, id: u64, number: u64) -> (String, String) {
        let (open, head, base) = {
            let p = &self.repo(id).pulls[&number];
            (p.open, p.head.clone(), p.base.clone())
        };
        if open {
            let h = self.branch_tip(id, &head);
            let b = self.branch_tip(id, &base);
            let p = self.repo_mut(id).pulls.get_mut(&number).unwrap();
            if let Some(h) = h {
                p.head_sha = h;
            }
            if let Some(b) = b {
                p.base_sha = b;
            }
        }
        let p = &self.repo(id).pulls[&number];
        (p.head_sha.clone(), p.base_sha.clone())
    }

    pub fn list_pulls(&self, id: u64, state: &str, asc: bool) -> Vec<u64> {
        let mut out: Vec<u64> = self
            .repo(id)
            .pulls
            .values()
            .filter(|p| match state {
                "open" => p.open,
                "closed" => !p.open,
                _ => true,
            })
            .map(|p| p.number)
            .collect();
        if !asc {
            out.reverse();
        }
        out
    }

    pub fn update_pull(&mut self, id: u64, number: u64, patch: &Json) -> FResult<()> {
        if let Some(t) = patch.get("title") {
            if t.as_str().map(|s| s.is_empty()).unwrap_or(true) {
                return ferr(422, "Validation Failed: title must be a non-empty string");
            }
        }
        let state = patch.at("state");
        if !state.is_null() && state.as_str() != Some("open") && state.as_str() != Some("closed") {
            return ferr(422, "Validation Failed: state must be open or closed");
        }
        self.pull_shas(id, number);
        let t = self.now_iso();
        let new_base = patch.at("base").as_str().filter(|b| self.branch_tip(id, b).is_some()).map(|s| s.to_string());
        let p = self.repo_mut(id).pulls.get_mut(&number).unwrap();
        if let Some(s) = patch.at("title").as_str() {
            p.title = s.to_string();
        }
        if let Some(s) = patch.at("body").as_str() {
            p.body = s.to_string();
        }
        if let Some(b) = new_base {
            p.base = b;
        }
        match state.as_str() {
            Some("closed") if p.open => {
                p.open = false;
                p.closed_at = Some(t.clone());
            }
            Some("open") if !p.open && !p.merged => {
                p.open = true;
                p.closed_at = None;
            }
            _ => {}
        }
        p.updated_at = t;
        Ok(())
    }

    pub fn pull_files(&mut self, id: u64, number: u64) -> Vec<FileChangeOut> {
        let (head, base) = self.pull_shas(id, number);
        let merged = self.repo(id).pulls[&number].merged;
        let from = if merged { base.clone() } else { self.objects.merge_base(&head, &base).unwrap_or(base.clone()) };
        let before = self.objects.flat_at(&from);
        let after = self.objects.flat_at(&head);
        diff_flat(&before, &after)
            .into_iter()
            .map(|c| {
                let text = |e: &Option<(String, String)>, s: &ObjectStore| -> String {
                    e.as_ref().and_then(|(_, oid)| s.get(oid)).map(|o| String::from_utf8_lossy(&o.data).into_owned()).unwrap_or_default()
                };
                let (a, d) = line_counts(&text(&c.before, &self.objects), &text(&c.after, &self.objects));
                let sha = c.after.as_ref().or(c.before.as_ref()).map(|e| e.1.clone()).unwrap_or_else(|| ZERO_OID.into());
                FileChangeOut { path: c.path, status: c.status, additions: a, deletions: d, sha }
            })
            .collect()
    }

    /// The merged flat tree, or None on a conflict.
    pub fn merge_result(&mut self, id: u64, number: u64) -> Option<Flat> {
        let (head, base) = self.pull_shas(id, number);
        if self.objects.is_ancestor(&base, &head) {
            return Some(self.objects.flat_at(&head));
        }
        let mb = self.objects.merge_base(&head, &base);
        let base_flat = match mb {
            Some(m) => self.objects.flat_at(&m),
            None => Flat::new(),
        };
        let ours = self.objects.flat_at(&base);
        let theirs = self.objects.flat_at(&head);
        merge_flat(&base_flat, &ours, &theirs)
    }

    pub fn mergeable(&mut self, id: u64, number: u64) -> Option<bool> {
        if !self.repo(id).pulls[&number].open {
            return None;
        }
        Some(self.merge_result(id, number).is_some())
    }

    /// Squash-merge an open pull onto its base branch.
    pub fn merge_pull(&mut self, id: u64, number: u64, commit_title: Option<&str>, commit_message: Option<&str>, expected_sha: Option<&str>) -> FResult<String> {
        {
            let p = &self.repo(id).pulls[&number];
            if p.merged {
                return ferr(405, "Pull Request is not mergeable: already merged");
            }
            if !p.open {
                return ferr(405, "Pull Request is not mergeable: it is closed");
            }
        }
        let (head, base) = self.pull_shas(id, number);
        if let Some(sha) = expected_sha {
            if !sha.is_empty() && sha.to_lowercase() != head {
                return ferr(409, "Head branch was modified. Review and try the merge again.");
            }
        }
        let Some(merged) = self.merge_result(id, number) else { return ferr(405, "Pull Request is not mergeable: merge conflict") };
        let tree = self.objects.build_tree(&merged);
        let (ptitle, pbase) = {
            let p = &self.repo(id).pulls[&number];
            (p.title.clone(), p.base.clone())
        };
        let title = match commit_title {
            Some(t) if !t.is_empty() => t.to_string(),
            _ => format!("{} (#{})", ptitle, number),
        };
        let extra = match commit_message {
            Some(m) if !m.is_empty() => format!("\n\n{}", m),
            _ => String::new(),
        };
        let sha = self.make_commit(tree, vec![base.clone()], format!("{}{}\n", title, extra));
        self.set_ref(id, &format!("refs/heads/{}", pbase), Some(&sha));
        let t = self.now_iso();
        let p = self.repo_mut(id).pulls.get_mut(&number).unwrap();
        p.head_sha = head;
        p.base_sha = base;
        p.merged = true;
        p.merged_at = Some(t.clone());
        p.closed_at = Some(t.clone());
        p.updated_at = t;
        p.open = false;
        p.merge_commit_sha = Some(sha.clone());
        Ok(sha)
    }
}
