//! GitHub GraphQL v4 at POST /api/graphql: a schema declared as text (it drives both validation and
//! introspection), an executor over parsed documents, and resolvers over the shared Forge store.

use crate::gql_parse::*;
use crate::jobj;
use crate::json::{self, Json};
use crate::present::*;
use crate::store::{Comment, Forge, ForgeError, Label};
use crate::util::{b64decode, b64encode, iso};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

// ---------------------------------------------------------------------------------------------
// Schema

pub struct FieldDef {
    pub name: String,
    pub ty: String,
    pub args: Vec<(String, String)>,
}

pub struct TypeDef {
    pub kind: &'static str,
    pub fields: Vec<FieldDef>,
    pub interfaces: Vec<String>,
    pub possible: Vec<String>,
    pub enums: Vec<String>,
}

const CONN: &str = "first:Int,last:Int,after:String,before:String";

const SDL: &str = r#"
OBJECT Query
 viewer User!
 user(login:String!) User
 repositoryOwner(login:String!) RepositoryOwner
 repository(owner:String!,name:String!,followRenames:Boolean) Repository
 node(id:ID!) Node
 nodes(ids:[ID!]!) [Node]!
 rateLimit RateLimit
 __type(name:String!) __Type
 __schema __Schema!
OBJECT Mutation
 createIssue(input:CreateIssueInput!) CreateIssuePayload
 updateIssue(input:UpdateIssueInput!) UpdateIssuePayload
 closeIssue(input:CloseIssueInput!) CloseIssuePayload
 reopenIssue(input:ReopenIssueInput!) ReopenIssuePayload
 addComment(input:AddCommentInput!) AddCommentPayload
 createPullRequest(input:CreatePullRequestInput!) CreatePullRequestPayload
 mergePullRequest(input:MergePullRequestInput!) MergePullRequestPayload
 closePullRequest(input:ClosePullRequestInput!) ClosePullRequestPayload
INTERFACE Node = Repository Issue PullRequest IssueComment User Label
 id ID!
INTERFACE Actor = User
 login String!
 url URI!
 avatarUrl(size:Int) URI!
INTERFACE RepositoryOwner = User
 id ID!
 login String!
 url URI!
UNION IssueOrPullRequest = Issue PullRequest
OBJECT User : Node Actor RepositoryOwner
 id ID!
 databaseId Int
 login String!
 name String
 email String!
 url URI!
 avatarUrl(size:Int) URI!
 repository(name:String!) Repository
 repositories(CONN) RepositoryConnection!
OBJECT Repository : Node
 id ID!
 databaseId Int
 name String!
 nameWithOwner String!
 owner RepositoryOwner!
 description String
 url URI!
 sshUrl GitSSHRemote!
 isPrivate Boolean!
 visibility RepositoryVisibility!
 isFork Boolean!
 isArchived Boolean!
 isEmpty Boolean!
 isTemplate Boolean!
 hasIssuesEnabled Boolean!
 hasWikiEnabled Boolean!
 hasProjectsEnabled Boolean!
 viewerPermission RepositoryPermission
 createdAt DateTime!
 updatedAt DateTime!
 pushedAt DateTime
 defaultBranchRef Ref
 ref(qualifiedName:String!) Ref
 issue(number:Int!) Issue
 issueOrPullRequest(number:Int!) IssueOrPullRequest
 issues(CONN,states:[IssueState!],orderBy:IssueOrder,labels:[String!],filterBy:IssueFilters) IssueConnection!
 pullRequest(number:Int!) PullRequest
 pullRequests(CONN,states:[PullRequestState!],orderBy:IssueOrder,headRefName:String,baseRefName:String,labels:[String!]) PullRequestConnection!
 labels(CONN,query:String) LabelConnection
 label(name:String!) Label
OBJECT Issue : Node
 id ID!
 databaseId Int
 number Int!
 title String!
 body String!
 bodyText String!
 state IssueState!
 stateReason IssueStateReason
 closed Boolean!
 closedAt DateTime
 createdAt DateTime!
 updatedAt DateTime!
 url URI!
 author Actor
 repository Repository!
 locked Boolean!
 labels(CONN) LabelConnection
 comments(CONN) IssueCommentConnection!
 assignees(CONN) UserConnection!
OBJECT PullRequest : Node
 id ID!
 databaseId Int
 number Int!
 title String!
 body String!
 state PullRequestState!
 merged Boolean!
 mergedAt DateTime
 closed Boolean!
 closedAt DateTime
 isDraft Boolean!
 createdAt DateTime!
 updatedAt DateTime!
 url URI!
 headRefName String!
 baseRefName String!
 headRefOid GitObjectID!
 baseRefOid GitObjectID!
 headRepository Repository
 baseRepository Repository
 repository Repository!
 mergeable MergeableState!
 mergeCommit Commit
 author Actor
 files(CONN) PullRequestChangedFileConnection
 changedFiles Int!
 additions Int!
 deletions Int!
 comments(CONN) IssueCommentConnection!
 labels(CONN) LabelConnection
OBJECT IssueComment : Node
 id ID!
 databaseId Int
 body String!
 createdAt DateTime!
 updatedAt DateTime!
 url URI!
 author Actor
OBJECT Label : Node
 id ID!
 name String!
 color String!
 description String
OBJECT Ref
 id ID!
 name String!
 prefix String!
 target Commit
OBJECT Commit
 oid GitObjectID!
 abbreviatedOid String!
 message String!
 committedDate DateTime!
OBJECT PullRequestChangedFile
 path String!
 additions Int!
 deletions Int!
 changeType PatchStatus!
OBJECT PageInfo
 hasNextPage Boolean!
 hasPreviousPage Boolean!
 startCursor String
 endCursor String
OBJECT RateLimit
 limit Int!
 remaining Int!
 cost Int!
 used Int!
 resetAt DateTime!
 nodeCount Int!
CONNECTION Issue
CONNECTION PullRequest
CONNECTION PullRequestChangedFile
CONNECTION IssueComment
CONNECTION Label
CONNECTION User
CONNECTION Repository
OBJECT TimelineEdge
 node Node
 cursor String!
PAYLOAD CreateIssuePayload
 issue Issue
PAYLOAD UpdateIssuePayload
 issue Issue
PAYLOAD CloseIssuePayload
 issue Issue
PAYLOAD ReopenIssuePayload
 issue Issue
PAYLOAD AddCommentPayload
 commentEdge IssueCommentEdge
 subject Node
 timelineEdge TimelineEdge
PAYLOAD CreatePullRequestPayload
 pullRequest PullRequest
PAYLOAD MergePullRequestPayload
 pullRequest PullRequest
PAYLOAD ClosePullRequestPayload
 pullRequest PullRequest
INPUT CreateIssueInput
 repositoryId ID!
 title String!
 body String
 labelIds [ID!]
 assigneeIds [ID!]
 clientMutationId String
INPUT UpdateIssueInput
 id ID!
 title String
 body String
 state IssueState
 clientMutationId String
INPUT CloseIssueInput
 issueId ID!
 stateReason IssueClosedStateReason
 clientMutationId String
INPUT ReopenIssueInput
 issueId ID!
 clientMutationId String
INPUT AddCommentInput
 subjectId ID!
 body String!
 clientMutationId String
INPUT CreatePullRequestInput
 repositoryId ID!
 baseRefName String!
 headRefName String!
 title String!
 body String
 draft Boolean
 maintainerCanModify Boolean
 clientMutationId String
INPUT MergePullRequestInput
 pullRequestId ID!
 mergeMethod PullRequestMergeMethod
 commitHeadline String
 commitBody String
 expectedHeadOid GitObjectID
 clientMutationId String
INPUT ClosePullRequestInput
 pullRequestId ID!
 clientMutationId String
INPUT IssueOrder
 field IssueOrderField!
 direction OrderDirection!
INPUT IssueFilters
 states [IssueState!]
 labels [String!]
 assignee String
 createdBy String
ENUM IssueState = OPEN CLOSED
ENUM IssueStateReason = COMPLETED NOT_PLANNED REOPENED
ENUM IssueClosedStateReason = COMPLETED NOT_PLANNED
ENUM PullRequestState = OPEN CLOSED MERGED
ENUM MergeableState = MERGEABLE CONFLICTING UNKNOWN
ENUM PullRequestMergeMethod = MERGE SQUASH REBASE
ENUM RepositoryPermission = ADMIN MAINTAIN WRITE TRIAGE READ
ENUM RepositoryVisibility = PRIVATE PUBLIC INTERNAL
ENUM SearchType = ISSUE ISSUE_ADVANCED REPOSITORY USER DISCUSSION
ENUM IssueOrderField = CREATED_AT UPDATED_AT COMMENTS
ENUM OrderDirection = ASC DESC
ENUM PatchStatus = ADDED DELETED RENAMED COPIED MODIFIED CHANGED
SCALAR ID String Int Float Boolean DateTime URI GitObjectID GitSSHRemote
META __Schema
 types [__Type!]!
 queryType __Type!
 mutationType __Type
 subscriptionType __Type
 directives [__Directive!]!
 description String
META __Type
 kind __TypeKind!
 name String
 description String
 specifiedByURL String
 fields(includeDeprecated:Boolean) [__Field!]
 interfaces [__Type!]
 possibleTypes [__Type!]
 enumValues(includeDeprecated:Boolean) [__EnumValue!]
 inputFields(includeDeprecated:Boolean) [__InputValue!]
 ofType __Type
 isOneOf Boolean
META __Field
 name String!
 description String
 args(includeDeprecated:Boolean) [__InputValue!]!
 type __Type!
 isDeprecated Boolean!
 deprecationReason String
META __InputValue
 name String!
 description String
 type __Type!
 defaultValue String
 isDeprecated Boolean!
 deprecationReason String
META __EnumValue
 name String!
 description String
 isDeprecated Boolean!
 deprecationReason String
META __Directive
 name String!
 description String
 locations [String!]!
 args [__InputValue!]!
ENUM __TypeKind = SCALAR OBJECT INTERFACE UNION ENUM INPUT_OBJECT LIST NON_NULL
"#;

fn parse_field(line: &str) -> FieldDef {
    let line = line.trim();
    let (head, ty) = line.rsplit_once(' ').unwrap();
    let (name, args) = match head.split_once('(') {
        Some((n, a)) => {
            let a = a.trim_end_matches(')').replace("CONN", CONN);
            let args = a.split(',').filter(|s| !s.is_empty()).map(|kv| {
                let (k, v) = kv.split_once(':').unwrap();
                (k.to_string(), v.to_string())
            });
            (n.to_string(), args.collect())
        }
        None => (head.to_string(), vec![]),
    };
    FieldDef { name, ty: ty.to_string(), args }
}

/// Type names in declaration order (the introspection listing order), and their definitions.
pub struct Schema {
    pub order: Vec<String>,
    pub types: HashMap<String, TypeDef>,
}

pub fn schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        let mut order = Vec::new();
        let mut types: HashMap<String, TypeDef> = HashMap::new();
        let mut current: Option<String> = None;
        let mut add = |name: &str, def: TypeDef, order: &mut Vec<String>, types: &mut HashMap<String, TypeDef>| {
            order.push(name.to_string());
            types.insert(name.to_string(), def);
        };
        for line in SDL.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if line.starts_with(' ') {
                let name = current.clone().unwrap();
                types.get_mut(&name).unwrap().fields.push(parse_field(line));
                continue;
            }
            let words: Vec<&str> = line.split_whitespace().collect();
            let blank = |kind: &'static str| TypeDef { kind, fields: vec![], interfaces: vec![], possible: vec![], enums: vec![] };
            match words[0] {
                "OBJECT" | "META" | "PAYLOAD" => {
                    let mut def = blank("OBJECT");
                    if let Some(pos) = words.iter().position(|w| *w == ":") {
                        def.interfaces = words[pos + 1..].iter().map(|s| s.to_string()).collect();
                    }
                    if words[0] == "PAYLOAD" {
                        def.fields.push(parse_field("clientMutationId String"));
                    }
                    add(words[1], def, &mut order, &mut types);
                    current = Some(words[1].to_string());
                }
                "INTERFACE" | "UNION" => {
                    let mut def = blank(if words[0] == "INTERFACE" { "INTERFACE" } else { "UNION" });
                    def.possible = words[3..].iter().map(|s| s.to_string()).collect();
                    add(words[1], def, &mut order, &mut types);
                    current = Some(words[1].to_string());
                }
                "ENUM" => {
                    let mut def = blank("ENUM");
                    def.enums = words[3..].iter().map(|s| s.to_string()).collect();
                    add(words[1], def, &mut order, &mut types);
                    current = None;
                }
                "INPUT" => {
                    add(words[1], blank("INPUT_OBJECT"), &mut order, &mut types);
                    current = Some(words[1].to_string());
                }
                "SCALAR" => {
                    for w in &words[1..] {
                        add(w, blank("SCALAR"), &mut order, &mut types);
                    }
                    current = None;
                }
                "CONNECTION" => {
                    let node = words[1];
                    let (conn, edge) = (format!("{}Connection", node), format!("{}Edge", node));
                    let mut c = blank("OBJECT");
                    for f in [format!("nodes [{}]", node), format!("edges [{}]", edge), "totalCount Int!".to_string(), "pageInfo PageInfo!".to_string()] {
                        c.fields.push(parse_field(&f));
                    }
                    add(&conn, c, &mut order, &mut types);
                    let mut e = blank("OBJECT");
                    e.fields.push(parse_field(&format!("node {}", node)));
                    e.fields.push(parse_field("cursor String!"));
                    add(&edge, e, &mut order, &mut types);
                    current = None;
                }
                _ => {}
            }
        }
        Schema { order, types }
    })
}

fn named(ty: &str) -> &str {
    ty.trim_matches(|c| c == '[' || c == ']' || c == '!')
}

// ---------------------------------------------------------------------------------------------
// Runtime values

#[derive(Clone)]
enum GV {
    Null,
    S(Json),
    List(Vec<GV>),
    Root,
    Repo(u64),
    Issue(u64, u64),
    Pull(u64, u64),
    Comment(u64, Comment),
    User(String),
    Label(Label),
    Ref { repo: u64, name: String, prefix: String, oid: String },
    Commit(String),
    Obj(Vec<(String, GV)>),
    Typed(String, Box<GV>),
    MetaType(String), // a type reference: "Issue", "[Issue!]!" ...
    MetaField(String, usize),
    MetaInput(String, String),
    MetaEnum(String),
    Schema,
}

struct FieldError(String, &'static str);

fn fe<T>(msg: impl Into<String>) -> Result<T, FieldError> {
    Err(FieldError(msg.into(), "UNPROCESSABLE"))
}
fn not_found_node<T>(id: &Json) -> Result<T, FieldError> {
    Err(FieldError(format!("Could not resolve to a node with the global id of '{}'", id.as_str().map(|s| s.to_string()).unwrap_or_else(|| id.to_string())), "NOT_FOUND"))
}
fn from_forge(e: ForgeError) -> FieldError {
    FieldError(e.message, if e.status == 404 { "NOT_FOUND" } else { "UNPROCESSABLE" })
}

fn s(v: impl Into<Json>) -> GV {
    GV::S(v.into())
}

fn cursor(i: usize) -> String {
    b64encode(format!("cursor:v2:{}", i).as_bytes())
}
fn decode_cursor(c: &Json) -> Option<usize> {
    let text = String::from_utf8(b64decode(c.as_str()?)?).ok()?;
    text.strip_prefix("cursor:v2:")?.parse().ok()
}

fn connection(list: Vec<GV>, args: &Json) -> Result<GV, FieldError> {
    let mut start = 0usize;
    let mut end = list.len();
    if let Some(a) = decode_cursor(args.at("after")) {
        start = start.max(a + 1);
    }
    if let Some(b) = decode_cursor(args.at("before")) {
        end = end.min(b);
    }
    if let Some(first) = args.at("first").as_f64() {
        if first < 0.0 {
            return fe("`first` must be non-negative");
        }
        end = end.min(start + first as usize);
    }
    if let Some(last) = args.at("last").as_f64() {
        if last < 0.0 {
            return fe("`last` must be non-negative");
        }
        start = start.max(end.saturating_sub(last as usize));
    }
    let total = list.len();
    let (start, end) = (start.min(total), end.min(total).max(start.min(total)));
    let edges: Vec<GV> = (start..end).map(|i| GV::Obj(vec![("node".into(), list[i].clone()), ("cursor".into(), s(cursor(i)))])).collect();
    let nodes: Vec<GV> = list[start..end].to_vec();
    Ok(GV::Obj(vec![
        ("nodes".into(), GV::List(nodes)),
        ("edges".into(), GV::List(edges)),
        ("totalCount".into(), s(total)),
        (
            "pageInfo".into(),
            GV::Obj(vec![
                ("hasNextPage".into(), s(end < total)),
                ("hasPreviousPage".into(), s(start > 0)),
                ("startCursor".into(), if end > start { s(cursor(start)) } else { GV::Null }),
                ("endCursor".into(), if end > start { s(cursor(end - 1)) } else { GV::Null }),
            ]),
        ),
    ]))
}

fn user_gv(login: &str) -> GV {
    GV::Typed("User".into(), Box::new(GV::User(login.to_string())))
}

fn input(args: &Json) -> Result<&Json, FieldError> {
    let i = args.at("input");
    if !i.is_object() {
        return fe("input is required");
    }
    Ok(i)
}

fn node_by_id(f: &Forge, id: &Json) -> GV {
    let Some((kind, n)) = id.as_str().and_then(decode_node_id) else { return GV::Null };
    match kind.as_str() {
        "Repository" => f.repo_by_id(n).map(|r| GV::Typed("Repository".into(), Box::new(GV::Repo(r)))).unwrap_or(GV::Null),
        "Issue" => f.find_issue(n).map(|(r, num)| GV::Typed("Issue".into(), Box::new(GV::Issue(r, num)))).unwrap_or(GV::Null),
        "PullRequest" => f.find_pull(n).map(|(r, num)| GV::Typed("PullRequest".into(), Box::new(GV::Pull(r, num)))).unwrap_or(GV::Null),
        "IssueComment" => f.find_comment(n).map(|(r, c)| GV::Typed("IssueComment".into(), Box::new(GV::Comment(r, c)))).unwrap_or(GV::Null),
        "User" if n == f.viewer.id => user_gv(&f.viewer.login),
        _ => GV::Null,
    }
}

fn issue_ref(f: &Forge, id: &Json) -> Result<(u64, u64), FieldError> {
    match id.as_str().and_then(decode_node_id) {
        Some((k, n)) if k == "Issue" => f.find_issue(n).ok_or(()).or_else(|_| not_found_node(id)),
        _ => not_found_node(id),
    }
}
fn pull_ref(f: &Forge, id: &Json) -> Result<(u64, u64), FieldError> {
    match id.as_str().and_then(decode_node_id) {
        Some((k, n)) if k == "PullRequest" => f.find_pull(n).ok_or(()).or_else(|_| not_found_node(id)),
        _ => not_found_node(id),
    }
}
fn repo_ref(f: &Forge, id: &Json) -> Result<u64, FieldError> {
    match id.as_str().and_then(decode_node_id) {
        Some((k, n)) if k == "Repository" => f.repo_by_id(n).ok_or(()).or_else(|_| not_found_node(id)),
        _ => not_found_node(id),
    }
}

fn str_list(v: &Json) -> Option<Vec<String>> {
    v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
}

fn pull_state(f: &Forge, r: u64, n: u64) -> &'static str {
    let p = &f.repo(r).pulls[&n];
    if p.merged {
        "MERGED"
    } else if p.open {
        "OPEN"
    } else {
        "CLOSED"
    }
}

fn mutation_payload(args: &Json, key: &str, value: GV) -> Result<GV, FieldError> {
    let cmid = input(args).map(|i| i.at("clientMutationId").clone()).unwrap_or(Json::Null);
    Ok(GV::Obj(vec![("clientMutationId".into(), GV::S(cmid)), (key.into(), value)]))
}

fn resolve(f: &mut Forge, base: &str, parent: &GV, ty: &str, field: &str, a: &Json) -> Result<GV, FieldError> {
    match (parent, field) {
        // ---- Query
        (GV::Root, "viewer") => Ok(GV::User(f.viewer.login.clone())),
        (GV::Root, "user") => Ok(if a.at("login").as_str().map(|l| l.eq_ignore_ascii_case(&f.viewer.login)).unwrap_or(false) { GV::User(f.viewer.login.clone()) } else { GV::Null }),
        (GV::Root, "repositoryOwner") => Ok(if a.at("login").as_str().map(|l| l.eq_ignore_ascii_case(&f.viewer.login)).unwrap_or(false) { user_gv(&f.viewer.login) } else { GV::Null }),
        (GV::Root, "repository") => Ok(f.repo_id(a.at("owner").as_str().unwrap_or(""), a.at("name").as_str().unwrap_or("")).map(GV::Repo).unwrap_or(GV::Null)),
        (GV::Root, "node") => Ok(node_by_id(f, a.at("id"))),
        (GV::Root, "nodes") => Ok(GV::List(a.at("ids").as_array().map(|ids| ids.iter().map(|i| node_by_id(f, i)).collect()).unwrap_or_default())),
        (GV::Root, "rateLimit") => Ok(GV::Obj(vec![("limit".into(), s(5000)), ("remaining".into(), s(5000)), ("cost".into(), s(1)), ("used".into(), s(0)), ("resetAt".into(), s("2030-01-01T00:00:00Z")), ("nodeCount".into(), s(0))])),
        (GV::Root, "__type") => {
            let n = a.at("name").as_str().unwrap_or("");
            Ok(if schema().types.contains_key(n) { GV::MetaType(n.to_string()) } else { GV::Null })
        }
        (GV::Root, "__schema") => Ok(GV::Schema),

        // ---- Mutation
        (GV::Root, "createIssue") => {
            let i = input(a)?;
            let r = repo_ref(f, i.at("repositoryId"))?;
            let labels = match i.at("labelIds").as_array() {
                Some(ids) => {
                    let mut names = Vec::new();
                    for id in ids {
                        let decoded = id.as_str().and_then(decode_node_id);
                        match decoded.and_then(|(_, n)| f.repo(r).labels.iter().find(|l| l.id == n).map(|l| l.name.clone())) {
                            Some(n) => names.push(Json::from(n)),
                            None => return not_found_node(id),
                        }
                    }
                    Json::Arr(names)
                }
                None => Json::Null,
            };
            let n = f.create_issue(r, i.at("title"), i.at("body"), &labels).map_err(from_forge)?;
            mutation_payload(a, "issue", GV::Issue(r, n))
        }
        (GV::Root, "updateIssue") => {
            let i = input(a)?;
            let (r, n) = issue_ref(f, i.at("id"))?;
            let mut patch = Json::Obj(vec![]);
            for k in ["title", "body"] {
                if let Some(v) = i.get(k) {
                    patch.set(k, v.clone());
                }
            }
            if let Some(st) = i.at("state").as_str() {
                patch.set("state", Json::from(st.to_lowercase()));
            }
            f.update_issue(r, n, &patch).map_err(from_forge)?;
            mutation_payload(a, "issue", GV::Issue(r, n))
        }
        (GV::Root, "closeIssue") | (GV::Root, "reopenIssue") => {
            let i = input(a)?;
            let (r, n) = issue_ref(f, i.at("issueId"))?;
            let state = if field == "closeIssue" { "closed" } else { "open" };
            f.update_issue(r, n, &jobj! { "state" => state }).map_err(from_forge)?;
            mutation_payload(a, "issue", GV::Issue(r, n))
        }
        (GV::Root, "addComment") => {
            let i = input(a)?;
            let subject_id = i.at("subjectId");
            let is_pull = subject_id.as_str().and_then(decode_node_id).map(|(k, _)| k == "PullRequest").unwrap_or(false);
            let (r, n, subject) = if is_pull {
                let (r, n) = pull_ref(f, subject_id)?;
                (r, n, GV::Typed("PullRequest".into(), Box::new(GV::Pull(r, n))))
            } else {
                let (r, n) = issue_ref(f, subject_id)?;
                (r, n, GV::Typed("Issue".into(), Box::new(GV::Issue(r, n))))
            };
            let c = f.add_comment(r, n, i.at("body")).map_err(from_forge)?;
            let idx = f.repo(r).comment_count(n).saturating_sub(1);
            let node = GV::Comment(r, c);
            let edge = GV::Obj(vec![("node".into(), node.clone()), ("cursor".into(), s(cursor(idx)))]);
            let timeline = GV::Obj(vec![("node".into(), GV::Typed("IssueComment".into(), Box::new(node))), ("cursor".into(), s(cursor(idx)))]);
            Ok(GV::Obj(vec![("clientMutationId".into(), GV::S(i.at("clientMutationId").clone())), ("commentEdge".into(), edge), ("subject".into(), subject), ("timelineEdge".into(), timeline)]))
        }
        (GV::Root, "createPullRequest") => {
            let i = input(a)?;
            let r = repo_ref(f, i.at("repositoryId"))?;
            let n = f.create_pull(r, i.at("title"), i.at("body"), i.at("headRefName"), i.at("baseRefName")).map_err(from_forge)?;
            mutation_payload(a, "pullRequest", GV::Pull(r, n))
        }
        (GV::Root, "mergePullRequest") => {
            let i = input(a)?;
            let (r, n) = pull_ref(f, i.at("pullRequestId"))?;
            let m = i.at("mergeMethod");
            if !m.is_null() && !matches!(m.as_str(), Some("MERGE") | Some("SQUASH") | Some("REBASE")) {
                return fe("mergeMethod is invalid");
            }
            f.merge_pull(r, n, i.at("commitHeadline").as_str(), i.at("commitBody").as_str(), i.at("expectedHeadOid").as_str()).map_err(from_forge)?;
            mutation_payload(a, "pullRequest", GV::Pull(r, n))
        }
        (GV::Root, "closePullRequest") => {
            let i = input(a)?;
            let (r, n) = pull_ref(f, i.at("pullRequestId"))?;
            f.update_pull(r, n, &jobj! { "state" => "closed" }).map_err(from_forge)?;
            mutation_payload(a, "pullRequest", GV::Pull(r, n))
        }

        // ---- User
        (GV::User(login), _) => {
            let me = login.eq_ignore_ascii_case(&f.viewer.login);
            let id = if me { f.viewer.id } else { 0 };
            Ok(match field {
                "id" => s(node_id("User", "04", id)),
                "databaseId" => s(id),
                "login" => s(login.as_str()),
                "name" => s(if me { f.viewer.name.clone() } else { login.clone() }),
                "email" => s(if me { f.viewer.email.clone() } else { String::new() }),
                "url" => s(format!("{}/{}", base, login)),
                "avatarUrl" => s(format!("{}/{}.png", base, login)),
                "repository" => f.repo_id(login, a.at("name").as_str().unwrap_or("")).map(GV::Repo).unwrap_or(GV::Null),
                "repositories" => return connection(f.list_repos(login).into_iter().map(GV::Repo).collect(), a),
                _ => GV::Null,
            })
        }

        // ---- Repository
        (GV::Repo(id), _) => {
            let id = *id;
            let r = f.repo(id);
            Ok(match field {
                "id" => s(repo_node_id(id)),
                "databaseId" => s(id),
                "name" => s(r.name.as_str()),
                "nameWithOwner" => s(r.full_name()),
                "owner" => user_gv(&r.owner.clone()),
                "description" => s(r.description.as_str()),
                "url" => s(format!("{}/{}", base, r.full_name())),
                "sshUrl" => s(format!("git@forge:{}.git", r.full_name())),
                "isPrivate" => s(r.private),
                "visibility" => s(if r.private { "PRIVATE" } else { "PUBLIC" }),
                "isFork" | "isArchived" | "isTemplate" | "hasWikiEnabled" | "hasProjectsEnabled" => s(false),
                "isEmpty" => s(r.refs.is_empty()),
                "hasIssuesEnabled" => s(true),
                "viewerPermission" => s("ADMIN"),
                "createdAt" => s(r.created_at.as_str()),
                "updatedAt" => s(r.updated_at.as_str()),
                "pushedAt" => s(r.pushed_at.as_str()),
                "defaultBranchRef" => match f.branch_tip(id, &r.default_branch) {
                    Some(oid) => GV::Ref { repo: id, name: r.default_branch.clone(), prefix: "refs/heads/".into(), oid },
                    None => GV::Null,
                },
                "ref" => {
                    let q = a.at("qualifiedName").as_str().unwrap_or("");
                    let full = if q.starts_with("refs/") { q.to_string() } else { format!("refs/heads/{}", q) };
                    match r.refs.get(&full) {
                        Some(oid) => {
                            let slash = full[5..].find('/').map(|i| i + 5).unwrap_or(4);
                            GV::Ref { repo: id, name: full[slash + 1..].to_string(), prefix: full[..slash + 1].to_string(), oid: oid.clone() }
                        }
                        None => GV::Null,
                    }
                }
                "issue" => {
                    let n = a.at("number").as_f64().unwrap_or(0.0) as u64;
                    if r.issues.contains_key(&n) {
                        GV::Issue(id, n)
                    } else {
                        GV::Null
                    }
                }
                "issueOrPullRequest" => {
                    let n = a.at("number").as_f64().unwrap_or(0.0) as u64;
                    if r.issues.contains_key(&n) {
                        GV::Typed("Issue".into(), Box::new(GV::Issue(id, n)))
                    } else if r.pulls.contains_key(&n) {
                        GV::Typed("PullRequest".into(), Box::new(GV::Pull(id, n)))
                    } else {
                        GV::Null
                    }
                }
                "issues" => {
                    let filter = a.at("filterBy");
                    let states = str_list(a.at("states")).or_else(|| str_list(filter.at("states")));
                    let labels = str_list(a.at("labels")).or_else(|| str_list(filter.at("labels"))).unwrap_or_default();
                    let wanted: Vec<String> = labels.iter().map(|l| l.to_lowercase()).collect();
                    let mut list: Vec<&crate::store::Issue> = r
                        .issues
                        .values()
                        .filter(|i| states.as_ref().map(|s| s.is_empty() || s.iter().any(|x| x == if i.open { "OPEN" } else { "CLOSED" })).unwrap_or(true))
                        .filter(|i| wanted.is_empty() || wanted.iter().any(|w| i.labels.iter().any(|l| l.to_lowercase() == *w)))
                        .collect();
                    let order = a.at("orderBy");
                    let desc = order.at("direction").as_str() == Some("DESC");
                    let field_key = order.at("field").as_str().unwrap_or("CREATED_AT").to_string();
                    list.sort_by(|x, y| {
                        let primary = match field_key.as_str() {
                            "UPDATED_AT" => x.updated_at.cmp(&y.updated_at),
                            "COMMENTS" => r.comment_count(x.number).cmp(&r.comment_count(y.number)),
                            _ => std::cmp::Ordering::Equal,
                        };
                        let ord = primary.then(x.number.cmp(&y.number));
                        if desc {
                            ord.reverse()
                        } else {
                            ord
                        }
                    });
                    return connection(list.iter().map(|i| GV::Issue(id, i.number)).collect(), a);
                }
                "pullRequest" => {
                    let n = a.at("number").as_f64().unwrap_or(0.0) as u64;
                    if r.pulls.contains_key(&n) {
                        GV::Pull(id, n)
                    } else {
                        GV::Null
                    }
                }
                "pullRequests" => {
                    let states = str_list(a.at("states"));
                    let numbers: Vec<u64> = r.pulls.keys().copied().collect();
                    let mut list: Vec<u64> = numbers
                        .into_iter()
                        .filter(|n| states.as_ref().map(|s| s.is_empty() || s.iter().any(|x| x == pull_state(f, id, *n))).unwrap_or(true))
                        .filter(|n| a.at("headRefName").as_str().map(|h| f.repo(id).pulls[n].head == h).unwrap_or(true))
                        .filter(|n| a.at("baseRefName").as_str().map(|b| f.repo(id).pulls[n].base == b).unwrap_or(true))
                        .collect();
                    if a.at("orderBy").at("direction").as_str() == Some("DESC") {
                        list.reverse();
                    }
                    return connection(list.into_iter().map(|n| GV::Pull(id, n)).collect(), a);
                }
                "labels" => return connection(r.labels.iter().cloned().map(GV::Label).collect(), a),
                "label" => r.label(a.at("name").as_str().unwrap_or("")).cloned().map(GV::Label).unwrap_or(GV::Null),
                _ => GV::Null,
            })
        }

        // ---- Issue
        (GV::Issue(rid, n), _) => {
            let (rid, n) = (*rid, *n);
            let r = f.repo(rid);
            let i = &r.issues[&n];
            Ok(match field {
                "id" => s(issue_node_id(i.id)),
                "databaseId" => s(i.id),
                "number" => s(i.number),
                "title" => s(i.title.as_str()),
                "body" | "bodyText" => s(i.body.as_str()),
                "state" => s(if i.open { "OPEN" } else { "CLOSED" }),
                "stateReason" => if i.open { GV::Null } else { s("COMPLETED") },
                "closed" => s(!i.open),
                "closedAt" => s(i.closed_at.clone()),
                "createdAt" => s(i.created_at.as_str()),
                "updatedAt" => s(i.updated_at.as_str()),
                "url" => s(format!("{}/{}/issues/{}", base, r.full_name(), n)),
                "author" => user_gv(&i.user.clone()),
                "repository" => GV::Repo(rid),
                "locked" => s(false),
                "labels" => return connection(i.labels.iter().filter_map(|l| r.label(l)).cloned().map(GV::Label).collect(), a),
                "comments" => return connection(r.comments.get(&n).cloned().unwrap_or_default().into_iter().map(|c| GV::Comment(rid, c)).collect(), a),
                "assignees" => return connection(vec![], a),
                _ => GV::Null,
            })
        }

        // ---- PullRequest
        (GV::Pull(rid, n), _) => {
            let (rid, n) = (*rid, *n);
            match field {
                "headRefOid" => return Ok(s(f.pull_shas(rid, n).0)),
                "baseRefOid" => return Ok(s(f.pull_shas(rid, n).1)),
                "mergeable" => {
                    return Ok(s(match f.mergeable(rid, n) {
                        None => "UNKNOWN",
                        Some(true) => "MERGEABLE",
                        Some(false) => "CONFLICTING",
                    }))
                }
                "files" | "changedFiles" | "additions" | "deletions" => {
                    let files = f.pull_files(rid, n);
                    return match field {
                        "files" => connection(
                            files
                                .into_iter()
                                .map(|x| {
                                    GV::Obj(vec![
                                        ("path".into(), s(x.path)),
                                        ("additions".into(), s(x.additions)),
                                        ("deletions".into(), s(x.deletions)),
                                        ("changeType".into(), s(match x.status {
                                            "added" => "ADDED",
                                            "removed" => "DELETED",
                                            _ => "MODIFIED",
                                        })),
                                    ])
                                })
                                .collect(),
                            a,
                        ),
                        "changedFiles" => Ok(s(files.len())),
                        "additions" => Ok(s(files.iter().map(|x| x.additions).sum::<usize>())),
                        _ => Ok(s(files.iter().map(|x| x.deletions).sum::<usize>())),
                    };
                }
                _ => {}
            }
            let state = pull_state(f, rid, n);
            let r = f.repo(rid);
            let p = &r.pulls[&n];
            Ok(match field {
                "id" => s(pull_node_id(p.id)),
                "databaseId" => s(p.id),
                "number" => s(p.number),
                "title" => s(p.title.as_str()),
                "body" => s(p.body.as_str()),
                "state" => s(state),
                "merged" => s(p.merged),
                "mergedAt" => s(p.merged_at.clone()),
                "closed" => s(!p.open),
                "closedAt" => s(p.closed_at.clone()),
                "isDraft" => s(false),
                "createdAt" => s(p.created_at.as_str()),
                "updatedAt" => s(p.updated_at.as_str()),
                "url" => s(format!("{}/{}/pull/{}", base, r.full_name(), n)),
                "headRefName" => s(p.head.as_str()),
                "baseRefName" => s(p.base.as_str()),
                "headRepository" | "baseRepository" | "repository" => GV::Repo(rid),
                "mergeCommit" => p.merge_commit_sha.clone().map(GV::Commit).unwrap_or(GV::Null),
                "author" => user_gv(&p.user.clone()),
                "comments" => return connection(r.comments.get(&n).cloned().unwrap_or_default().into_iter().map(|c| GV::Comment(rid, c)).collect(), a),
                "labels" => return connection(vec![], a),
                _ => GV::Null,
            })
        }

        // ---- IssueComment, Label, Ref, Commit
        (GV::Comment(rid, c), _) => Ok(match field {
            "id" => s(comment_node_id(c.id)),
            "databaseId" => s(c.id),
            "body" => s(c.body.as_str()),
            "createdAt" => s(c.created_at.as_str()),
            "updatedAt" => s(c.updated_at.as_str()),
            "url" => s(format!("{}/{}/issues/{}#issuecomment-{}", base, f.repo(*rid).full_name(), c.issue_number, c.id)),
            "author" => user_gv(&c.user),
            _ => GV::Null,
        }),
        (GV::Label(l), _) => Ok(match field {
            "id" => s(label_node_id(l.id)),
            "name" => s(l.name.as_str()),
            "color" => s(l.color.as_str()),
            "description" => s(l.description.as_str()),
            _ => GV::Null,
        }),
        (GV::Ref { repo, name, prefix, oid }, _) => Ok(match field {
            "id" => s(b64encode(format!("03:Ref{}:{}{}", repo, prefix, name).as_bytes())),
            "name" => s(name.as_str()),
            "prefix" => s(prefix.as_str()),
            "target" => GV::Commit(oid.clone()),
            _ => GV::Null,
        }),
        (GV::Commit(oid), _) => {
            let c = f.objects.commit(oid);
            Ok(match field {
                "oid" => s(oid.as_str()),
                "abbreviatedOid" => s(&oid[..7.min(oid.len())]),
                "message" => s(c.map(|c| c.message.clone()).unwrap_or_default()),
                "committedDate" => s(iso(c.map(|c| c.committer.time).unwrap_or(0))),
                _ => GV::Null,
            })
        }

        // ---- generic objects (connections, payloads, edges)
        (GV::Obj(fields), _) => Ok(fields.iter().find(|(k, _)| k == field).map(|(_, v)| v.clone()).unwrap_or(GV::Null)),

        // ---- introspection
        (GV::Schema, _) => Ok(match field {
            "types" => GV::List(schema().order.iter().map(|n| GV::MetaType(n.clone())).collect()),
            "queryType" => GV::MetaType("Query".into()),
            "mutationType" => GV::MetaType("Mutation".into()),
            "directives" => GV::List(vec![]),
            _ => GV::Null,
        }),
        (GV::MetaType(tref), _) => Ok(meta_type(tref, field)),
        (GV::MetaField(t, idx), _) => {
            let def = &schema().types[t].fields[*idx];
            Ok(match field {
                "name" => s(def.name.as_str()),
                "args" => GV::List(def.args.iter().map(|(n, ty)| GV::MetaInput(n.clone(), ty.clone())).collect()),
                "type" => GV::MetaType(def.ty.clone()),
                "isDeprecated" => s(false),
                _ => GV::Null,
            })
        }
        (GV::MetaInput(n, ty), _) => Ok(match field {
            "name" => s(n.as_str()),
            "type" => GV::MetaType(ty.clone()),
            "isDeprecated" => s(false),
            _ => GV::Null,
        }),
        (GV::MetaEnum(n), _) => Ok(match field {
            "name" => s(n.as_str()),
            "isDeprecated" => s(false),
            _ => GV::Null,
        }),
        _ => {
            let _ = ty;
            Ok(GV::Null)
        }
    }
}

fn meta_type(tref: &str, field: &str) -> GV {
    if let Some(inner) = tref.strip_suffix('!') {
        return match field {
            "kind" => s("NON_NULL"),
            "ofType" => GV::MetaType(inner.to_string()),
            _ => GV::Null,
        };
    }
    if tref.starts_with('[') {
        return match field {
            "kind" => s("LIST"),
            "ofType" => GV::MetaType(tref[1..tref.len() - 1].to_string()),
            _ => GV::Null,
        };
    }
    let Some(def) = schema().types.get(tref) else { return GV::Null };
    match field {
        "kind" => s(def.kind),
        "name" => s(tref),
        "fields" if def.kind == "OBJECT" || def.kind == "INTERFACE" => GV::List((0..def.fields.len()).map(|i| GV::MetaField(tref.to_string(), i)).collect()),
        "interfaces" if def.kind == "OBJECT" => GV::List(def.interfaces.iter().map(|n| GV::MetaType(n.clone())).collect()),
        "possibleTypes" if def.kind == "INTERFACE" || def.kind == "UNION" => GV::List(def.possible.iter().map(|n| GV::MetaType(n.clone())).collect()),
        "enumValues" if def.kind == "ENUM" => GV::List(def.enums.iter().map(|n| GV::MetaEnum(n.clone())).collect()),
        "inputFields" if def.kind == "INPUT_OBJECT" => GV::List(def.fields.iter().map(|fd| GV::MetaInput(fd.name.clone(), fd.ty.clone())).collect()),
        "isOneOf" => s(false),
        _ => GV::Null,
    }
}

// ---------------------------------------------------------------------------------------------
// Execution

struct Exec<'a> {
    f: &'a mut Forge,
    base: &'a str,
    doc: &'a Document,
    vars: Json,
    errors: Vec<Json>,
}

struct Validation(String);

fn skipped(vars: &Json, directives: &[Directive]) -> bool {
    for d in directives {
        let cond = d.args.iter().find(|(k, _)| k == "if").map(|(_, v)| value_of(v, vars));
        if d.name == "skip" && cond == Some(Json::Bool(true)) {
            return true;
        }
        if d.name == "include" && cond == Some(Json::Bool(false)) {
            return true;
        }
    }
    false
}

fn type_matches(cond: Option<&str>, runtime: &str) -> bool {
    match cond {
        None => true,
        Some(c) if c == runtime => true,
        Some(c) => schema().types.get(c).map(|d| (d.kind == "INTERFACE" || d.kind == "UNION") && d.possible.iter().any(|p| p == runtime)).unwrap_or(false),
    }
}

type Collected<'a> = Vec<(String, Vec<&'a Selection>)>;

fn collect<'a>(doc: &'a Document, vars: &Json, runtime: &str, sels: &'a [Selection], out: &mut Collected<'a>, visited: &mut HashSet<String>) -> Result<(), Validation> {
    for sel in sels {
        match sel {
            Selection::Field { alias, name, directives, .. } => {
                if skipped(vars, directives) {
                    continue;
                }
                let key = alias.clone().unwrap_or_else(|| name.clone());
                match out.iter_mut().find(|(k, _)| *k == key) {
                    Some(slot) => slot.1.push(sel),
                    None => out.push((key, vec![sel])),
                }
            }
            Selection::Inline { type_condition, directives, selections } => {
                if skipped(vars, directives) {
                    continue;
                }
                if let Some(tc) = type_condition {
                    if !schema().types.contains_key(tc) {
                        return Err(Validation(format!("Fragment on unknown type '{}'", tc)));
                    }
                }
                if type_matches(type_condition.as_deref(), runtime) {
                    collect(doc, vars, runtime, selections, out, visited)?;
                }
            }
            Selection::Spread { name, directives } => {
                if skipped(vars, directives) {
                    continue;
                }
                let Some(frag) = doc.fragments.get(name) else { return Err(Validation(format!("Fragment {} was used, but not defined", name))) };
                if !visited.insert(name.clone()) {
                    continue;
                }
                if type_matches(Some(&frag.type_condition), runtime) {
                    collect(doc, vars, runtime, &frag.selections, out, visited)?;
                }
            }
        }
    }
    Ok(())
}

fn execute(e: &mut Exec, type_name: &str, parent: &GV, sels: &[Selection], path: &mut Vec<Json>) -> Result<Json, Validation> {
    let def = schema().types.get(type_name).ok_or_else(|| Validation(format!("unknown type {}", type_name)))?;
    let doc = e.doc;
    let mut collected: Collected = Vec::new();
    collect(doc, &e.vars, type_name, sels, &mut collected, &mut HashSet::new())?;
    let mut out: Vec<(String, Json)> = Vec::new();
    for (key, fields) in collected {
        let Selection::Field { name, args, .. } = fields[0] else { continue };
        if name == "__typename" {
            out.push((key, Json::from(type_name)));
            continue;
        }
        let Some(fdef) = def.fields.iter().find(|fd| fd.name == *name) else {
            return Err(Validation(format!("Field '{}' doesn't exist on type '{}'", name, type_name)));
        };
        for (an, _) in args {
            if !fdef.args.iter().any(|(k, _)| k == an) {
                return Err(Validation(format!("Field '{}' doesn't accept argument '{}'", name, an)));
            }
        }
        let arg_json = Json::Obj(args.iter().map(|(k, v)| (k.clone(), value_of(v, &e.vars))).collect());
        let merged: Vec<Selection> = fields.iter().flat_map(|f| match f {
            Selection::Field { selections: Some(s), .. } => s.clone(),
            _ => vec![],
        }).collect();
        path.push(Json::from(key.as_str()));
        let value = match resolve(e.f, e.base, parent, type_name, name, &arg_json) {
            Ok(v) => complete(e, &fdef.ty, v, &merged, path, name)?,
            Err(FieldError(msg, kind)) => {
                e.errors.push(jobj! { "type" => kind, "path" => Json::Arr(path.clone()), "locations" => Json::Arr(vec![]), "message" => msg });
                Json::Null
            }
        };
        path.pop();
        out.push((key, value));
    }
    Ok(Json::Obj(out))
}

fn complete(e: &mut Exec, ty: &str, v: GV, sels: &[Selection], path: &mut Vec<Json>, field: &str) -> Result<Json, Validation> {
    if matches!(v, GV::Null) {
        return Ok(Json::Null);
    }
    let t = ty.strip_suffix('!').unwrap_or(ty);
    if t.starts_with('[') {
        let inner = &t[1..t.len() - 1];
        let GV::List(items) = v else { return Ok(Json::Null) };
        let mut out = Vec::with_capacity(items.len());
        for (i, item) in items.into_iter().enumerate() {
            path.push(Json::from(i));
            out.push(complete(e, inner, item, sels, path, field)?);
            path.pop();
        }
        return Ok(Json::Arr(out));
    }
    let Some(def) = schema().types.get(t) else { return Ok(Json::Null) };
    match def.kind {
        "SCALAR" | "ENUM" => {
            if !sels.is_empty() {
                return Err(Validation(format!("Selections can't be made on scalars (field '{}' returns {})", field, t)));
            }
            Ok(match v {
                GV::S(j) => j,
                _ => Json::Null,
            })
        }
        _ => {
            if sels.is_empty() {
                return Err(Validation(format!("Field must have selections (field '{}' returns {} but has no selections)", field, t)));
            }
            match (def.kind, v) {
                ("INTERFACE" | "UNION", GV::Typed(runtime, inner)) => execute(e, &runtime, &inner, sels, path),
                ("INTERFACE" | "UNION", _) => Ok(Json::Null),
                (_, GV::Typed(_, inner)) => execute(e, t, &inner, sels, path),
                (_, other) => execute(e, t, &other, sels, path),
            }
        }
    }
}

fn err_only(msg: &str) -> Json {
    jobj! { "errors" => Json::Arr(vec![jobj! { "message" => msg, "locations" => Json::Arr(vec![]) }]) }
}

pub fn execute_request(f: &mut Forge, base: &str, request: &Json) -> Json {
    if !request.is_object() {
        return err_only("A query attribute must be specified and must be a string.");
    }
    let query = match request.at("query").as_str() {
        Some(q) if !q.trim().is_empty() => q,
        _ => return err_only("A query attribute must be specified and must be a string."),
    };
    let doc = match parse_document(query) {
        Ok(d) => d,
        Err(e) => return err_only(&e),
    };
    let op = match request.at("operationName").as_str().filter(|s| !s.is_empty()) {
        Some(name) => match doc.operations.iter().find(|o| o.name.as_deref() == Some(name)) {
            Some(o) => o,
            None => return err_only(&format!("No operation named \"{}\"", name)),
        },
        None if doc.operations.len() == 1 => &doc.operations[0],
        None => return err_only("An operation name is required when the document holds several operations"),
    };
    if op.kind == "subscription" {
        return err_only("Subscriptions are not supported");
    }
    // variables
    let given = request.at("variables");
    let mut vars: Vec<(String, Json)> = Vec::new();
    for v in &op.vars {
        let value = match given.get(&v.name) {
            Some(x) => x.clone(),
            None => match &v.default {
                Some(d) => value_of(d, &Json::Null),
                None if v.type_ref.ends_with('!') => return jobj! { "errors" => Json::Arr(vec![jobj! { "message" => format!("Variable ${} of type {} was provided invalid value", v.name, v.type_ref), "locations" => Json::Arr(vec![]) }]) },
                None => Json::Null,
            },
        };
        if !schema().types.contains_key(named(&v.type_ref)) {
            return err_only(&format!("{} isn't a defined input type (on ${})", named(&v.type_ref), v.name));
        }
        vars.push((v.name.clone(), value));
    }
    let root = if op.kind == "mutation" { "Mutation" } else { "Query" };
    let mut e = Exec { f, base, doc: &doc, vars: Json::Obj(vars), errors: Vec::new() };
    let result = execute(&mut e, root, &GV::Root, &op.selections, &mut Vec::new());
    match result {
        Ok(data) => {
            if e.errors.is_empty() {
                jobj! { "data" => data }
            } else {
                jobj! { "data" => data, "errors" => Json::Arr(e.errors) }
            }
        }
        Err(Validation(msg)) => jobj! { "errors" => Json::Arr(vec![jobj! { "message" => msg, "locations" => Json::Arr(vec![]), "extensions" => jobj! { "code" => "undefinedField" } }]) },
    }
}

pub fn handle(f: &mut Forge, body: &[u8], base: &str) -> crate::http::Response {
    let parsed = match json::parse_bytes(body) {
        Ok(v) => v,
        Err(_) => {
            return crate::http::Response::json(400, &jobj! { "message" => "Problems parsing JSON", "errors" => Json::Arr(vec![jobj! { "message" => "Problems parsing JSON" }]) });
        }
    };
    let out = match &parsed {
        Json::Arr(items) => Json::Arr(items.iter().map(|r| execute_request(f, base, r)).collect()),
        other => execute_request(f, base, other),
    };
    crate::http::Response::json(200, &out)
}
