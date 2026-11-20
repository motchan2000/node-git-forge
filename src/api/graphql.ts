// GitHub GraphQL v4 at POST /api/graphql: a typed schema (which also answers introspection), an
// executor over parsed documents, and the queries and mutations gh and github-mcp-server send. Every
// resolver reads and writes the same Forge store as REST, so ids and states agree across surfaces.

import type { Req as IncomingMessage, Res as ServerResponse } from '../server.ts';
import { Forge, ForgeError, type Comment, type Issue, type Pull, type Repo } from '../store.ts';
import { readBody, sendJson } from '../http.ts';
import { GraphQLSyntaxError, parseDocument, valueOf, type Directive, type Document, type Operation, type Selection } from './graphql-parse.ts';
import { decodeNodeId, issueNodeId, pullNodeId, repoNodeId } from './present.ts';

type Obj = Record<string, unknown>;
type Args = Record<string, unknown>;

interface Ctx {
  readonly forge: Forge;
  readonly base: string;
}

interface FieldDef {
  readonly type: string;
  readonly args?: Readonly<Record<string, string>>;
  readonly resolve?: (parent: any, args: Args, ctx: Ctx) => unknown;
  readonly description?: string;
}

interface TypeDef {
  readonly kind: 'OBJECT' | 'INTERFACE' | 'ENUM' | 'INPUT_OBJECT' | 'SCALAR' | 'UNION';
  readonly fields?: Readonly<Record<string, FieldDef>>;
  readonly interfaces?: readonly string[];
  readonly possibleTypes?: readonly string[];
  readonly enumValues?: readonly string[];
  readonly inputFields?: Readonly<Record<string, string>>;
  readonly description?: string;
}

/** A value of an abstract type carries its concrete type. */
interface Typed {
  readonly __typename: string;
  readonly value: unknown;
}

class GraphQLFieldError extends Error {
  readonly type: string;
  constructor(message: string, type = 'UNPROCESSABLE') {
    super(message);
    this.type = type;
  }
}

class ValidationError extends Error {}

// ---------------------------------------------------------------------------------------------
// Wrapped domain values

interface IssueRef {
  readonly repo: Repo;
  readonly issue: Issue;
}
interface PullRef {
  readonly repo: Repo;
  readonly pull: Pull;
}
interface CommentRef {
  readonly repo: Repo;
  readonly comment: Comment;
}

function iso(seconds: number): string {
  return new Date(seconds * 1000).toISOString().replace(/\.\d{3}Z$/, 'Z');
}

function commentNodeId(id: number): string {
  return Buffer.from(`012:IssueComment${id}`).toString('base64');
}

function userValue(ctx: Ctx, login: string): Obj {
  const viewer = ctx.forge.viewer;
  const self = login.toLowerCase() === viewer.login.toLowerCase();
  return {
    login,
    id: Buffer.from(`04:User${self ? viewer.id : 0}`).toString('base64'),
    databaseId: self ? viewer.id : 0,
    name: self ? viewer.name : login,
    email: self ? viewer.email : '',
    url: `${ctx.base}/${login}`,
    __typename: 'User',
  };
}

function encodeCursor(index: number): string {
  return Buffer.from(`cursor:v2:${index}`).toString('base64');
}

function decodeCursor(cursor: unknown): number | null {
  if (typeof cursor !== 'string' || cursor === '') return null;
  const m = /^cursor:v2:(\d+)$/.exec(Buffer.from(cursor, 'base64').toString('utf8'));
  return m === null ? null : Number(m[1]);
}

/** A Relay connection over a list, honouring first/last/after/before. */
function connection<T>(list: readonly T[], args: Args): Obj {
  let start = 0;
  let end = list.length;
  const after = decodeCursor(args.after);
  const before = decodeCursor(args.before);
  if (after !== null) start = Math.max(start, after + 1);
  if (before !== null) end = Math.min(end, before);
  if (typeof args.first === 'number') {
    if (args.first < 0) throw new GraphQLFieldError('`first` must be non-negative');
    end = Math.min(end, start + args.first);
  }
  if (typeof args.last === 'number') {
    if (args.last < 0) throw new GraphQLFieldError('`last` must be non-negative');
    start = Math.max(start, end - args.last);
  }
  const edges: Obj[] = [];
  for (let i = start; i < end; i += 1) edges.push({ node: list[i], cursor: encodeCursor(i) });
  return {
    nodes: edges.map((e) => e.node),
    edges,
    totalCount: list.length,
    pageInfo: {
      hasNextPage: end < list.length,
      hasPreviousPage: start > 0,
      startCursor: edges.length > 0 ? edges[0]?.cursor : null,
      endCursor: edges.length > 0 ? edges[edges.length - 1]?.cursor : null,
    },
  };
}

function pullState(pull: Pull): string {
  return pull.merged ? 'MERGED' : pull.state === 'open' ? 'OPEN' : 'CLOSED';
}

function repoByGlobalId(ctx: Ctx, id: unknown): Repo {
  const decoded = typeof id === 'string' ? decodeNodeId(id) : null;
  const repo = decoded?.kind === 'Repository' ? ctx.forge.getRepoById(decoded.id) : null;
  if (repo === null || repo === undefined) throw new GraphQLFieldError(`Could not resolve to a node with the global id of '${String(id)}'`, 'NOT_FOUND');
  return repo;
}

function issueByGlobalId(ctx: Ctx, id: unknown): IssueRef {
  const decoded = typeof id === 'string' ? decodeNodeId(id) : null;
  const found = decoded?.kind === 'Issue' ? ctx.forge.findIssue(decoded.id) : null;
  if (found === null) throw new GraphQLFieldError(`Could not resolve to a node with the global id of '${String(id)}'`, 'NOT_FOUND');
  return found;
}

function pullByGlobalId(ctx: Ctx, id: unknown): PullRef {
  const decoded = typeof id === 'string' ? decodeNodeId(id) : null;
  const found = decoded?.kind === 'PullRequest' ? ctx.forge.findPull(decoded.id) : null;
  if (found === null) throw new GraphQLFieldError(`Could not resolve to a node with the global id of '${String(id)}'`, 'NOT_FOUND');
  return found;
}

function nodeById(ctx: Ctx, id: unknown): Typed | null {
  const decoded = typeof id === 'string' ? decodeNodeId(id) : null;
  if (decoded === null) return null;
  switch (decoded.kind) {
    case 'Repository': {
      const repo = ctx.forge.getRepoById(decoded.id);
      return repo === null ? null : { __typename: 'Repository', value: repo };
    }
    case 'Issue': {
      const found = ctx.forge.findIssue(decoded.id);
      return found === null ? null : { __typename: 'Issue', value: found };
    }
    case 'PullRequest': {
      const found = ctx.forge.findPull(decoded.id);
      return found === null ? null : { __typename: 'PullRequest', value: found };
    }
    case 'IssueComment': {
      const found = ctx.forge.findComment(decoded.id);
      return found === null ? null : { __typename: 'IssueComment', value: found };
    }
    case 'User':
      return decoded.id === ctx.forge.viewer.id ? { __typename: 'User', value: userValue(ctx, ctx.forge.viewer.login) } : null;
    default:
      return null;
  }
}

function input(args: Args): Obj {
  const value = args.input;
  if (typeof value !== 'object' || value === null) throw new GraphQLFieldError('input is required');
  return value as Obj;
}

function mapForgeError<T>(fn: () => T): T {
  try {
    return fn();
  } catch (err) {
    if (err instanceof ForgeError) throw new GraphQLFieldError(err.message, err.status === 404 ? 'NOT_FOUND' : 'UNPROCESSABLE');
    throw err;
  }
}

// ---------------------------------------------------------------------------------------------
// The schema

const connectionFields = (node: string, edge: string): Record<string, FieldDef> => ({
  nodes: { type: `[${node}]` },
  edges: { type: `[${edge}]` },
  totalCount: { type: 'Int!' },
  pageInfo: { type: 'PageInfo!' },
});

const CONN_ARGS = { first: 'Int', last: 'Int', after: 'String', before: 'String' } as const;

const repositoryFields: Record<string, FieldDef> = {
  id: { type: 'ID!', resolve: (r: Repo) => repoNodeId(r.id) },
  databaseId: { type: 'Int', resolve: (r: Repo) => r.id },
  name: { type: 'String!', resolve: (r: Repo) => r.name },
  nameWithOwner: { type: 'String!', resolve: (r: Repo) => `${r.owner}/${r.name}` },
  owner: { type: 'RepositoryOwner!', resolve: (r: Repo, _a, ctx) => ({ __typename: 'User', value: userValue(ctx, r.owner) }) },
  description: { type: 'String', resolve: (r: Repo) => r.description },
  url: { type: 'URI!', resolve: (r: Repo, _a, ctx) => `${ctx.base}/${r.owner}/${r.name}` },
  sshUrl: { type: 'GitSSHRemote!', resolve: (r: Repo) => `git@forge:${r.owner}/${r.name}.git` },
  isPrivate: { type: 'Boolean!', resolve: (r: Repo) => r.private },
  visibility: { type: 'RepositoryVisibility!', resolve: (r: Repo) => (r.private ? 'PRIVATE' : 'PUBLIC') },
  isFork: { type: 'Boolean!', resolve: () => false },
  isArchived: { type: 'Boolean!', resolve: () => false },
  isEmpty: { type: 'Boolean!', resolve: (r: Repo) => r.refs.size === 0 },
  isTemplate: { type: 'Boolean!', resolve: () => false },
  hasIssuesEnabled: { type: 'Boolean!', resolve: () => true },
  hasWikiEnabled: { type: 'Boolean!', resolve: () => false },
  hasProjectsEnabled: { type: 'Boolean!', resolve: () => false },
  viewerPermission: { type: 'RepositoryPermission', resolve: () => 'ADMIN' },
  createdAt: { type: 'DateTime!', resolve: (r: Repo) => r.createdAt },
  updatedAt: { type: 'DateTime!', resolve: (r: Repo) => r.updatedAt },
  pushedAt: { type: 'DateTime', resolve: (r: Repo) => r.pushedAt },
  defaultBranchRef: {
    type: 'Ref',
    resolve: (r: Repo, _a, ctx) => {
      const oid = ctx.forge.branchTip(r, r.defaultBranch);
      return oid === null ? null : { repo: r, name: r.defaultBranch, prefix: 'refs/heads/', oid };
    },
  },
  ref: {
    type: 'Ref',
    args: { qualifiedName: 'String!' },
    resolve: (r: Repo, a) => {
      const q = String(a.qualifiedName);
      const full = q.startsWith('refs/') ? q : `refs/heads/${q}`;
      const oid = r.refs.get(full);
      if (oid === undefined) return null;
      const slash = full.indexOf('/', 5);
      return { repo: r, name: full.slice(slash + 1), prefix: full.slice(0, slash + 1), oid };
    },
  },
  issue: {
    type: 'Issue',
    args: { number: 'Int!' },
    resolve: (r: Repo, a) => {
      const issue = r.issues.get(Number(a.number));
      return issue === undefined ? null : { repo: r, issue };
    },
  },
  issueOrPullRequest: {
    type: 'IssueOrPullRequest',
    args: { number: 'Int!' },
    resolve: (r: Repo, a) => {
      const n = Number(a.number);
      const issue = r.issues.get(n);
      if (issue !== undefined) return { __typename: 'Issue', value: { repo: r, issue } };
      const pull = r.pulls.get(n);
      return pull === undefined ? null : { __typename: 'PullRequest', value: { repo: r, pull } };
    },
  },
  issues: {
    type: 'IssueConnection!',
    args: { ...CONN_ARGS, states: '[IssueState!]', orderBy: 'IssueOrder', labels: '[String!]', filterBy: 'IssueFilters' },
    resolve: (r: Repo, a, ctx) => {
      let list = [...r.issues.values()];
      const filterBy = (a.filterBy ?? {}) as Obj;
      const states = (a.states ?? (Array.isArray(filterBy.states) ? filterBy.states : null)) as string[] | null;
      if (Array.isArray(states) && states.length > 0) list = list.filter((i) => states.includes(i.state.toUpperCase()));
      const labels = (a.labels ?? filterBy.labels) as string[] | null | undefined;
      if (Array.isArray(labels) && labels.length > 0) {
        const wanted = labels.map((l) => l.toLowerCase());
        list = list.filter((i) => wanted.some((w) => i.labels.some((l) => l.toLowerCase() === w)));
      }
      const order = (a.orderBy ?? { field: 'CREATED_AT', direction: 'ASC' }) as Obj;
      const desc = order.direction === 'DESC';
      const key = (i: Issue): string => (order.field === 'UPDATED_AT' ? i.updatedAt : order.field === 'COMMENTS' ? String((r.comments.get(i.number) ?? []).length).padStart(10, '0') : '');
      list.sort((x, y) => {
        const kx = key(x);
        const ky = key(y);
        const primary = kx === ky ? 0 : kx < ky ? -1 : 1;
        const cmp = primary !== 0 ? primary : x.number - y.number;
        return desc ? -cmp : cmp;
      });
      void ctx;
      return connection(list.map((issue) => ({ repo: r, issue })), a);
    },
  },
  pullRequest: {
    type: 'PullRequest',
    args: { number: 'Int!' },
    resolve: (r: Repo, a) => {
      const pull = r.pulls.get(Number(a.number));
      return pull === undefined ? null : { repo: r, pull };
    },
  },
  pullRequests: {
    type: 'PullRequestConnection!',
    args: { ...CONN_ARGS, states: '[PullRequestState!]', orderBy: 'IssueOrder', headRefName: 'String', baseRefName: 'String', labels: '[String!]' },
    resolve: (r: Repo, a) => {
      let list = [...r.pulls.values()];
      const states = a.states as string[] | null | undefined;
      if (Array.isArray(states) && states.length > 0) list = list.filter((p) => states.includes(pullState(p)));
      if (typeof a.headRefName === 'string') list = list.filter((p) => p.head === a.headRefName);
      if (typeof a.baseRefName === 'string') list = list.filter((p) => p.base === a.baseRefName);
      const order = (a.orderBy ?? { field: 'CREATED_AT', direction: 'ASC' }) as Obj;
      const desc = order.direction === 'DESC';
      list.sort((x, y) => (desc ? y.number - x.number : x.number - y.number));
      return connection(list.map((pull) => ({ repo: r, pull })), a);
    },
  },
  labels: {
    type: 'LabelConnection',
    args: { ...CONN_ARGS, query: 'String' },
    resolve: (r: Repo, a) => connection([...r.labels.values()], a),
  },
  label: { type: 'Label', args: { name: 'String!' }, resolve: (r: Repo, a) => r.labels.get(String(a.name).toLowerCase()) ?? null },
};

const issueFields: Record<string, FieldDef> = {
  id: { type: 'ID!', resolve: (x: IssueRef) => issueNodeId(x.issue.id) },
  databaseId: { type: 'Int', resolve: (x: IssueRef) => x.issue.id },
  number: { type: 'Int!', resolve: (x: IssueRef) => x.issue.number },
  title: { type: 'String!', resolve: (x: IssueRef) => x.issue.title },
  body: { type: 'String!', resolve: (x: IssueRef) => x.issue.body },
  bodyText: { type: 'String!', resolve: (x: IssueRef) => x.issue.body },
  state: { type: 'IssueState!', resolve: (x: IssueRef) => x.issue.state.toUpperCase() },
  stateReason: { type: 'IssueStateReason', resolve: (x: IssueRef) => (x.issue.state === 'closed' ? 'COMPLETED' : null) },
  closed: { type: 'Boolean!', resolve: (x: IssueRef) => x.issue.state === 'closed' },
  closedAt: { type: 'DateTime', resolve: (x: IssueRef) => x.issue.closedAt },
  createdAt: { type: 'DateTime!', resolve: (x: IssueRef) => x.issue.createdAt },
  updatedAt: { type: 'DateTime!', resolve: (x: IssueRef) => x.issue.updatedAt },
  url: { type: 'URI!', resolve: (x: IssueRef, _a, ctx) => `${ctx.base}/${x.repo.owner}/${x.repo.name}/issues/${x.issue.number}` },
  author: { type: 'Actor', resolve: (x: IssueRef, _a, ctx) => ({ __typename: 'User', value: userValue(ctx, x.issue.user) }) },
  repository: { type: 'Repository!', resolve: (x: IssueRef) => x.repo },
  locked: { type: 'Boolean!', resolve: () => false },
  labels: {
    type: 'LabelConnection',
    args: CONN_ARGS,
    resolve: (x: IssueRef, a) => connection(x.issue.labels.map((n) => x.repo.labels.get(n.toLowerCase())).filter((l) => l !== undefined), a),
  },
  comments: {
    type: 'IssueCommentConnection!',
    args: CONN_ARGS,
    resolve: (x: IssueRef, a) => connection((x.repo.comments.get(x.issue.number) ?? []).map((comment) => ({ repo: x.repo, comment })), a),
  },
  assignees: { type: 'UserConnection!', args: CONN_ARGS, resolve: (_x: IssueRef, a) => connection([], a) },
};

const pullFields: Record<string, FieldDef> = {
  id: { type: 'ID!', resolve: (x: PullRef) => pullNodeId(x.pull.id) },
  databaseId: { type: 'Int', resolve: (x: PullRef) => x.pull.id },
  number: { type: 'Int!', resolve: (x: PullRef) => x.pull.number },
  title: { type: 'String!', resolve: (x: PullRef) => x.pull.title },
  body: { type: 'String!', resolve: (x: PullRef) => x.pull.body },
  state: { type: 'PullRequestState!', resolve: (x: PullRef) => pullState(x.pull) },
  merged: { type: 'Boolean!', resolve: (x: PullRef) => x.pull.merged },
  mergedAt: { type: 'DateTime', resolve: (x: PullRef) => x.pull.mergedAt },
  closed: { type: 'Boolean!', resolve: (x: PullRef) => x.pull.state === 'closed' },
  closedAt: { type: 'DateTime', resolve: (x: PullRef) => x.pull.closedAt },
  isDraft: { type: 'Boolean!', resolve: () => false },
  createdAt: { type: 'DateTime!', resolve: (x: PullRef) => x.pull.createdAt },
  updatedAt: { type: 'DateTime!', resolve: (x: PullRef) => x.pull.updatedAt },
  url: { type: 'URI!', resolve: (x: PullRef, _a, ctx) => `${ctx.base}/${x.repo.owner}/${x.repo.name}/pull/${x.pull.number}` },
  headRefName: { type: 'String!', resolve: (x: PullRef) => x.pull.head },
  baseRefName: { type: 'String!', resolve: (x: PullRef) => x.pull.base },
  headRefOid: { type: 'GitObjectID!', resolve: (x: PullRef, _a, ctx) => ctx.forge.pullShas(x.repo, x.pull).head },
  baseRefOid: { type: 'GitObjectID!', resolve: (x: PullRef, _a, ctx) => ctx.forge.pullShas(x.repo, x.pull).base },
  headRepository: { type: 'Repository', resolve: (x: PullRef) => x.repo },
  baseRepository: { type: 'Repository', resolve: (x: PullRef) => x.repo },
  repository: { type: 'Repository!', resolve: (x: PullRef) => x.repo },
  mergeable: {
    type: 'MergeableState!',
    resolve: (x: PullRef, _a, ctx) => (x.pull.state !== 'open' ? 'UNKNOWN' : ctx.forge.mergeResult(x.repo, x.pull) === null ? 'CONFLICTING' : 'MERGEABLE'),
  },
  mergeCommit: { type: 'Commit', resolve: (x: PullRef) => (x.pull.mergeCommitSha === null ? null : { oid: x.pull.mergeCommitSha }) },
  author: { type: 'Actor', resolve: (x: PullRef, _a, ctx) => ({ __typename: 'User', value: userValue(ctx, x.pull.user) }) },
  files: {
    type: 'PullRequestChangedFileConnection',
    args: CONN_ARGS,
    resolve: (x: PullRef, a, ctx) =>
      connection(
        ctx.forge.pullFiles(x.repo, x.pull).map((f) => ({ path: f.path, additions: f.additions, deletions: f.deletions, changeType: f.status === 'added' ? 'ADDED' : f.status === 'removed' ? 'DELETED' : 'MODIFIED' })),
        a,
      ),
  },
  changedFiles: { type: 'Int!', resolve: (x: PullRef, _a, ctx) => ctx.forge.pullFiles(x.repo, x.pull).length },
  additions: { type: 'Int!', resolve: (x: PullRef, _a, ctx) => ctx.forge.pullFiles(x.repo, x.pull).reduce((s, f) => s + f.additions, 0) },
  deletions: { type: 'Int!', resolve: (x: PullRef, _a, ctx) => ctx.forge.pullFiles(x.repo, x.pull).reduce((s, f) => s + f.deletions, 0) },
  comments: {
    type: 'IssueCommentConnection!',
    args: CONN_ARGS,
    resolve: (x: PullRef, a) => connection((x.repo.comments.get(x.pull.number) ?? []).map((comment) => ({ repo: x.repo, comment })), a),
  },
  labels: { type: 'LabelConnection', args: CONN_ARGS, resolve: (_x: PullRef, a) => connection([], a) },
};

const commentFields: Record<string, FieldDef> = {
  id: { type: 'ID!', resolve: (x: CommentRef) => commentNodeId(x.comment.id) },
  databaseId: { type: 'Int', resolve: (x: CommentRef) => x.comment.id },
  body: { type: 'String!', resolve: (x: CommentRef) => x.comment.body },
  createdAt: { type: 'DateTime!', resolve: (x: CommentRef) => x.comment.createdAt },
  updatedAt: { type: 'DateTime!', resolve: (x: CommentRef) => x.comment.updatedAt },
  url: { type: 'URI!', resolve: (x: CommentRef, _a, ctx) => `${ctx.base}/${x.repo.owner}/${x.repo.name}/issues/${x.comment.issueNumber}#issuecomment-${x.comment.id}` },
  author: { type: 'Actor', resolve: (x: CommentRef, _a, ctx) => ({ __typename: 'User', value: userValue(ctx, x.comment.user) }) },
};

const userFields: Record<string, FieldDef> = {
  id: { type: 'ID!' },
  databaseId: { type: 'Int' },
  login: { type: 'String!' },
  name: { type: 'String' },
  email: { type: 'String!' },
  url: { type: 'URI!' },
  avatarUrl: { type: 'URI!', args: { size: 'Int' }, resolve: (u: Obj) => `${String(u.url)}.png` },
  repository: {
    type: 'Repository',
    args: { name: 'String!' },
    resolve: (u: Obj, a, ctx) => ctx.forge.getRepo(String(u.login), String(a.name)),
  },
  repositories: {
    type: 'RepositoryConnection!',
    args: CONN_ARGS,
    resolve: (u: Obj, a, ctx) => connection(ctx.forge.listRepos(String(u.login)), a),
  },
};

function enumType(values: readonly string[]): TypeDef {
  return { kind: 'ENUM', enumValues: values };
}

function payload(fields: Record<string, FieldDef>): TypeDef {
  return { kind: 'OBJECT', fields: { clientMutationId: { type: 'String' }, ...fields } };
}

const TYPES: Record<string, TypeDef> = {
  Query: {
    kind: 'OBJECT',
    fields: {
      viewer: { type: 'User!', resolve: (_p, _a, ctx) => userValue(ctx, ctx.forge.viewer.login) },
      user: {
        type: 'User',
        args: { login: 'String!' },
        resolve: (_p, a, ctx) => (String(a.login).toLowerCase() === ctx.forge.viewer.login.toLowerCase() ? userValue(ctx, ctx.forge.viewer.login) : null),
      },
      repositoryOwner: {
        type: 'RepositoryOwner',
        args: { login: 'String!' },
        resolve: (_p, a, ctx) => (String(a.login).toLowerCase() === ctx.forge.viewer.login.toLowerCase() ? { __typename: 'User', value: userValue(ctx, ctx.forge.viewer.login) } : null),
      },
      repository: {
        type: 'Repository',
        args: { owner: 'String!', name: 'String!', followRenames: 'Boolean' },
        resolve: (_p, a, ctx) => ctx.forge.getRepo(String(a.owner), String(a.name)),
      },
      node: { type: 'Node', args: { id: 'ID!' }, resolve: (_p, a, ctx) => nodeById(ctx, a.id) },
      nodes: { type: '[Node]!', args: { ids: '[ID!]!' }, resolve: (_p, a, ctx) => (Array.isArray(a.ids) ? a.ids.map((id) => nodeById(ctx, id)) : []) },
      rateLimit: { type: 'RateLimit', resolve: () => ({ limit: 5000, remaining: 5000, cost: 1, used: 0, resetAt: '2030-01-01T00:00:00Z', nodeCount: 0 }) },
      __type: { type: '__Type', args: { name: 'String!' }, resolve: (_p, a) => (typeDef(String(a.name)) === undefined ? null : { named: String(a.name) }) },
      __schema: { type: '__Schema!', resolve: () => ({}) },
    },
  },
  Mutation: {
    kind: 'OBJECT',
    fields: {
      createIssue: {
        type: 'CreateIssuePayload',
        args: { input: 'CreateIssueInput!' },
        resolve: (_p, a, ctx) => {
          const i = input(a);
          const repo = repoByGlobalId(ctx, i.repositoryId);
          const labels = Array.isArray(i.labelIds) ? (i.labelIds as unknown[]).map((id) => labelNameById(repo, id)) : undefined;
          const issue = mapForgeError(() => ctx.forge.createIssue(repo, { title: i.title, body: i.body, labels }));
          return { clientMutationId: i.clientMutationId ?? null, issue: { repo, issue } };
        },
      },
      updateIssue: {
        type: 'UpdateIssuePayload',
        args: { input: 'UpdateIssueInput!' },
        resolve: (_p, a, ctx) => {
          const i = input(a);
          const ref = issueByGlobalId(ctx, i.id);
          const patch: Obj = {};
          if (i.title !== undefined) patch.title = i.title;
          if (i.body !== undefined) patch.body = i.body;
          if (typeof i.state === 'string') patch.state = i.state.toLowerCase();
          mapForgeError(() => ctx.forge.updateIssue(ref.repo, ref.issue, patch));
          return { clientMutationId: i.clientMutationId ?? null, issue: ref };
        },
      },
      closeIssue: {
        type: 'CloseIssuePayload',
        args: { input: 'CloseIssueInput!' },
        resolve: (_p, a, ctx) => {
          const i = input(a);
          const ref = issueByGlobalId(ctx, i.issueId);
          mapForgeError(() => ctx.forge.updateIssue(ref.repo, ref.issue, { state: 'closed' }));
          return { clientMutationId: i.clientMutationId ?? null, issue: ref };
        },
      },
      reopenIssue: {
        type: 'ReopenIssuePayload',
        args: { input: 'ReopenIssueInput!' },
        resolve: (_p, a, ctx) => {
          const i = input(a);
          const ref = issueByGlobalId(ctx, i.issueId);
          mapForgeError(() => ctx.forge.updateIssue(ref.repo, ref.issue, { state: 'open' }));
          return { clientMutationId: i.clientMutationId ?? null, issue: ref };
        },
      },
      addComment: {
        type: 'AddCommentPayload',
        args: { input: 'AddCommentInput!' },
        resolve: (_p, a, ctx) => {
          const i = input(a);
          const decoded = typeof i.subjectId === 'string' ? decodeNodeId(i.subjectId) : null;
          let repo: Repo;
          let number: number;
          let subject: Typed;
          if (decoded?.kind === 'PullRequest') {
            const ref = pullByGlobalId(ctx, i.subjectId);
            repo = ref.repo;
            number = ref.pull.number;
            subject = { __typename: 'PullRequest', value: ref };
          } else {
            const ref = issueByGlobalId(ctx, i.subjectId);
            repo = ref.repo;
            number = ref.issue.number;
            subject = { __typename: 'Issue', value: ref };
          }
          const comment = mapForgeError(() => ctx.forge.addComment(repo, number, i.body));
          const node = { repo, comment };
          const index = (repo.comments.get(number) ?? []).length - 1;
          return { clientMutationId: i.clientMutationId ?? null, commentEdge: { node, cursor: encodeCursor(index) }, subject, timelineEdge: { node: { __typename: 'IssueComment', value: node }, cursor: encodeCursor(index) } };
        },
      },
      createPullRequest: {
        type: 'CreatePullRequestPayload',
        args: { input: 'CreatePullRequestInput!' },
        resolve: (_p, a, ctx) => {
          const i = input(a);
          const repo = repoByGlobalId(ctx, i.repositoryId);
          const pull = mapForgeError(() => ctx.forge.createPull(repo, { title: i.title, body: i.body, head: i.headRefName, base: i.baseRefName }));
          return { clientMutationId: i.clientMutationId ?? null, pullRequest: { repo, pull } };
        },
      },
      mergePullRequest: {
        type: 'MergePullRequestPayload',
        args: { input: 'MergePullRequestInput!' },
        resolve: (_p, a, ctx) => {
          const i = input(a);
          const ref = pullByGlobalId(ctx, i.pullRequestId);
          if (i.mergeMethod !== undefined && i.mergeMethod !== null && !['MERGE', 'SQUASH', 'REBASE'].includes(String(i.mergeMethod))) throw new GraphQLFieldError('mergeMethod is invalid');
          mapForgeError(() => ctx.forge.mergePull(ref.repo, ref.pull, { commit_title: i.commitHeadline, commit_message: i.commitBody, sha: i.expectedHeadOid }));
          return { clientMutationId: i.clientMutationId ?? null, pullRequest: ref };
        },
      },
      closePullRequest: {
        type: 'ClosePullRequestPayload',
        args: { input: 'ClosePullRequestInput!' },
        resolve: (_p, a, ctx) => {
          const i = input(a);
          const ref = pullByGlobalId(ctx, i.pullRequestId);
          mapForgeError(() => ctx.forge.updatePull(ref.repo, ref.pull, { state: 'closed' }));
          return { clientMutationId: i.clientMutationId ?? null, pullRequest: ref };
        },
      },
    },
  },
  Node: { kind: 'INTERFACE', fields: { id: { type: 'ID!' } }, possibleTypes: ['Repository', 'Issue', 'PullRequest', 'IssueComment', 'User'] },
  Actor: { kind: 'INTERFACE', fields: { login: { type: 'String!' }, url: { type: 'URI!' }, avatarUrl: { type: 'URI!', args: { size: 'Int' } } }, possibleTypes: ['User'] },
  RepositoryOwner: { kind: 'INTERFACE', fields: { id: { type: 'ID!' }, login: { type: 'String!' }, url: { type: 'URI!' } }, possibleTypes: ['User'] },
  IssueOrPullRequest: { kind: 'UNION', possibleTypes: ['Issue', 'PullRequest'] },
  User: { kind: 'OBJECT', fields: userFields, interfaces: ['Node', 'Actor', 'RepositoryOwner'] },
  Repository: { kind: 'OBJECT', fields: repositoryFields, interfaces: ['Node'] },
  Issue: { kind: 'OBJECT', fields: issueFields, interfaces: ['Node'] },
  PullRequest: { kind: 'OBJECT', fields: pullFields, interfaces: ['Node'] },
  IssueComment: { kind: 'OBJECT', fields: commentFields, interfaces: ['Node'] },
  Label: {
    kind: 'OBJECT',
    fields: {
      id: { type: 'ID!', resolve: (l: Obj) => Buffer.from(`010:Label${String(l.id)}`).toString('base64') },
      name: { type: 'String!' },
      color: { type: 'String!' },
      description: { type: 'String' },
    },
    interfaces: ['Node'],
  },
  Ref: {
    kind: 'OBJECT',
    fields: {
      id: { type: 'ID!', resolve: (r: Obj) => Buffer.from(`03:Ref${(r.repo as Repo).id}:${String(r.prefix)}${String(r.name)}`).toString('base64') },
      name: { type: 'String!' },
      prefix: { type: 'String!' },
      target: { type: 'Commit', resolve: (r: Obj) => ({ oid: r.oid }) },
    },
  },
  Commit: {
    kind: 'OBJECT',
    fields: {
      oid: { type: 'GitObjectID!' },
      abbreviatedOid: { type: 'String!', resolve: (c: Obj) => String(c.oid).slice(0, 7) },
      message: { type: 'String!', resolve: (c: Obj, _a, ctx) => ctx.forge.objects.commit(String(c.oid))?.message ?? '' },
      committedDate: { type: 'DateTime!', resolve: (c: Obj, _a, ctx) => iso(ctx.forge.objects.commit(String(c.oid))?.committer.time ?? 0) },
    },
  },
  PullRequestChangedFile: {
    kind: 'OBJECT',
    fields: { path: { type: 'String!' }, additions: { type: 'Int!' }, deletions: { type: 'Int!' }, changeType: { type: 'PatchStatus!' } },
  },
  PageInfo: {
    kind: 'OBJECT',
    fields: { hasNextPage: { type: 'Boolean!' }, hasPreviousPage: { type: 'Boolean!' }, startCursor: { type: 'String' }, endCursor: { type: 'String' } },
  },
  RateLimit: {
    kind: 'OBJECT',
    fields: { limit: { type: 'Int!' }, remaining: { type: 'Int!' }, cost: { type: 'Int!' }, used: { type: 'Int!' }, resetAt: { type: 'DateTime!' }, nodeCount: { type: 'Int!' } },
  },
  IssueConnection: { kind: 'OBJECT', fields: connectionFields('Issue', 'IssueEdge') },
  IssueEdge: { kind: 'OBJECT', fields: { node: { type: 'Issue' }, cursor: { type: 'String!' } } },
  PullRequestConnection: { kind: 'OBJECT', fields: connectionFields('PullRequest', 'PullRequestEdge') },
  PullRequestEdge: { kind: 'OBJECT', fields: { node: { type: 'PullRequest' }, cursor: { type: 'String!' } } },
  PullRequestChangedFileConnection: { kind: 'OBJECT', fields: connectionFields('PullRequestChangedFile', 'PullRequestChangedFileEdge') },
  PullRequestChangedFileEdge: { kind: 'OBJECT', fields: { node: { type: 'PullRequestChangedFile' }, cursor: { type: 'String!' } } },
  IssueCommentConnection: { kind: 'OBJECT', fields: connectionFields('IssueComment', 'IssueCommentEdge') },
  IssueCommentEdge: { kind: 'OBJECT', fields: { node: { type: 'IssueComment' }, cursor: { type: 'String!' } } },
  LabelConnection: { kind: 'OBJECT', fields: connectionFields('Label', 'LabelEdge') },
  LabelEdge: { kind: 'OBJECT', fields: { node: { type: 'Label' }, cursor: { type: 'String!' } } },
  UserConnection: { kind: 'OBJECT', fields: connectionFields('User', 'UserEdge') },
  UserEdge: { kind: 'OBJECT', fields: { node: { type: 'User' }, cursor: { type: 'String!' } } },
  RepositoryConnection: { kind: 'OBJECT', fields: connectionFields('Repository', 'RepositoryEdge') },
  RepositoryEdge: { kind: 'OBJECT', fields: { node: { type: 'Repository' }, cursor: { type: 'String!' } } },
  TimelineEdge: { kind: 'OBJECT', fields: { node: { type: 'Node' }, cursor: { type: 'String!' } } },
  CreateIssuePayload: payload({ issue: { type: 'Issue' } }),
  UpdateIssuePayload: payload({ issue: { type: 'Issue' } }),
  CloseIssuePayload: payload({ issue: { type: 'Issue' } }),
  ReopenIssuePayload: payload({ issue: { type: 'Issue' } }),
  AddCommentPayload: payload({ commentEdge: { type: 'IssueCommentEdge' }, subject: { type: 'Node' }, timelineEdge: { type: 'TimelineEdge' } }),
  CreatePullRequestPayload: payload({ pullRequest: { type: 'PullRequest' } }),
  MergePullRequestPayload: payload({ pullRequest: { type: 'PullRequest' } }),
  ClosePullRequestPayload: payload({ pullRequest: { type: 'PullRequest' } }),
  CreateIssueInput: { kind: 'INPUT_OBJECT', inputFields: { repositoryId: 'ID!', title: 'String!', body: 'String', labelIds: '[ID!]', assigneeIds: '[ID!]', clientMutationId: 'String' } },
  UpdateIssueInput: { kind: 'INPUT_OBJECT', inputFields: { id: 'ID!', title: 'String', body: 'String', state: 'IssueState', clientMutationId: 'String' } },
  CloseIssueInput: { kind: 'INPUT_OBJECT', inputFields: { issueId: 'ID!', stateReason: 'IssueClosedStateReason', clientMutationId: 'String' } },
  ReopenIssueInput: { kind: 'INPUT_OBJECT', inputFields: { issueId: 'ID!', clientMutationId: 'String' } },
  AddCommentInput: { kind: 'INPUT_OBJECT', inputFields: { subjectId: 'ID!', body: 'String!', clientMutationId: 'String' } },
  CreatePullRequestInput: {
    kind: 'INPUT_OBJECT',
    inputFields: { repositoryId: 'ID!', baseRefName: 'String!', headRefName: 'String!', title: 'String!', body: 'String', draft: 'Boolean', maintainerCanModify: 'Boolean', clientMutationId: 'String' },
  },
  MergePullRequestInput: {
    kind: 'INPUT_OBJECT',
    inputFields: { pullRequestId: 'ID!', mergeMethod: 'PullRequestMergeMethod', commitHeadline: 'String', commitBody: 'String', expectedHeadOid: 'GitObjectID', clientMutationId: 'String' },
  },
  ClosePullRequestInput: { kind: 'INPUT_OBJECT', inputFields: { pullRequestId: 'ID!', clientMutationId: 'String' } },
  IssueOrder: { kind: 'INPUT_OBJECT', inputFields: { field: 'IssueOrderField!', direction: 'OrderDirection!' } },
  IssueFilters: { kind: 'INPUT_OBJECT', inputFields: { states: '[IssueState!]', labels: '[String!]', assignee: 'String', createdBy: 'String' } },
  IssueState: enumType(['OPEN', 'CLOSED']),
  IssueStateReason: enumType(['COMPLETED', 'NOT_PLANNED', 'REOPENED']),
  IssueClosedStateReason: enumType(['COMPLETED', 'NOT_PLANNED']),
  PullRequestState: enumType(['OPEN', 'CLOSED', 'MERGED']),
  MergeableState: enumType(['MERGEABLE', 'CONFLICTING', 'UNKNOWN']),
  PullRequestMergeMethod: enumType(['MERGE', 'SQUASH', 'REBASE']),
  RepositoryPermission: enumType(['ADMIN', 'MAINTAIN', 'WRITE', 'TRIAGE', 'READ']),
  RepositoryVisibility: enumType(['PRIVATE', 'PUBLIC', 'INTERNAL']),
  SearchType: enumType(['ISSUE', 'ISSUE_ADVANCED', 'REPOSITORY', 'USER', 'DISCUSSION']),
  IssueOrderField: enumType(['CREATED_AT', 'UPDATED_AT', 'COMMENTS']),
  OrderDirection: enumType(['ASC', 'DESC']),
  PatchStatus: enumType(['ADDED', 'DELETED', 'RENAMED', 'COPIED', 'MODIFIED', 'CHANGED']),
  ID: { kind: 'SCALAR' },
  String: { kind: 'SCALAR' },
  Int: { kind: 'SCALAR' },
  Float: { kind: 'SCALAR' },
  Boolean: { kind: 'SCALAR' },
  DateTime: { kind: 'SCALAR' },
  URI: { kind: 'SCALAR' },
  GitObjectID: { kind: 'SCALAR' },
  GitSSHRemote: { kind: 'SCALAR' },
};

function labelNameById(repo: Repo, id: unknown): string {
  const decoded = typeof id === 'string' ? decodeNodeId(id) : null;
  for (const label of repo.labels.values()) if (decoded !== null && label.id === decoded.id) return label.name;
  throw new GraphQLFieldError(`Could not resolve to a node with the global id of '${String(id)}'`, 'NOT_FOUND');
}

// ---------------------------------------------------------------------------------------------
// Introspection types (built on the same machinery)

interface TypeRefValue {
  readonly kind: 'NON_NULL' | 'LIST' | 'NAMED';
  readonly ofType?: TypeRefValue;
  readonly named?: string;
}

function parseTypeRef(ref: string): TypeRefValue {
  if (ref.endsWith('!')) return { kind: 'NON_NULL', ofType: parseTypeRef(ref.slice(0, -1)) };
  if (ref.startsWith('[')) return { kind: 'LIST', ofType: parseTypeRef(ref.slice(1, -1)) };
  return { kind: 'NAMED', named: ref };
}

function namedOf(ref: string): string {
  return ref.replace(/[[\]!]/g, '');
}

/** A __Type value: either a named type ({named}) or a wrapper ({wrapper}). */
type MetaType = { named: string } | { wrapper: TypeRefValue };

function metaOf(ref: TypeRefValue): MetaType {
  return ref.kind === 'NAMED' ? { named: ref.named as string } : { wrapper: ref };
}

const META_TYPES: Record<string, TypeDef> = {
  __Schema: {
    kind: 'OBJECT',
    fields: {
      types: { type: '[__Type!]!', resolve: () => Object.keys(TYPES).sort().map((named) => ({ named })) },
      queryType: { type: '__Type!', resolve: () => ({ named: 'Query' }) },
      mutationType: { type: '__Type', resolve: () => ({ named: 'Mutation' }) },
      subscriptionType: { type: '__Type', resolve: () => null },
      directives: { type: '[__Directive!]!', resolve: () => [] },
      description: { type: 'String', resolve: () => null },
    },
  },
  __Type: {
    kind: 'OBJECT',
    fields: {
      kind: { type: '__TypeKind!', resolve: (t: MetaType) => ('named' in t ? (TYPES[t.named] as TypeDef).kind : t.wrapper.kind) },
      name: { type: 'String', resolve: (t: MetaType) => ('named' in t ? t.named : null) },
      description: { type: 'String', resolve: (t: MetaType) => ('named' in t ? (TYPES[t.named]?.description ?? null) : null) },
      specifiedByURL: { type: 'String', resolve: () => null },
      fields: {
        type: '[__Field!]',
        args: { includeDeprecated: 'Boolean' },
        resolve: (t: MetaType) => {
          if (!('named' in t)) return null;
          const def = TYPES[t.named] as TypeDef;
          if (def.kind !== 'OBJECT' && def.kind !== 'INTERFACE') return null;
          return Object.entries(def.fields ?? {}).map(([name, f]) => ({ name, f }));
        },
      },
      interfaces: { type: '[__Type!]', resolve: (t: MetaType) => ('named' in t && TYPES[t.named]?.kind === 'OBJECT' ? (TYPES[t.named]?.interfaces ?? []).map((named) => ({ named })) : null) },
      possibleTypes: { type: '[__Type!]', resolve: (t: MetaType) => ('named' in t && TYPES[t.named]?.possibleTypes !== undefined ? (TYPES[t.named]?.possibleTypes ?? []).map((named) => ({ named })) : null) },
      enumValues: {
        type: '[__EnumValue!]',
        args: { includeDeprecated: 'Boolean' },
        resolve: (t: MetaType) => ('named' in t && TYPES[t.named]?.kind === 'ENUM' ? (TYPES[t.named]?.enumValues ?? []).map((name) => ({ name })) : null),
      },
      inputFields: {
        type: '[__InputValue!]',
        args: { includeDeprecated: 'Boolean' },
        resolve: (t: MetaType) => ('named' in t && TYPES[t.named]?.kind === 'INPUT_OBJECT' ? Object.entries(TYPES[t.named]?.inputFields ?? {}).map(([name, type]) => ({ name, type })) : null),
      },
      ofType: { type: '__Type', resolve: (t: MetaType) => ('wrapper' in t && t.wrapper.ofType !== undefined ? metaOf(t.wrapper.ofType) : null) },
      isOneOf: { type: 'Boolean', resolve: () => false },
    },
  },
  __Field: {
    kind: 'OBJECT',
    fields: {
      name: { type: 'String!' },
      description: { type: 'String', resolve: (x: { f: FieldDef }) => x.f.description ?? null },
      args: {
        type: '[__InputValue!]!',
        args: { includeDeprecated: 'Boolean' },
        resolve: (x: { f: FieldDef }) => Object.entries(x.f.args ?? {}).map(([name, type]) => ({ name, type })),
      },
      type: { type: '__Type!', resolve: (x: { f: FieldDef }) => metaOf(parseTypeRef(x.f.type)) },
      isDeprecated: { type: 'Boolean!', resolve: () => false },
      deprecationReason: { type: 'String', resolve: () => null },
    },
  },
  __InputValue: {
    kind: 'OBJECT',
    fields: {
      name: { type: 'String!' },
      description: { type: 'String', resolve: () => null },
      type: { type: '__Type!', resolve: (x: { type: string }) => metaOf(parseTypeRef(x.type)) },
      defaultValue: { type: 'String', resolve: () => null },
      isDeprecated: { type: 'Boolean!', resolve: () => false },
      deprecationReason: { type: 'String', resolve: () => null },
    },
  },
  __EnumValue: {
    kind: 'OBJECT',
    fields: {
      name: { type: 'String!' },
      description: { type: 'String', resolve: () => null },
      isDeprecated: { type: 'Boolean!', resolve: () => false },
      deprecationReason: { type: 'String', resolve: () => null },
    },
  },
  __Directive: { kind: 'OBJECT', fields: { name: { type: 'String!' }, description: { type: 'String' }, locations: { type: '[String!]!' }, args: { type: '[__InputValue!]!' } } },
  __TypeKind: enumType(['SCALAR', 'OBJECT', 'INTERFACE', 'UNION', 'ENUM', 'INPUT_OBJECT', 'LIST', 'NON_NULL']),
};

function typeDef(name: string): TypeDef | undefined {
  return TYPES[name] ?? META_TYPES[name];
}

// ---------------------------------------------------------------------------------------------
// Execution

interface Exec {
  readonly ctx: Ctx;
  readonly doc: Document;
  readonly variables: Readonly<Record<string, unknown>>;
  readonly errors: Obj[];
}

function skipped(exec: Exec, directives: readonly Directive[]): boolean {
  for (const d of directives) {
    const cond = d.args.if === undefined ? undefined : valueOf(d.args.if, exec.variables);
    if (d.name === 'skip' && cond === true) return true;
    if (d.name === 'include' && cond === false) return true;
  }
  return false;
}

function typeMatches(condition: string | null, runtime: string): boolean {
  if (condition === null || condition === runtime) return true;
  const def = typeDef(condition);
  return def !== undefined && (def.kind === 'INTERFACE' || def.kind === 'UNION') && (def.possibleTypes ?? []).includes(runtime);
}

/** Collect the fields a selection set asks of one runtime type, in document order, merged by response key. */
function collectFields(exec: Exec, runtime: string, selections: readonly Selection[], out = new Map<string, Extract<Selection, { kind: 'field' }>[]>(), visited = new Set<string>()): Map<string, Extract<Selection, { kind: 'field' }>[]> {
  for (const sel of selections) {
    if (skipped(exec, sel.directives)) continue;
    if (sel.kind === 'field') {
      const key = sel.alias ?? sel.name;
      const list = out.get(key) ?? [];
      list.push(sel);
      out.set(key, list);
    } else if (sel.kind === 'inline') {
      if (sel.typeCondition !== null && typeDef(sel.typeCondition) === undefined) throw new ValidationError(`Fragment on unknown type '${sel.typeCondition}'`);
      if (typeMatches(sel.typeCondition, runtime)) collectFields(exec, runtime, sel.selections, out, visited);
    } else {
      const frag = exec.doc.fragments.get(sel.name);
      if (frag === undefined) throw new ValidationError(`Fragment ${sel.name} was used, but not defined`);
      if (visited.has(sel.name)) continue;
      visited.add(sel.name);
      if (typeMatches(frag.typeCondition, runtime)) collectFields(exec, runtime, frag.selections, out, visited);
    }
  }
  return out;
}

function executeSelections(exec: Exec, typeName: string, parent: unknown, selections: readonly Selection[], path: (string | number)[]): Obj {
  const result: Obj = {};
  const def = typeDef(typeName) as TypeDef;
  for (const [key, fields] of collectFields(exec, typeName, selections)) {
    const field = fields[0] as Extract<Selection, { kind: 'field' }>;
    if (field.name === '__typename') {
      result[key] = typeName;
      continue;
    }
    const fdef = def.fields?.[field.name];
    if (fdef === undefined) throw new ValidationError(`Field '${field.name}' doesn't exist on type '${typeName}'`);
    for (const argName of Object.keys(field.args)) {
      if (fdef.args === undefined || fdef.args[argName] === undefined) throw new ValidationError(`Field '${field.name}' doesn't accept argument '${argName}'`);
    }
    const args: Args = {};
    for (const [name, node] of Object.entries(field.args)) args[name] = valueOf(node, exec.variables);
    const merged = fields.flatMap((f) => f.selections ?? []);
    const fieldPath = [...path, key];
    let value: unknown;
    try {
      value = fdef.resolve !== undefined ? fdef.resolve(parent, args, exec.ctx) : (parent as Obj | null)?.[field.name];
    } catch (err) {
      if (err instanceof ValidationError) throw err;
      if (err instanceof GraphQLFieldError) {
        exec.errors.push({ type: err.type, path: fieldPath, locations: [], message: err.message });
        result[key] = null;
        continue;
      }
      throw err;
    }
    result[key] = completeValue(exec, fdef.type, value, merged, fieldPath, field.name);
  }
  return result;
}

function completeValue(exec: Exec, typeRef: string, value: unknown, selections: readonly Selection[], path: (string | number)[], fieldName: string): unknown {
  if (value === null || value === undefined) return null;
  const ref = typeRef.endsWith('!') ? typeRef.slice(0, -1) : typeRef;
  if (ref.startsWith('[')) {
    if (!Array.isArray(value)) return null;
    const inner = ref.slice(1, -1);
    return value.map((item, i) => completeValue(exec, inner, item, selections, [...path, i], fieldName));
  }
  const def = typeDef(ref);
  if (def === undefined) return value;
  if (def.kind === 'SCALAR' || def.kind === 'ENUM') {
    if (selections.length > 0) throw new ValidationError(`Selections can't be made on scalars (field '${fieldName}' returns ${ref})`);
    return value;
  }
  if (selections.length === 0) throw new ValidationError(`Field must have selections (field '${fieldName}' returns ${ref} but has no selections)`);
  if (def.kind === 'INTERFACE' || def.kind === 'UNION') {
    const typed = value as Typed;
    return executeSelections(exec, typed.__typename, typed.value, selections, path);
  }
  return executeSelections(exec, ref, value, selections, path);
}

function coerceVariables(op: Operation, provided: unknown): Record<string, unknown> {
  const given = typeof provided === 'object' && provided !== null && !Array.isArray(provided) ? (provided as Obj) : {};
  const out: Record<string, unknown> = {};
  for (const v of op.variables) {
    if (Object.prototype.hasOwnProperty.call(given, v.name)) out[v.name] = given[v.name];
    else if (v.defaultValue !== null) out[v.name] = valueOf(v.defaultValue, {});
    else if (v.type.endsWith('!')) throw new ValidationError(`Variable $${v.name} of type ${v.type} was provided invalid value`);
    else out[v.name] = null;
    if (typeDef(namedOf(v.type)) === undefined) throw new ValidationError(`${namedOf(v.type)} isn't a defined input type (on $${v.name})`);
  }
  return out;
}

export function executeGraphQL(ctx: Ctx, request: unknown): Obj {
  if (typeof request !== 'object' || request === null || Array.isArray(request)) return { errors: [{ message: 'A query attribute must be specified and must be a string.' }] };
  const { query, variables, operationName } = request as Obj;
  if (typeof query !== 'string' || query.trim() === '') return { errors: [{ message: 'A query attribute must be specified and must be a string.' }] };
  let doc: Document;
  try {
    doc = parseDocument(query);
  } catch (err) {
    if (err instanceof GraphQLSyntaxError) return { errors: [{ message: err.message, locations: [] }] };
    throw err;
  }
  let op: Operation | undefined;
  if (typeof operationName === 'string' && operationName !== '') {
    op = doc.operations.find((o) => o.name === operationName);
    if (op === undefined) return { errors: [{ message: `No operation named "${operationName}"` }] };
  } else if (doc.operations.length === 1) {
    op = doc.operations[0];
  } else {
    return { errors: [{ message: 'An operation name is required when the document holds several operations' }] };
  }
  const operation = op as Operation;
  if (operation.operation === 'subscription') return { errors: [{ message: 'Subscriptions are not supported' }] };
  const errors: Obj[] = [];
  try {
    const vars = coerceVariables(operation, variables);
    const exec: Exec = { ctx, doc, variables: vars, errors };
    const root = operation.operation === 'mutation' ? 'Mutation' : 'Query';
    const data = executeSelections(exec, root, {}, operation.selections, []);
    return errors.length > 0 ? { data, errors } : { data };
  } catch (err) {
    if (err instanceof ValidationError) return { errors: [{ message: err.message, locations: [], extensions: { code: 'undefinedField' } }] };
    throw err;
  }
}

export async function handleGraphQL(forge: Forge, req: IncomingMessage, res: ServerResponse, base: string): Promise<void> {
  const raw = await readBody(req);
  let body: unknown;
  try {
    body = JSON.parse(raw.toString('utf8'));
  } catch {
    sendJson(res, 400, { message: 'Problems parsing JSON', errors: [{ message: 'Problems parsing JSON' }] });
    return;
  }
  const ctx: Ctx = { forge, base };
  if (Array.isArray(body)) {
    sendJson(res, 200, body.map((entry) => executeGraphQL(ctx, entry)));
    return;
  }
  sendJson(res, 200, executeGraphQL(ctx, body));
}
