// REST v3 representations of the store's objects. MCP tool results reuse them, so an object read
// over either surface carries the same ids and fields.

import type { Forge, Issue, Label, Pull, Repo, Comment } from '../store.ts';
import type { Oid } from '../git/objects.ts';

export function userJson(forge: Forge, base: string, login: string): Record<string, unknown> {
  const id = login.toLowerCase() === forge.viewer.login.toLowerCase() ? forge.viewer.id : 0;
  return {
    login,
    id,
    node_id: Buffer.from(`04:User${id}`).toString('base64'),
    type: 'User',
    site_admin: false,
    url: `${base}/api/v3/users/${login}`,
    html_url: `${base}/${login}`,
  };
}

export function repoJson(forge: Forge, base: string, repo: Repo): Record<string, unknown> {
  const full = `${repo.owner}/${repo.name}`;
  const api = `${base}/api/v3/repos/${full}`;
  return {
    id: repo.id,
    node_id: repoNodeId(repo.id),
    name: repo.name,
    full_name: full,
    private: repo.private,
    visibility: repo.private ? 'private' : 'public',
    owner: userJson(forge, base, repo.owner),
    html_url: `${base}/${full}`,
    description: repo.description,
    fork: false,
    url: api,
    clone_url: `${base}/${full}.git`,
    git_url: `${base}/${full}.git`,
    default_branch: repo.defaultBranch,
    created_at: repo.createdAt,
    updated_at: repo.updatedAt,
    pushed_at: repo.pushedAt,
    size: 0,
    archived: false,
    disabled: false,
    has_issues: true,
    has_projects: false,
    has_wiki: false,
    open_issues_count: [...repo.issues.values()].filter((i) => i.state === 'open').length,
    permissions: { admin: true, maintain: true, push: true, triage: true, pull: true },
  };
}

export function labelJson(base: string, repo: Repo, label: Label): Record<string, unknown> {
  return {
    id: label.id,
    node_id: Buffer.from(`010:Label${label.id}`).toString('base64'),
    url: `${base}/api/v3/repos/${repo.owner}/${repo.name}/labels/${encodeURIComponent(label.name)}`,
    name: label.name,
    color: label.color,
    description: label.description,
    default: false,
  };
}

export function issueJson(forge: Forge, base: string, repo: Repo, issue: Issue): Record<string, unknown> {
  const api = `${base}/api/v3/repos/${repo.owner}/${repo.name}`;
  return {
    id: issue.id,
    node_id: issueNodeId(issue.id),
    url: `${api}/issues/${issue.number}`,
    repository_url: api,
    html_url: `${base}/${repo.owner}/${repo.name}/issues/${issue.number}`,
    number: issue.number,
    title: issue.title,
    body: issue.body,
    state: issue.state,
    state_reason: issue.state === 'closed' ? 'completed' : null,
    user: userJson(forge, base, issue.user),
    labels: issue.labels.map((name) => {
      const label = repo.labels.get(name.toLowerCase());
      return label === undefined ? { name } : labelJson(base, repo, label);
    }),
    assignee: null,
    assignees: [],
    milestone: null,
    locked: false,
    comments: (repo.comments.get(issue.number) ?? []).length,
    created_at: issue.createdAt,
    updated_at: issue.updatedAt,
    closed_at: issue.closedAt,
    author_association: 'OWNER',
  };
}

export function commentJson(forge: Forge, base: string, repo: Repo, comment: Comment): Record<string, unknown> {
  const api = `${base}/api/v3/repos/${repo.owner}/${repo.name}`;
  return {
    id: comment.id,
    node_id: Buffer.from(`012:IssueComment${comment.id}`).toString('base64'),
    url: `${api}/issues/comments/${comment.id}`,
    issue_url: `${api}/issues/${comment.issueNumber}`,
    html_url: `${base}/${repo.owner}/${repo.name}/issues/${comment.issueNumber}#issuecomment-${comment.id}`,
    body: comment.body,
    user: userJson(forge, base, comment.user),
    created_at: comment.createdAt,
    updated_at: comment.updatedAt,
    author_association: 'OWNER',
  };
}

export function pullJson(forge: Forge, base: string, repo: Repo, pull: Pull): Record<string, unknown> {
  const api = `${base}/api/v3/repos/${repo.owner}/${repo.name}`;
  const shas = forge.pullShas(repo, pull);
  const repoView = repoJson(forge, base, repo);
  const mergeable = pull.state === 'open' ? forge.mergeResult(repo, pull) !== null : null;
  return {
    id: pull.id,
    node_id: pullNodeId(pull.id),
    url: `${api}/pulls/${pull.number}`,
    html_url: `${base}/${repo.owner}/${repo.name}/pull/${pull.number}`,
    issue_url: `${api}/issues/${pull.number}`,
    number: pull.number,
    state: pull.state,
    locked: false,
    title: pull.title,
    body: pull.body,
    user: userJson(forge, base, pull.user),
    labels: [],
    created_at: pull.createdAt,
    updated_at: pull.updatedAt,
    closed_at: pull.closedAt,
    merged_at: pull.mergedAt,
    merge_commit_sha: pull.mergeCommitSha,
    head: { label: `${repo.owner}:${pull.head}`, ref: pull.head, sha: shas.head, user: userJson(forge, base, repo.owner), repo: repoView },
    base: { label: `${repo.owner}:${pull.base}`, ref: pull.base, sha: shas.base, user: userJson(forge, base, repo.owner), repo: repoView },
    draft: false,
    merged: pull.merged,
    mergeable,
    mergeable_state: pull.state === 'open' ? (mergeable === true ? 'clean' : 'dirty') : 'unknown',
    merged_by: pull.merged ? userJson(forge, base, forge.viewer.login) : null,
    comments: (repo.comments.get(pull.number) ?? []).length,
    commits: 1,
    author_association: 'OWNER',
  };
}

export function commitJson(forge: Forge, base: string, repo: Repo, oid: Oid): Record<string, unknown> | null {
  const c = forge.objects.commit(oid);
  if (c === null) return null;
  const api = `${base}/api/v3/repos/${repo.owner}/${repo.name}`;
  const date = (t: number): string => new Date(t * 1000).toISOString().replace(/\.\d{3}Z$/, 'Z');
  return {
    sha: oid,
    node_id: Buffer.from(`06:Commit${oid}`).toString('base64'),
    url: `${api}/commits/${oid}`,
    html_url: `${base}/${repo.owner}/${repo.name}/commit/${oid}`,
    commit: {
      message: c.message.replace(/\n$/, ''),
      author: { name: c.author.name, email: c.author.email, date: date(c.author.time) },
      committer: { name: c.committer.name, email: c.committer.email, date: date(c.committer.time) },
      tree: { sha: c.tree, url: `${api}/git/trees/${c.tree}` },
      url: `${api}/git/commits/${oid}`,
    },
    author: null,
    committer: null,
    parents: c.parents.map((p) => ({ sha: p, url: `${api}/commits/${p}` })),
  };
}

// Relay-style node ids: stable per object kind and database id.
export function repoNodeId(id: number): string {
  return Buffer.from(`010:Repository${id}`).toString('base64');
}
export function issueNodeId(id: number): string {
  return Buffer.from(`05:Issue${id}`).toString('base64');
}
export function pullNodeId(id: number): string {
  return Buffer.from(`011:PullRequest${id}`).toString('base64');
}
export function decodeNodeId(id: string): { kind: string; id: number } | null {
  let text: string;
  try {
    text = Buffer.from(id, 'base64').toString('utf8');
  } catch {
    return null;
  }
  const m = /^\d+:([A-Za-z]+)(\d+)$/.exec(text);
  if (m === null) return null;
  return { kind: m[1] ?? '', id: Number(m[2]) };
}
