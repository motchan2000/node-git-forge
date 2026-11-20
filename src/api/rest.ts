// GitHub REST v3 under /api/v3. The caller has already authenticated the request; this module
// routes it, calls the store, and writes the GitHub-shaped JSON reply.

import type { Req as IncomingMessage, Res as ServerResponse } from '../server.ts';
import { Forge, ForgeError, type Issue, type Repo } from '../store.ts';
import { sendJson, sendJsonText, parseJson, readBody } from '../http.ts';
import { commentJson, commitJson, issueJson, labelJson, pullJson, repoJson, userJson } from './present.ts';
import { ZERO_OID } from '../git/objects.ts';

type Json = Record<string, unknown>;

interface Ctx {
  readonly forge: Forge;
  readonly req: IncomingMessage;
  readonly res: ServerResponse;
  readonly method: string;
  readonly segs: readonly string[]; // decoded path segments after /api/v3
  readonly query: URLSearchParams;
  readonly base: string; // origin
  readonly pathname: string;
}

async function jsonBody(ctx: Ctx): Promise<Json> {
  const raw = await readBody(ctx.req);
  const parsed = parseJson(raw);
  if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) throw new ForgeError(400, 'Body should be a JSON object');
  return parsed as Json;
}

function intParam(q: URLSearchParams, name: string, fallback: number, min: number, max: number): number {
  const raw = q.get(name);
  if (raw === null || raw === '') return fallback;
  const n = Number(raw);
  if (!Number.isInteger(n)) return fallback;
  return Math.min(max, Math.max(min, n));
}

/** Slice a list by per_page/page and answer it with a Link header when more pages exist. */
function sendPage(ctx: Ctx, items: readonly unknown[], status = 200): void {
  const perPage = intParam(ctx.query, 'per_page', 30, 1, 100);
  const page = intParam(ctx.query, 'page', 1, 1, 1_000_000);
  const start = (page - 1) * perPage;
  const slice = items.slice(start, start + perPage);
  const last = Math.max(1, Math.ceil(items.length / perPage));
  const headers: Record<string, string> = {};
  const link = (p: number): string => {
    const q = new URLSearchParams(ctx.query);
    q.set('per_page', String(perPage));
    q.set('page', String(p));
    return `<${ctx.base}${ctx.pathname}?${q.toString()}>`;
  };
  const rels: string[] = [];
  if (page < last) rels.push(`${link(page + 1)}; rel="next"`, `${link(last)}; rel="last"`);
  if (page > 1) rels.push(`${link(1)}; rel="first"`, `${link(page - 1)}; rel="prev"`);
  if (rels.length > 0) headers.link = rels.join(', ');
  sendJson(ctx.res, status, slice, headers);
}

function issueNumberOf(seg: string | undefined): number {
  const n = Number(seg);
  if (!Number.isInteger(n) || n < 1) throw new ForgeError(404, 'Not Found');
  return n;
}

const SHA_RE = /^[0-9a-f]{40}$/i;

// ---------------------------------------------------------------------------------------------

export async function handleRest(forge: Forge, req: IncomingMessage, res: ServerResponse, url: URL, base: string): Promise<void> {
  const rest = url.pathname.slice('/api/v3'.length);
  const segs = rest.split('/').filter((s) => s !== '').map((s) => {
    try {
      return decodeURIComponent(s);
    } catch {
      return s;
    }
  });
  const ctx: Ctx = { forge, req, res, method: req.method ?? 'GET', segs, query: url.searchParams, base, pathname: url.pathname };
  await route(ctx);
}

async function route(ctx: Ctx): Promise<void> {
  const { segs, method, forge, res, base } = ctx;
  const s0 = segs[0];

  if (segs.length === 0 && method === 'GET') {
    sendJson(res, 200, {
      current_user_url: `${base}/api/v3/user`,
      repository_url: `${base}/api/v3/repos/{owner}/{repo}`,
      rate_limit_url: `${base}/api/v3/rate_limit`,
    }, { 'x-oauth-scopes': 'repo, read:org, workflow', 'x-accepted-oauth-scopes': '' });
    return;
  }
  if (s0 === 'meta' && segs.length === 1 && method === 'GET') {
    sendJson(res, 200, { verifiable_password_authentication: false, installed_version: '3.14.0' });
    return;
  }
  if (s0 === 'rate_limit' && segs.length === 1 && method === 'GET') {
    const core = { limit: 5000, remaining: 5000, reset: 0, used: 0 };
    sendJson(res, 200, { resources: { core, graphql: core }, rate: core });
    return;
  }
  if (s0 === 'user' && segs.length === 1 && method === 'GET') {
    sendJson(res, 200, { ...userJson(forge, base, forge.viewer.login), name: forge.viewer.name, email: forge.viewer.email });
    return;
  }
  if (s0 === 'user' && segs[1] === 'repos' && segs.length === 2) {
    if (method === 'GET') {
      sendPage(ctx, forge.listRepos(forge.viewer.login).map((r) => repoJson(forge, base, r)));
      return;
    }
    if (method === 'POST') {
      const body = await jsonBody(ctx);
      const repo = forge.createRepo(body as { name: unknown });
      if (body.auto_init === true) forge.commitFiles(repo, repo.defaultBranch, [{ path: 'README.md', content: Buffer.from(`# ${repo.name}\n`) }], 'Initial commit\n');
      sendJson(res, 201, repoJson(forge, base, repo));
      return;
    }
  }
  if (s0 === 'users' && segs.length >= 2) {
    const login = segs[1] as string;
    if (segs.length === 2 && method === 'GET') {
      if (login.toLowerCase() !== forge.viewer.login.toLowerCase()) throw new ForgeError(404, 'Not Found');
      sendJson(res, 200, userJson(forge, base, forge.viewer.login));
      return;
    }
    if (segs.length === 3 && segs[2] === 'repos' && method === 'GET') {
      sendPage(ctx, forge.listRepos(login).map((r) => repoJson(forge, base, r)));
      return;
    }
  }
  if (s0 === 'repos' && segs.length >= 3) {
    const repo = forge.requireRepo(segs[1] as string, segs[2] as string);
    await routeRepo(ctx, repo, segs.slice(3));
    return;
  }
  throw new ForgeError(404, 'Not Found');
}

async function routeRepo(ctx: Ctx, repo: Repo, tail: readonly string[]): Promise<void> {
  const { method, forge, res, base } = ctx;
  const t0 = tail[0];

  if (tail.length === 0) {
    if (method === 'GET') return sendJson(res, 200, repoJson(forge, base, repo));
    if (method === 'PATCH') {
      const body = await jsonBody(ctx);
      if (typeof body.name === 'string' && body.name !== repo.name) throw new ForgeError(422, 'renaming is not supported');
      forge.updateRepo(repo, body);
      return sendJson(res, 200, repoJson(forge, base, repo));
    }
    if (method === 'DELETE') {
      forge.deleteRepo(repo);
      return sendJson(res, 204, null);
    }
    throw new ForgeError(404, 'Not Found');
  }

  switch (t0) {
    case 'branches':
      return branches(ctx, repo, tail);
    case 'commits':
      return commits(ctx, repo, tail);
    case 'git':
      return gitData(ctx, repo, tail);
    case 'contents':
      return contents(ctx, repo, tail.slice(1).join('/'));
    case 'readme': {
      if (method !== 'GET' || tail.length !== 1) break;
      return contents(ctx, repo, 'README.md');
    }
    case 'labels':
      return labels(ctx, repo, tail);
    case 'issues':
      return issues(ctx, repo, tail);
    case 'pulls':
      return pulls(ctx, repo, tail);
    case 'tags':
      if (method === 'GET' && tail.length === 1) {
        const list = [...repo.refs.entries()].filter(([r]) => r.startsWith('refs/tags/')).sort(([a], [b]) => (a < b ? -1 : 1));
        return sendPage(ctx, list.map(([r, oid]) => ({ name: r.slice(10), commit: { sha: forge.peelToCommit(oid) ?? oid } })));
      }
      break;
    default:
      break;
  }
  throw new ForgeError(404, 'Not Found');
}

// ---- branches, commits, refs -------------------------------------------------------------------

function branchJson(ctx: Ctx, repo: Repo, name: string, oid: string): Json {
  return {
    name,
    commit: { sha: oid, url: `${ctx.base}/api/v3/repos/${repo.owner}/${repo.name}/commits/${oid}` },
    protected: false,
  };
}

function branches(ctx: Ctx, repo: Repo, tail: readonly string[]): void {
  if (ctx.method !== 'GET') throw new ForgeError(404, 'Not Found');
  if (tail.length === 1) {
    const list = [...repo.refs.entries()].filter(([r]) => r.startsWith('refs/heads/')).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
    return sendPage(ctx, list.map(([r, oid]) => branchJson(ctx, repo, r.slice(11), oid)));
  }
  const name = tail.slice(1).join('/');
  const oid = ctx.forge.branchTip(repo, name);
  if (oid === null) throw new ForgeError(404, 'Branch not found');
  const full = { ...branchJson(ctx, repo, name, oid), commit: commitJson(ctx.forge, ctx.base, repo, oid) };
  sendJson(ctx.res, 200, full);
}

function commits(ctx: Ctx, repo: Repo, tail: readonly string[]): void {
  const { forge } = ctx;
  if (ctx.method !== 'GET') throw new ForgeError(404, 'Not Found');
  if (tail.length === 1) {
    const ref = ctx.query.get('sha') ?? undefined;
    if (repo.refs.size === 0) throw new ForgeError(409, 'Git Repository is empty.');
    const tip = forge.resolve(repo, ref);
    if (tip === null) throw new ForgeError(404, 'No commit found for SHA: ' + (ref ?? ''));
    const perPage = intParam(ctx.query, 'per_page', 30, 1, 100);
    const page = intParam(ctx.query, 'page', 1, 1, 1_000_000);
    let list = forge.history(tip, perPage * page + 1);
    const path = ctx.query.get('path');
    if (path !== null && path !== '') {
      list = forge.history(tip).filter((oid) => {
        const c = forge.objects.commit(oid);
        const here = forge.lookup(oid, path);
        const parent = c !== null && c.parents.length > 0 ? forge.lookup(c.parents[0] as string, path) : null;
        return (here?.oid ?? null) !== (parent?.oid ?? null);
      });
    }
    return sendPage(ctx, list.map((oid) => commitJson(forge, ctx.base, repo, oid)));
  }
  const ref = tail.slice(1).join('/');
  const oid = forge.resolve(repo, ref);
  if (oid === null) throw new ForgeError(404, `No commit found for SHA: ${ref}`);
  const body = commitJson(forge, ctx.base, repo, oid) as Json;
  const c = forge.objects.commit(oid);
  if (c !== null) {
    const before = c.parents.length > 0 ? forge.flatAt(c.parents[0] as string) : new Map();
    const after = forge.flatAt(oid);
    const files: Json[] = [];
    for (const [path, entry] of after) {
      const old = before.get(path);
      if (old === undefined) files.push({ filename: path, status: 'added', sha: entry.oid });
      else if (old.oid !== entry.oid) files.push({ filename: path, status: 'modified', sha: entry.oid });
    }
    for (const [path, entry] of before) if (!after.has(path)) files.push({ filename: path, status: 'removed', sha: entry.oid });
    body.files = files;
  }
  sendJson(ctx.res, 200, body);
}

function refJson(ctx: Ctx, repo: Repo, ref: string, oid: string): Json {
  const type = ctx.forge.objects.get(oid)?.type ?? 'commit';
  return {
    ref,
    node_id: Buffer.from(`03:Ref${repo.id}:${ref}`).toString('base64'),
    url: `${ctx.base}/api/v3/repos/${repo.owner}/${repo.name}/git/${ref}`,
    object: { sha: oid, type, url: `${ctx.base}/api/v3/repos/${repo.owner}/${repo.name}/git/${type}s/${oid}` },
  };
}

async function gitData(ctx: Ctx, repo: Repo, tail: readonly string[]): Promise<void> {
  const { forge, method, res } = ctx;
  const kind = tail[1];
  if (kind === 'refs' || kind === 'ref') {
    if (method === 'POST' && tail.length === 2 && kind === 'refs') {
      const body = await jsonBody(ctx);
      const ref = body.ref;
      const sha = typeof body.sha === 'string' ? body.sha.toLowerCase() : '';
      if (typeof ref !== 'string' || !/^refs\/[^\s]+$/.test(ref) || ref.split('/').length < 3) throw new ForgeError(422, 'Reference name is invalid');
      if (!SHA_RE.test(sha) || !forge.objects.has(sha)) throw new ForgeError(422, 'Object does not exist');
      if (repo.refs.has(ref)) throw new ForgeError(422, 'Reference already exists');
      forge.setRef(repo, ref, sha);
      return sendJson(res, 201, refJson(ctx, repo, ref, sha));
    }
    const name = tail.slice(2).join('/');
    const full = `refs/${name}`;
    if (method === 'GET') {
      if (name === '' || name === 'heads' || name === 'tags') {
        const prefix = name === '' ? 'refs/' : `${full}/`;
        const list = [...repo.refs.entries()].filter(([r]) => r.startsWith(prefix)).sort(([a], [b]) => (a < b ? -1 : 1));
        return sendJson(res, 200, list.map(([r, oid]) => refJson(ctx, repo, r, oid)));
      }
      const oid = repo.refs.get(full);
      if (oid === undefined) {
        const matches = [...repo.refs.entries()].filter(([r]) => r.startsWith(`${full}/`));
        if (kind === 'refs' && matches.length > 0) return sendJson(res, 200, matches.map(([r, o]) => refJson(ctx, repo, r, o)));
        throw new ForgeError(404, 'Not Found');
      }
      return sendJson(res, 200, refJson(ctx, repo, full, oid));
    }
    if (method === 'PATCH') {
      const body = await jsonBody(ctx);
      const old = repo.refs.get(full);
      if (old === undefined) throw new ForgeError(422, 'Reference does not exist');
      const sha = typeof body.sha === 'string' ? body.sha.toLowerCase() : '';
      if (!SHA_RE.test(sha) || !forge.objects.has(sha)) throw new ForgeError(422, 'Object does not exist');
      if (body.force !== true && full.startsWith('refs/heads/') && !forge.isAncestor(old, sha)) throw new ForgeError(422, 'Update is not a fast forward');
      forge.setRef(repo, full, sha);
      return sendJson(res, 200, refJson(ctx, repo, full, sha));
    }
    if (method === 'DELETE') {
      if (!repo.refs.has(full)) throw new ForgeError(422, 'Reference does not exist');
      forge.setRef(repo, full, null);
      return sendJson(res, 204, null);
    }
  }
  if (method === 'GET' && tail.length === 3 && SHA_RE.test(tail[2] ?? '')) {
    const oid = (tail[2] as string).toLowerCase();
    const obj = forge.objects.get(oid);
    if (kind === 'commits' && obj?.type === 'commit') {
      const c = commitJson(forge, ctx.base, repo, oid) as Json;
      return sendJson(res, 200, { sha: oid, ...(c.commit as Json), parents: c.parents });
    }
    if (kind === 'trees' && obj?.type === 'tree') {
      const entries = forge.objects.tree(oid) ?? [];
      return sendJson(res, 200, {
        sha: oid,
        tree: entries.map((e) => ({ path: e.name, mode: e.mode === '40000' ? '040000' : e.mode, type: e.mode === '40000' ? 'tree' : e.mode === '160000' ? 'commit' : 'blob', sha: e.oid })),
        truncated: false,
      });
    }
    if (kind === 'blobs' && obj?.type === 'blob') {
      return sendJson(res, 200, { sha: oid, size: obj.data.length, encoding: 'base64', content: obj.data.toString('base64') });
    }
  }
  throw new ForgeError(404, 'Not Found');
}

// ---- contents ----------------------------------------------------------------------------------

function contentEntry(ctx: Ctx, repo: Repo, path: string, kind: 'file' | 'dir' | 'symlink' | 'submodule', oid: string, size: number): Json {
  const api = `${ctx.base}/api/v3/repos/${repo.owner}/${repo.name}`;
  const name = path.slice(path.lastIndexOf('/') + 1);
  return {
    type: kind,
    name,
    path,
    sha: oid,
    size,
    url: `${api}/contents/${path}`,
    html_url: `${ctx.base}/${repo.owner}/${repo.name}/blob/${repo.defaultBranch}/${path}`,
    git_url: `${api}/git/${kind === 'dir' ? 'trees' : 'blobs'}/${oid}`,
    download_url: kind === 'file' ? `${ctx.base}/${repo.owner}/${repo.name}/raw/${repo.defaultBranch}/${path}` : null,
  };
}

function normalizePath(path: string): string {
  return path.split('/').filter((p) => p !== '' && p !== '.').join('/');
}

async function contents(ctx: Ctx, repo: Repo, rawPath: string): Promise<void> {
  const { forge, method, res } = ctx;
  const path = normalizePath(rawPath);
  if (path.split('/').includes('..')) throw new ForgeError(422, 'path is invalid');

  if (method === 'GET') {
    if (repo.refs.size === 0) throw new ForgeError(404, 'This repository is empty.');
    const ref = ctx.query.get('ref') ?? undefined;
    const tip = forge.resolve(repo, ref);
    if (tip === null) throw new ForgeError(404, `No commit found for the ref ${ref ?? ''}`);
    const found = forge.lookup(tip, path);
    if (found === null) throw new ForgeError(404, 'Not Found');
    if (found.kind === 'tree') {
      const entries = forge.objects.tree(found.oid) ?? [];
      const list = entries.map((e) => {
        const child = path === '' ? e.name : `${path}/${e.name}`;
        if (e.mode === '40000') return contentEntry(ctx, repo, child, 'dir', e.oid, 0);
        if (e.mode === '160000') return contentEntry(ctx, repo, child, 'submodule', e.oid, 0);
        const size = forge.objects.get(e.oid)?.data.length ?? 0;
        return contentEntry(ctx, repo, child, e.mode === '120000' ? 'symlink' : 'file', e.oid, size);
      });
      list.sort((a, b) => ((a.name as string) < (b.name as string) ? -1 : 1));
      return sendJson(res, 200, list);
    }
    const blob = forge.objects.get(found.oid);
    const data = blob?.data ?? Buffer.alloc(0);
    const b64 = data.toString('base64').replace(/(.{60})/g, '$1\n');
    return sendJson(res, 200, { ...contentEntry(ctx, repo, path, 'file', found.oid, data.length), encoding: 'base64', content: b64 });
  }

  if (method === 'PUT' || method === 'DELETE') {
    if (path === '') throw new ForgeError(422, 'path is required');
    const body = await jsonBody(ctx);
    const branch = typeof body.branch === 'string' && body.branch !== '' ? body.branch.replace(/^refs\/heads\//, '') : forge.headBranch(repo);
    const tip = forge.branchTip(repo, branch);
    if (tip === null && repo.refs.size > 0 && typeof body.branch === 'string') throw new ForgeError(404, `Branch ${branch} not found`);
    const existing = tip === null ? null : forge.lookup(tip, path);
    if (existing !== null && existing.kind === 'tree') throw new ForgeError(422, 'path is a directory');
    const message = typeof body.message === 'string' && body.message !== '' ? body.message : method === 'PUT' ? `Update ${path}` : `Delete ${path}`;
    const sha = typeof body.sha === 'string' ? body.sha.toLowerCase() : undefined;
    const commitMessage = message.endsWith('\n') ? message : `${message}\n`;

    if (method === 'PUT') {
      if (typeof body.content !== 'string') throw new ForgeError(422, 'Invalid request.\n\n"content" wasn\'t supplied.');
      if (!/^[A-Za-z0-9+/=\s]*$/.test(body.content)) throw new ForgeError(422, 'content is not valid Base64');
      if (existing !== null) {
        if (sha === undefined) throw new ForgeError(422, 'Invalid request.\n\n"sha" wasn\'t supplied.');
        if (sha !== existing.oid) throw new ForgeError(409, `${path} does not match ${sha}`);
      }
      const data = Buffer.from(body.content, 'base64');
      const { commit } = forge.commitFiles(repo, branch, [{ path, content: data }], commitMessage);
      const blobOid = forge.lookup(commit, path)?.oid ?? ZERO_OID;
      const c = commitJson(forge, ctx.base, repo, commit) as Json;
      return sendJson(res, existing === null ? 201 : 200, {
        content: { ...contentEntry(ctx, repo, path, 'file', blobOid, data.length), encoding: 'base64' },
        commit: { sha: commit, ...(c.commit as Json), parents: c.parents },
      });
    }

    // DELETE
    if (existing === null) throw new ForgeError(404, 'Not Found');
    if (sha === undefined) throw new ForgeError(422, 'Invalid request.\n\n"sha" wasn\'t supplied.');
    if (sha !== existing.oid) throw new ForgeError(409, `${path} does not match ${sha}`);
    const { commit } = forge.commitFiles(repo, branch, [{ path, content: null }], commitMessage);
    const c = commitJson(forge, ctx.base, repo, commit) as Json;
    return sendJson(res, 200, { content: null, commit: { sha: commit, ...(c.commit as Json), parents: c.parents } });
  }
  throw new ForgeError(404, 'Not Found');
}

// ---- labels, issues, comments ------------------------------------------------------------------

async function labels(ctx: Ctx, repo: Repo, tail: readonly string[]): Promise<void> {
  const { forge, method, res, base } = ctx;
  if (tail.length === 1) {
    if (method === 'GET') return sendPage(ctx, [...repo.labels.values()].map((l) => labelJson(base, repo, l)));
    if (method === 'POST') {
      const body = await jsonBody(ctx);
      const label = forge.createLabel(repo, body as { name: unknown });
      return sendJson(res, 201, labelJson(base, repo, label));
    }
  }
  if (tail.length === 2) {
    const label = repo.labels.get((tail[1] as string).toLowerCase());
    if (label === undefined) throw new ForgeError(404, 'Not Found');
    if (method === 'GET') return sendJson(res, 200, labelJson(base, repo, label));
    if (method === 'DELETE') {
      repo.labels.delete(label.name.toLowerCase());
      for (const issue of repo.issues.values()) issue.labels = issue.labels.filter((l) => l.toLowerCase() !== label.name.toLowerCase());
      return sendJson(res, 204, null);
    }
  }
  throw new ForgeError(404, 'Not Found');
}

/**
 * A single issue's serialized JSON, reused until anything it shows changes: every mutation moves
 * updatedAt (the forge clock ticks per event), and the key also covers state, labels, the comment
 * count and the origin the URLs are built from.
 */
const issueTextCache = new WeakMap<Issue, { key: string; text: string }>();

function cachedIssueText(forge: Forge, base: string, repo: Repo, issue: Issue): string {
  const label = repo.labels.size;
  const key = `${base}|${repo.owner}/${repo.name}|${issue.updatedAt}|${issue.state}|${issue.labels.join(',')}|${label}|${(repo.comments.get(issue.number) ?? []).length}`;
  const hit = issueTextCache.get(issue);
  if (hit !== undefined && hit.key === key) return hit.text;
  const text = JSON.stringify(issueJson(forge, base, repo, issue));
  issueTextCache.set(issue, { key, text });
  return text;
}

async function issues(ctx: Ctx, repo: Repo, tail: readonly string[]): Promise<void> {
  const { forge, method, res, base, query } = ctx;
  if (tail.length === 1) {
    if (method === 'GET') {
      const direction = query.get('direction') === 'asc' ? 'asc' : 'desc';
      const labelsQ = query.get('labels');
      const list = forge.listIssues(repo, {
        state: query.get('state') ?? 'open',
        direction,
        labels: labelsQ === null || labelsQ === '' ? undefined : labelsQ.split(','),
      });
      if (query.get('sort') === 'updated') list.sort((a, b) => (a.updatedAt === b.updatedAt ? b.number - a.number : a.updatedAt < b.updatedAt ? 1 : -1) * (direction === 'asc' ? -1 : 1));
      return sendPage(ctx, list.map((i) => issueJson(forge, base, repo, i)));
    }
    if (method === 'POST') {
      const body = await jsonBody(ctx);
      const issue = forge.createIssue(repo, body as { title: unknown });
      return sendJson(res, 201, issueJson(forge, base, repo, issue));
    }
  }
  if (tail[1] === 'comments' && tail.length === 2 && method === 'GET') {
    const all = [...repo.comments.values()].flat().sort((a, b) => a.id - b.id);
    return sendPage(ctx, all.map((c) => commentJson(forge, base, repo, c)));
  }
  if (tail.length >= 2) {
    const number = issueNumberOf(tail[1]);
    if (tail.length === 2) {
      const pull = repo.pulls.get(number);
      if (pull !== undefined && method === 'GET') {
        // A pull request is an issue too; answer its issue view.
        const view = issueJson(forge, base, repo, { ...pull, labels: [], state: pull.state } as never);
        return sendJson(res, 200, { ...view, pull_request: { url: `${base}/api/v3/repos/${repo.owner}/${repo.name}/pulls/${number}` } });
      }
      const issue = forge.requireIssue(repo, number);
      if (method === 'GET') return sendJsonText(res, 200, cachedIssueText(forge, base, repo, issue));
      if (method === 'PATCH') {
        const body = await jsonBody(ctx);
        forge.updateIssue(repo, issue, body);
        return sendJson(res, 200, issueJson(forge, base, repo, issue));
      }
    }
    if (tail[2] === 'comments' && tail.length === 3) {
      if (method === 'GET') return sendPage(ctx, forge.listComments(repo, number).map((c) => commentJson(forge, base, repo, c)));
      if (method === 'POST') {
        const body = await jsonBody(ctx);
        const comment = forge.addComment(repo, number, body.body);
        return sendJson(res, 201, commentJson(forge, base, repo, comment));
      }
    }
    if (tail[2] === 'labels' && tail.length === 3) {
      const issue = forge.requireIssue(repo, number);
      if (method === 'GET') return sendJson(res, 200, issue.labels.map((n) => labelJson(base, repo, repo.labels.get(n.toLowerCase()) as never)));
      if (method === 'POST' || method === 'PUT') {
        const body = await jsonBody(ctx);
        const add = forge.resolveLabels(repo, Array.isArray(body) ? body : body.labels);
        issue.labels = method === 'PUT' ? add : [...new Set([...issue.labels, ...add])];
        return sendJson(res, 200, issue.labels.map((n) => labelJson(base, repo, repo.labels.get(n.toLowerCase()) as never)));
      }
    }
  }
  throw new ForgeError(404, 'Not Found');
}

// ---- pulls -------------------------------------------------------------------------------------

async function pulls(ctx: Ctx, repo: Repo, tail: readonly string[]): Promise<void> {
  const { forge, method, res, base, query } = ctx;
  if (tail.length === 1) {
    if (method === 'GET') {
      const direction = query.get('direction') === 'asc' ? 'asc' : 'desc';
      let list = forge.listPulls(repo, { state: query.get('state') ?? 'open', direction });
      const head = query.get('head');
      if (head !== null && head !== '') list = list.filter((p) => p.head === Forge.branchArg(head));
      const baseQ = query.get('base');
      if (baseQ !== null && baseQ !== '') list = list.filter((p) => p.base === baseQ);
      return sendPage(ctx, list.map((p) => pullJson(forge, base, repo, p)));
    }
    if (method === 'POST') {
      const body = await jsonBody(ctx);
      const pull = forge.createPull(repo, body as { title: unknown; head: unknown; base: unknown });
      return sendJson(res, 201, pullJson(forge, base, repo, pull));
    }
  }
  if (tail.length >= 2) {
    const number = issueNumberOf(tail[1]);
    const pull = forge.requirePull(repo, number);
    if (tail.length === 2) {
      if (method === 'GET') return sendJson(res, 200, pullJson(forge, base, repo, pull));
      if (method === 'PATCH') {
        const body = await jsonBody(ctx);
        forge.updatePull(repo, pull, body);
        return sendJson(res, 200, pullJson(forge, base, repo, pull));
      }
    }
    if (tail[2] === 'files' && tail.length === 3 && method === 'GET') {
      const files = forge.pullFiles(repo, pull).map((f) => ({
        sha: f.sha,
        filename: f.path,
        status: f.status,
        additions: f.additions,
        deletions: f.deletions,
        changes: f.changes,
        blob_url: `${base}/${repo.owner}/${repo.name}/blob/${f.sha}/${f.path}`,
        raw_url: `${base}/${repo.owner}/${repo.name}/raw/${f.sha}/${f.path}`,
        contents_url: `${base}/api/v3/repos/${repo.owner}/${repo.name}/contents/${f.path}`,
        patch: f.patch,
      }));
      return sendPage(ctx, files);
    }
    if (tail[2] === 'commits' && tail.length === 3 && method === 'GET') {
      const { head, base: baseSha } = forge.pullShas(repo, pull);
      const baseSet = new Set(forge.history(baseSha));
      const list = forge.history(head).filter((oid) => !baseSet.has(oid)).reverse();
      return sendPage(ctx, list.map((oid) => commitJson(forge, base, repo, oid)));
    }
    if (tail[2] === 'merge' && tail.length === 3) {
      if (method === 'GET') {
        if (pull.merged) return sendJson(res, 204, null);
        throw new ForgeError(404, 'Not Found');
      }
      if (method === 'PUT') {
        const body = await jsonBody(ctx);
        const method_ = body.merge_method;
        if (method_ !== undefined && method_ !== 'squash' && method_ !== 'merge' && method_ !== 'rebase') throw new ForgeError(422, 'merge_method is invalid');
        const result = forge.mergePull(repo, pull, body);
        return sendJson(res, 200, { sha: result.sha, merged: true, message: result.message });
      }
    }
  }
  throw new ForgeError(404, 'Not Found');
}
