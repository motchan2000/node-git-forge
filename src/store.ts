// The forge's one store and its domain services. REST, GraphQL, MCP and git smart HTTP all call
// these methods; none of them keeps a counter or a cache of its own, so ids, numbers and states agree
// across every surface. Errors are ForgeError(status, message); adapters map them onto their wire.

import { ObjectStore, ZERO_OID, serializeCommit, type Oid, type Signature } from './git/objects.ts';
import { ancestors, buildTree, diffFlat, flattenTree, isAncestor, lineCounts, lookupPath, mergeBase, mergeFlat, type FlatEntry } from './git/treeops.ts';

export class ForgeError extends Error {
  readonly status: number;
  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

export interface User {
  readonly id: number;
  readonly login: string;
  readonly name: string;
  readonly email: string;
}

export interface Label {
  readonly id: number;
  name: string;
  color: string;
  description: string;
}

export interface Comment {
  readonly id: number;
  readonly issueNumber: number;
  body: string;
  readonly user: string;
  readonly createdAt: string;
  updatedAt: string;
}

export interface Issue {
  readonly id: number;
  readonly number: number;
  title: string;
  body: string;
  state: 'open' | 'closed';
  labels: string[];
  readonly user: string;
  readonly createdAt: string;
  updatedAt: string;
  closedAt: string | null;
  readonly seq: number;
}

export interface Pull {
  readonly id: number;
  readonly number: number;
  title: string;
  body: string;
  state: 'open' | 'closed';
  readonly head: string; // branch name
  base: string; // branch name
  /** The head tip recorded when the pull closed (kept so a merged pull still reports its head). */
  headSha: string;
  baseSha: string;
  merged: boolean;
  mergedAt: string | null;
  mergeCommitSha: string | null;
  readonly user: string;
  readonly createdAt: string;
  updatedAt: string;
  closedAt: string | null;
  readonly seq: number;
}

export interface Repo {
  readonly id: number;
  readonly owner: string;
  readonly name: string;
  description: string;
  private: boolean;
  defaultBranch: string;
  readonly createdAt: string;
  updatedAt: string;
  pushedAt: string;
  readonly refs: Map<string, Oid>;
  readonly issues: Map<number, Issue>;
  readonly pulls: Map<number, Pull>;
  readonly labels: Map<string, Label>; // keyed by lowercased name
  readonly comments: Map<number, Comment[]>; // by issue number
  nextNumber: number;
  /** Bumped on every ref change; git pack caches key on it. */
  refVersion: number;
}

export interface FileEdit {
  readonly path: string;
  /** New content, or null to delete the path. */
  readonly content: Buffer | null;
}

const DEFAULT_EPOCH = Date.UTC(2026, 0, 1) / 1000;

function trialEpoch(): number {
  const raw = process.env.MAKEFUN_TRIAL_NOW;
  if (raw !== undefined && raw !== '') {
    const ms = Date.parse(raw);
    if (Number.isFinite(ms)) return Math.floor(ms / 1000);
    const n = Number(raw);
    if (Number.isFinite(n) && n > 0) return Math.floor(n > 1e12 ? n / 1000 : n);
  }
  return DEFAULT_EPOCH;
}

export class Forge {
  readonly objects = new ObjectStore();
  readonly viewer: User;
  readonly #repos = new Map<string, Repo>(); // key: lower(owner)/lower(name)
  #nextRepoId = 1;
  #nextIssueId = 1;
  #nextPullId = 1;
  #nextCommentId = 1;
  #nextLabelId = 1;
  #seq = 0;
  readonly #epoch = trialEpoch();
  #tick = 0;

  constructor(viewer: User) {
    this.viewer = viewer;
  }

  /** A deterministic clock: the trial's opening time plus one second per event. */
  now(): { seconds: number; iso: string } {
    this.#tick += 1;
    const seconds = this.#epoch + this.#tick;
    return { seconds, iso: new Date(seconds * 1000).toISOString().replace(/\.\d{3}Z$/, 'Z') };
  }

  signature(): Signature {
    return { name: this.viewer.name, email: this.viewer.email, time: this.now().seconds, tz: '+0000' };
  }

  // ------------------------------------------------------------------------------------------
  // Repositories

  static key(owner: string, name: string): string {
    return `${owner.toLowerCase()}/${name.toLowerCase()}`;
  }

  getRepo(owner: string, name: string): Repo | null {
    return this.#repos.get(Forge.key(owner, name)) ?? null;
  }

  requireRepo(owner: string, name: string): Repo {
    const repo = this.getRepo(owner, name);
    if (repo === null) throw new ForgeError(404, 'Not Found');
    return repo;
  }

  getRepoById(id: number): Repo | null {
    for (const repo of this.#repos.values()) if (repo.id === id) return repo;
    return null;
  }

  findIssue(id: number): { repo: Repo; issue: Issue } | null {
    for (const repo of this.#repos.values()) for (const issue of repo.issues.values()) if (issue.id === id) return { repo, issue };
    return null;
  }

  findPull(id: number): { repo: Repo; pull: Pull } | null {
    for (const repo of this.#repos.values()) for (const pull of repo.pulls.values()) if (pull.id === id) return { repo, pull };
    return null;
  }

  findComment(id: number): { repo: Repo; comment: Comment } | null {
    for (const repo of this.#repos.values()) for (const list of repo.comments.values()) for (const comment of list) if (comment.id === id) return { repo, comment };
    return null;
  }

  listRepos(owner: string): Repo[] {
    const lower = owner.toLowerCase();
    return [...this.#repos.values()].filter((r) => r.owner.toLowerCase() === lower);
  }

  createRepo(input: { name: unknown; description?: unknown; private?: unknown; default_branch?: unknown }): Repo {
    const name = input.name;
    if (typeof name !== 'string' || name === '' || name.length > 100 || !/^[A-Za-z0-9._-]+$/.test(name) || name === '.' || name === '..') {
      throw new ForgeError(422, 'Repository creation failed: name is invalid');
    }
    if (this.getRepo(this.viewer.login, name) !== null) throw new ForgeError(422, 'Repository creation failed: name already exists on this account');
    const t = this.now().iso;
    const repo: Repo = {
      id: this.#nextRepoId++,
      owner: this.viewer.login,
      name,
      description: typeof input.description === 'string' ? input.description : '',
      private: input.private === true,
      defaultBranch: typeof input.default_branch === 'string' && input.default_branch !== '' ? input.default_branch : 'main',
      createdAt: t,
      updatedAt: t,
      pushedAt: t,
      refs: new Map(),
      issues: new Map(),
      pulls: new Map(),
      labels: new Map(),
      comments: new Map(),
      nextNumber: 1,
      refVersion: 0,
    };
    this.#repos.set(Forge.key(repo.owner, repo.name), repo);
    return repo;
  }

  updateRepo(repo: Repo, patch: Record<string, unknown>): Repo {
    if (patch.description !== undefined) {
      if (patch.description !== null && typeof patch.description !== 'string') throw new ForgeError(422, 'description must be a string');
      repo.description = patch.description ?? '';
    }
    if (patch.private !== undefined) {
      if (typeof patch.private !== 'boolean') throw new ForgeError(422, 'private must be a boolean');
      repo.private = patch.private;
    }
    if (patch.default_branch !== undefined) {
      if (typeof patch.default_branch !== 'string' || patch.default_branch === '') throw new ForgeError(422, 'default_branch must be a branch name');
      repo.defaultBranch = patch.default_branch;
    }
    repo.updatedAt = this.now().iso;
    return repo;
  }

  deleteRepo(repo: Repo): void {
    this.#repos.delete(Forge.key(repo.owner, repo.name));
  }

  // ------------------------------------------------------------------------------------------
  // Refs

  /** Set a ref, bumping the repository's ref version. `null` deletes it. */
  setRef(repo: Repo, ref: string, oid: Oid | null): void {
    if (oid === null || oid === ZERO_OID) repo.refs.delete(ref);
    else repo.refs.set(ref, oid);
    repo.refVersion += 1;
    repo.pushedAt = this.now().iso;
  }

  branchTip(repo: Repo, branch: string): Oid | null {
    return repo.refs.get(`refs/heads/${branch}`) ?? null;
  }

  /** HEAD's target: the default branch, or (when it is unborn) the first branch that exists. */
  headBranch(repo: Repo): string {
    if (repo.refs.has(`refs/heads/${repo.defaultBranch}`)) return repo.defaultBranch;
    for (const ref of [...repo.refs.keys()].sort()) if (ref.startsWith('refs/heads/')) return ref.slice(11);
    return repo.defaultBranch;
  }

  /** Resolve a branch name, tag name, full ref or commit sha to a commit oid. */
  resolve(repo: Repo, refish: string | undefined): Oid | null {
    const name = refish === undefined || refish === '' ? this.headBranch(repo) : refish;
    const direct = repo.refs.get(name) ?? repo.refs.get(`refs/heads/${name}`) ?? repo.refs.get(`refs/tags/${name}`);
    if (direct !== undefined) return this.peelToCommit(direct);
    if (/^[0-9a-f]{40}$/i.test(name)) {
      const oid = name.toLowerCase();
      return this.objects.has(oid) ? this.peelToCommit(oid) : null;
    }
    if (/^[0-9a-f]{4,39}$/i.test(name)) {
      const lower = name.toLowerCase();
      for (const oid of this.objects.objects.keys()) if (oid.startsWith(lower) && this.objects.get(oid)?.type === 'commit') return oid;
    }
    return null;
  }

  peelToCommit(oid: Oid): Oid | null {
    let current: Oid | null = oid;
    for (let i = 0; i < 10 && current !== null; i += 1) {
      const obj = this.objects.get(current);
      if (obj === undefined) return null;
      if (obj.type === 'commit') return current;
      if (obj.type !== 'tag') return null;
      const m = /^object ([0-9a-f]{40})/.exec(obj.data.toString('latin1', 0, 60));
      current = m === null ? null : (m[1] ?? null);
    }
    return null;
  }

  // ------------------------------------------------------------------------------------------
  // Commits on branches (contents API, merges)

  makeCommit(tree: Oid, parents: readonly Oid[], message: string): Oid {
    const sig = this.signature();
    return this.objects.put('commit', serializeCommit({ tree, parents, author: sig, committer: sig, message }));
  }

  /**
   * Commit file edits on top of a branch (creating it, or the repository's first commit, when it
   * does not exist) and move the branch. Returns the new commit oid and the new flat tree.
   */
  commitFiles(repo: Repo, branch: string, edits: readonly FileEdit[], message: string): { commit: Oid; flat: Map<string, FlatEntry> } {
    const parent = this.branchTip(repo, branch);
    const flat = parent === null ? new Map<string, FlatEntry>() : this.flatAt(parent);
    for (const edit of edits) {
      if (edit.content === null) flat.delete(edit.path);
      else flat.set(edit.path, { mode: flat.get(edit.path)?.mode ?? '100644', oid: this.objects.put('blob', edit.content) });
    }
    const tree = buildTree(this.objects, flat);
    const commit = this.makeCommit(tree, parent === null ? [] : [parent], message);
    this.setRef(repo, `refs/heads/${branch}`, commit);
    return { commit, flat };
  }

  flatAt(commitOid: Oid): Map<string, FlatEntry> {
    const c = this.objects.commit(commitOid);
    return c === null ? new Map() : flattenTree(this.objects, c.tree);
  }

  lookup(commitOid: Oid, path: string): ReturnType<typeof lookupPath> {
    const c = this.objects.commit(commitOid);
    if (c === null) return null;
    return lookupPath(this.objects, c.tree, path);
  }

  history(tip: Oid, limit = Infinity): Oid[] {
    return ancestors(this.objects, tip, limit);
  }

  isAncestor(a: Oid, b: Oid): boolean {
    return isAncestor(this.objects, a, b);
  }

  // ------------------------------------------------------------------------------------------
  // Labels, issues and comments

  createLabel(repo: Repo, input: { name: unknown; color?: unknown; description?: unknown }): Label {
    if (typeof input.name !== 'string' || input.name.trim() === '') throw new ForgeError(422, 'Validation Failed: name is required');
    const key = input.name.toLowerCase();
    if (repo.labels.has(key)) throw new ForgeError(422, 'Validation Failed: label already exists');
    const label: Label = {
      id: this.#nextLabelId++,
      name: input.name,
      color: typeof input.color === 'string' && input.color !== '' ? input.color.replace(/^#/, '') : 'ededed',
      description: typeof input.description === 'string' ? input.description : '',
    };
    repo.labels.set(key, label);
    return label;
  }

  /** Normalise a labels argument ([string] or [{name}]); unknown labels are a 422. */
  resolveLabels(repo: Repo, labels: unknown): string[] {
    if (labels === undefined || labels === null) return [];
    if (!Array.isArray(labels)) throw new ForgeError(422, 'Validation Failed: labels must be an array');
    const out: string[] = [];
    for (const item of labels) {
      const name = typeof item === 'string' ? item : typeof item === 'object' && item !== null && typeof (item as { name?: unknown }).name === 'string' ? (item as { name: string }).name : null;
      if (name === null) throw new ForgeError(422, 'Validation Failed: a label must be a name');
      const label = repo.labels.get(name.toLowerCase());
      if (label === undefined) throw new ForgeError(422, `Validation Failed: label ${JSON.stringify(name)} does not exist`);
      if (!out.includes(label.name)) out.push(label.name);
    }
    return out;
  }

  createIssue(repo: Repo, input: { title: unknown; body?: unknown; labels?: unknown }): Issue {
    if (typeof input.title !== 'string' || input.title === '') throw new ForgeError(422, 'Validation Failed: title is required');
    if (input.body !== undefined && input.body !== null && typeof input.body !== 'string') throw new ForgeError(422, 'Validation Failed: body must be a string');
    const labels = this.resolveLabels(repo, input.labels);
    const t = this.now().iso;
    const issue: Issue = {
      id: this.#nextIssueId++,
      number: repo.nextNumber++,
      title: input.title,
      body: typeof input.body === 'string' ? input.body : '',
      state: 'open',
      labels,
      user: this.viewer.login,
      createdAt: t,
      updatedAt: t,
      closedAt: null,
      seq: ++this.#seq,
    };
    repo.issues.set(issue.number, issue);
    return issue;
  }

  requireIssue(repo: Repo, number: number): Issue {
    const issue = repo.issues.get(number);
    if (issue === undefined) throw new ForgeError(404, 'Not Found');
    return issue;
  }

  updateIssue(repo: Repo, issue: Issue, patch: Record<string, unknown>): Issue {
    if (patch.title !== undefined) {
      if (typeof patch.title !== 'string' || patch.title === '') throw new ForgeError(422, 'Validation Failed: title must be a non-empty string');
    }
    if (patch.body !== undefined && patch.body !== null && typeof patch.body !== 'string') throw new ForgeError(422, 'Validation Failed: body must be a string');
    if (patch.state !== undefined && patch.state !== 'open' && patch.state !== 'closed') throw new ForgeError(422, 'Validation Failed: state must be open or closed');
    const labels = patch.labels === undefined ? undefined : this.resolveLabels(repo, patch.labels);
    const t = this.now().iso;
    if (typeof patch.title === 'string') issue.title = patch.title;
    if (patch.body !== undefined) issue.body = typeof patch.body === 'string' ? patch.body : '';
    if (labels !== undefined) issue.labels = labels;
    if (patch.state === 'closed' && issue.state !== 'closed') {
      issue.state = 'closed';
      issue.closedAt = t;
    } else if (patch.state === 'open' && issue.state !== 'open') {
      issue.state = 'open';
      issue.closedAt = null;
    }
    issue.updatedAt = t;
    return issue;
  }

  /** Issues filtered by state ('open' default, 'closed', 'all') and sorted by creation. */
  listIssues(repo: Repo, opts: { state?: string; direction?: 'asc' | 'desc'; labels?: readonly string[] } = {}): Issue[] {
    const state = opts.state ?? 'open';
    let list = [...repo.issues.values()];
    if (state === 'open' || state === 'closed') list = list.filter((i) => i.state === state);
    if (opts.labels !== undefined && opts.labels.length > 0) {
      const wanted = opts.labels.map((l) => l.toLowerCase());
      list = list.filter((i) => wanted.every((w) => i.labels.some((l) => l.toLowerCase() === w)));
    }
    list.sort((a, b) => (opts.direction === 'asc' ? a.number - b.number : b.number - a.number));
    return list;
  }

  addComment(repo: Repo, number: number, body: unknown): Comment {
    if (!repo.issues.has(number) && !repo.pulls.has(number)) throw new ForgeError(404, 'Not Found');
    if (typeof body !== 'string' || body === '') throw new ForgeError(422, 'Validation Failed: body is required');
    const t = this.now().iso;
    const comment: Comment = { id: this.#nextCommentId++, issueNumber: number, body, user: this.viewer.login, createdAt: t, updatedAt: t };
    const list = repo.comments.get(number) ?? [];
    list.push(comment);
    repo.comments.set(number, list);
    const issue = repo.issues.get(number);
    if (issue !== undefined) issue.updatedAt = t;
    return comment;
  }

  listComments(repo: Repo, number: number): Comment[] {
    if (!repo.issues.has(number) && !repo.pulls.has(number)) throw new ForgeError(404, 'Not Found');
    return repo.comments.get(number) ?? [];
  }

  // ------------------------------------------------------------------------------------------
  // Pull requests

  /** Strip an `owner:` prefix from a head argument (GitHub accepts `user:branch`). */
  static branchArg(value: string): string {
    const colon = value.indexOf(':');
    return colon >= 0 ? value.slice(colon + 1) : value;
  }

  createPull(repo: Repo, input: { title: unknown; body?: unknown; head: unknown; base: unknown }): Pull {
    if (typeof input.title !== 'string' || input.title === '') throw new ForgeError(422, 'Validation Failed: title is required');
    if (typeof input.head !== 'string' || input.head === '') throw new ForgeError(422, 'Validation Failed: head is required');
    if (typeof input.base !== 'string' || input.base === '') throw new ForgeError(422, 'Validation Failed: base is required');
    const head = Forge.branchArg(input.head).replace(/^refs\/heads\//, '');
    const base = input.base.replace(/^refs\/heads\//, '');
    const headSha = this.branchTip(repo, head);
    const baseSha = this.branchTip(repo, base);
    if (headSha === null) throw new ForgeError(422, `Validation Failed: head branch ${JSON.stringify(head)} does not exist`);
    if (baseSha === null) throw new ForgeError(422, `Validation Failed: base branch ${JSON.stringify(base)} does not exist`);
    if (head === base) throw new ForgeError(422, 'Validation Failed: head and base must differ');
    for (const p of repo.pulls.values()) {
      if (p.state === 'open' && p.head === head && p.base === base) throw new ForgeError(422, `Validation Failed: A pull request already exists for ${repo.owner}:${head}.`);
    }
    const t = this.now().iso;
    const pull: Pull = {
      id: this.#nextPullId++,
      number: repo.nextNumber++,
      title: input.title,
      body: typeof input.body === 'string' ? input.body : '',
      state: 'open',
      head,
      base,
      headSha,
      baseSha,
      merged: false,
      mergedAt: null,
      mergeCommitSha: null,
      user: this.viewer.login,
      createdAt: t,
      updatedAt: t,
      closedAt: null,
      seq: ++this.#seq,
    };
    repo.pulls.set(pull.number, pull);
    return pull;
  }

  requirePull(repo: Repo, number: number): Pull {
    const pull = repo.pulls.get(number);
    if (pull === undefined) throw new ForgeError(404, 'Not Found');
    return pull;
  }

  /** Live head/base shas: the branch tips while open, the recorded ones once closed. */
  pullShas(repo: Repo, pull: Pull): { head: Oid; base: Oid } {
    if (pull.state === 'open') {
      pull.headSha = this.branchTip(repo, pull.head) ?? pull.headSha;
      pull.baseSha = this.branchTip(repo, pull.base) ?? pull.baseSha;
    }
    return { head: pull.headSha, base: pull.baseSha };
  }

  listPulls(repo: Repo, opts: { state?: string; direction?: 'asc' | 'desc' } = {}): Pull[] {
    const state = opts.state ?? 'open';
    let list = [...repo.pulls.values()];
    if (state === 'open' || state === 'closed') list = list.filter((p) => p.state === state);
    list.sort((a, b) => (opts.direction === 'asc' ? a.number - b.number : b.number - a.number));
    return list;
  }

  updatePull(repo: Repo, pull: Pull, patch: Record<string, unknown>): Pull {
    if (patch.title !== undefined && (typeof patch.title !== 'string' || patch.title === '')) throw new ForgeError(422, 'Validation Failed: title must be a non-empty string');
    if (patch.state !== undefined && patch.state !== 'open' && patch.state !== 'closed') throw new ForgeError(422, 'Validation Failed: state must be open or closed');
    this.pullShas(repo, pull);
    const t = this.now().iso;
    if (typeof patch.title === 'string') pull.title = patch.title;
    if (typeof patch.body === 'string') pull.body = patch.body;
    if (typeof patch.base === 'string' && this.branchTip(repo, patch.base) !== null) pull.base = patch.base;
    if (patch.state === 'closed' && pull.state === 'open') {
      pull.state = 'closed';
      pull.closedAt = t;
    } else if (patch.state === 'open' && pull.state === 'closed' && !pull.merged) {
      pull.state = 'open';
      pull.closedAt = null;
    }
    pull.updatedAt = t;
    return pull;
  }

  /** The files a pull changes: base...head (from the merge base), as GitHub reports them. */
  pullFiles(repo: Repo, pull: Pull): { path: string; status: string; additions: number; deletions: number; changes: number; sha: string; patch: string }[] {
    const { head, base } = this.pullShas(repo, pull);
    const from = pull.merged ? base : (mergeBase(this.objects, head, base) ?? base);
    const before = this.flatAt(from);
    const after = this.flatAt(head);
    return diffFlat(before, after).map((change) => {
      const oldText = change.before === undefined ? '' : (this.objects.get(change.before.oid)?.data.toString('utf8') ?? '');
      const newText = change.after === undefined ? '' : (this.objects.get(change.after.oid)?.data.toString('utf8') ?? '');
      const { additions, deletions } = lineCounts(oldText, newText);
      const sha = (change.after ?? change.before)?.oid ?? ZERO_OID;
      return { path: change.path, status: change.status, additions, deletions, changes: additions + deletions, sha, patch: '' };
    });
  }

  /** Whether the pull merges cleanly: the merged flat tree, or null on a conflict. */
  mergeResult(repo: Repo, pull: Pull): Map<string, FlatEntry> | null {
    const { head, base } = this.pullShas(repo, pull);
    if (this.isAncestor(base, head)) return this.flatAt(head);
    const mb = mergeBase(this.objects, head, base);
    const baseFlat = mb === null ? new Map<string, FlatEntry>() : this.flatAt(mb);
    return mergeFlat(baseFlat, this.flatAt(base), this.flatAt(head));
  }

  /** Squash-merge an open pull onto its base branch. 405 when it is not mergeable. */
  mergePull(repo: Repo, pull: Pull, opts: { commit_title?: unknown; commit_message?: unknown; sha?: unknown } = {}): { sha: Oid; message: string } {
    if (pull.merged) throw new ForgeError(405, 'Pull Request is not mergeable: already merged');
    if (pull.state !== 'open') throw new ForgeError(405, 'Pull Request is not mergeable: it is closed');
    const { head, base } = this.pullShas(repo, pull);
    if (typeof opts.sha === 'string' && opts.sha !== '' && opts.sha.toLowerCase() !== head) throw new ForgeError(409, 'Head branch was modified. Review and try the merge again.');
    const merged = this.mergeResult(repo, pull);
    if (merged === null) throw new ForgeError(405, 'Pull Request is not mergeable: merge conflict');
    const tree = buildTree(this.objects, merged);
    const title = typeof opts.commit_title === 'string' && opts.commit_title !== '' ? opts.commit_title : `${pull.title} (#${pull.number})`;
    const extra = typeof opts.commit_message === 'string' && opts.commit_message !== '' ? `\n\n${opts.commit_message}` : '';
    const message = `${title}${extra}\n`;
    const sha = this.makeCommit(tree, [base], message);
    this.setRef(repo, `refs/heads/${pull.base}`, sha);
    const t = this.now().iso;
    pull.headSha = head;
    pull.baseSha = base;
    pull.merged = true;
    pull.mergedAt = t;
    pull.closedAt = t;
    pull.updatedAt = t;
    pull.state = 'closed';
    pull.mergeCommitSha = sha;
    return { sha, message: 'Pull Request successfully merged' };
  }
}
