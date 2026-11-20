// Model Context Protocol at POST /mcp: JSON-RPC 2.0 over Streamable HTTP, answering application/json.
// Speaks revision 2026-07-28 (server/discover, per-request _meta, no session) and the handshake
// revisions (initialize + notifications/initialized). The tools call the same Forge services as REST
// and answer REST-shaped JSON, so an object read over either surface has the same id and fields.

import type { IncomingMessage, ServerResponse } from 'node:http';
import { Forge, ForgeError, type Repo } from '../store.ts';
import { readBody } from '../http.ts';
import { issueJson, pullJson, repoJson } from './present.ts';

type Obj = Record<string, unknown>;

const SERVER_INFO = { name: 'forge-mcp', title: 'Forge MCP', version: '1.0.0' };
const MODERN = '2026-07-28';
const SUPPORTED_VERSIONS = [MODERN, '2025-11-25', '2025-06-18', '2025-03-26', '2024-11-05'];
const CAPABILITIES = { tools: { listChanged: false } };

const PARSE_ERROR = -32700;
const INVALID_REQUEST = -32600;
const METHOD_NOT_FOUND = -32601;
const INVALID_PARAMS = -32602;

class RpcError extends Error {
  readonly code: number;
  constructor(code: number, message: string) {
    super(message);
    this.code = code;
  }
}

/** A tool failure reported as a result with isError: true (the model sees it; the transport succeeded). */
class ToolError extends Error {}

interface ToolDef {
  readonly name: string;
  readonly title: string;
  readonly description: string;
  readonly inputSchema: Obj;
  readonly readOnly: boolean;
  readonly run: (forge: Forge, base: string, args: Obj) => unknown;
}

const str = (description: string): Obj => ({ type: 'string', description });
const int = (description: string): Obj => ({ type: 'integer', minimum: 1, description });
const OWNER = str('Repository owner (the account login)');
const REPO = str('Repository name');

function requireString(args: Obj, key: string): string {
  const value = args[key];
  if (typeof value !== 'string' || value === '') throw new ToolError(`missing required parameter: ${key}`);
  return value;
}

function requireInt(args: Obj, key: string): number {
  const value = args[key];
  const n = typeof value === 'string' && /^\d+$/.test(value) ? Number(value) : value;
  if (typeof n !== 'number' || !Number.isInteger(n) || n < 1) throw new ToolError(`missing required parameter: ${key}`);
  return n;
}

function repoOf(forge: Forge, args: Obj): Repo {
  const owner = requireString(args, 'owner');
  const name = requireString(args, 'repo');
  const repo = forge.getRepo(owner, name);
  if (repo === null) throw new ToolError(`repository ${owner}/${name} not found`);
  return repo;
}

/** The tools, in the fixed order tools/list answers them. */
const TOOLS: readonly ToolDef[] = [
  {
    name: 'create_repo',
    title: 'Create repository',
    description: 'Create a new repository owned by the authenticated user. Returns the repository object.',
    inputSchema: { type: 'object', properties: { name: str('Repository name'), description: str('Repository description'), private: { type: 'boolean', description: 'Whether the repository is private' } }, required: ['name'], additionalProperties: false },
    readOnly: false,
    run: (forge, base, args) => {
      const name = requireString(args, 'name');
      const repo = forge.createRepo({ name, description: args.description, private: args.private });
      return repoJson(forge, base, repo);
    },
  },
  {
    name: 'get_repo',
    title: 'Get repository',
    description: 'Get a repository by owner and name. Returns the repository object.',
    inputSchema: { type: 'object', properties: { owner: OWNER, repo: REPO }, required: ['owner', 'repo'], additionalProperties: false },
    readOnly: true,
    run: (forge, base, args) => repoJson(forge, base, repoOf(forge, args)),
  },
  {
    name: 'list_repos',
    title: 'List repositories',
    description: 'List the repositories the authenticated user owns.',
    inputSchema: { type: 'object', properties: {}, additionalProperties: false },
    readOnly: true,
    run: (forge, base) => forge.listRepos(forge.viewer.login).map((r) => repoJson(forge, base, r)),
  },
  {
    name: 'create_issue',
    title: 'Create issue',
    description: 'Open a new issue in a repository. Labels must already exist in the repository.',
    inputSchema: {
      type: 'object',
      properties: { owner: OWNER, repo: REPO, title: str('Issue title'), body: str('Issue body (markdown)'), labels: { type: 'array', items: { type: 'string' }, description: 'Existing label names' } },
      required: ['owner', 'repo', 'title'],
      additionalProperties: false,
    },
    readOnly: false,
    run: (forge, base, args) => {
      const repo = repoOf(forge, args);
      const title = requireString(args, 'title');
      const issue = forge.createIssue(repo, { title, body: args.body, labels: args.labels });
      return issueJson(forge, base, repo, issue);
    },
  },
  {
    name: 'get_issue',
    title: 'Get issue',
    description: 'Get one issue of a repository by its number.',
    inputSchema: { type: 'object', properties: { owner: OWNER, repo: REPO, issue_number: int('Issue number') }, required: ['owner', 'repo', 'issue_number'], additionalProperties: false },
    readOnly: true,
    run: (forge, base, args) => {
      const repo = repoOf(forge, args);
      return issueJson(forge, base, repo, forge.requireIssue(repo, requireInt(args, 'issue_number')));
    },
  },
  {
    name: 'list_issues',
    title: 'List issues',
    description: 'List the issues of a repository, newest first. state is open (default), closed or all.',
    inputSchema: {
      type: 'object',
      properties: { owner: OWNER, repo: REPO, state: { type: 'string', enum: ['open', 'closed', 'all'], description: 'Issue state filter' } },
      required: ['owner', 'repo'],
      additionalProperties: false,
    },
    readOnly: true,
    run: (forge, base, args) => {
      const repo = repoOf(forge, args);
      const state = typeof args.state === 'string' ? args.state.toLowerCase() : 'open';
      if (!['open', 'closed', 'all'].includes(state)) throw new ToolError('state must be open, closed or all');
      return forge.listIssues(repo, { state, direction: 'desc' }).map((i) => issueJson(forge, base, repo, i));
    },
  },
  {
    name: 'update_issue',
    title: 'Update issue',
    description: 'Update an issue: its title, body or state (open or closed).',
    inputSchema: {
      type: 'object',
      properties: { owner: OWNER, repo: REPO, issue_number: int('Issue number'), title: str('New title'), body: str('New body'), state: { type: 'string', enum: ['open', 'closed'], description: 'New state' } },
      required: ['owner', 'repo', 'issue_number'],
      additionalProperties: false,
    },
    readOnly: false,
    run: (forge, base, args) => {
      const repo = repoOf(forge, args);
      const issue = forge.requireIssue(repo, requireInt(args, 'issue_number'));
      const patch: Obj = {};
      for (const key of ['title', 'body', 'state']) if (args[key] !== undefined) patch[key] = args[key];
      forge.updateIssue(repo, issue, patch);
      return issueJson(forge, base, repo, issue);
    },
  },
  {
    name: 'add_issue_comment',
    title: 'Comment on issue',
    description: 'Add a comment to an issue or pull request.',
    inputSchema: { type: 'object', properties: { owner: OWNER, repo: REPO, issue_number: int('Issue or pull request number'), body: str('Comment body') }, required: ['owner', 'repo', 'issue_number', 'body'], additionalProperties: false },
    readOnly: false,
    run: (forge, _base, args) => {
      const repo = repoOf(forge, args);
      const comment = forge.addComment(repo, requireInt(args, 'issue_number'), requireString(args, 'body'));
      return { id: comment.id, body: comment.body, user: { login: comment.user }, created_at: comment.createdAt };
    },
  },
  {
    name: 'create_pull_request',
    title: 'Create pull request',
    description: 'Open a pull request from a head branch into a base branch of the same repository.',
    inputSchema: {
      type: 'object',
      properties: { owner: OWNER, repo: REPO, title: str('Pull request title'), body: str('Pull request description'), head: str('Branch with the changes'), base: str('Branch to merge into') },
      required: ['owner', 'repo', 'title', 'head', 'base'],
      additionalProperties: false,
    },
    readOnly: false,
    run: (forge, base, args) => {
      const repo = repoOf(forge, args);
      const pull = forge.createPull(repo, { title: requireString(args, 'title'), body: args.body, head: requireString(args, 'head'), base: requireString(args, 'base') });
      return pullJson(forge, base, repo, pull);
    },
  },
  {
    name: 'get_pull_request',
    title: 'Get pull request',
    description: 'Get one pull request by its number.',
    inputSchema: { type: 'object', properties: { owner: OWNER, repo: REPO, pull_number: int('Pull request number') }, required: ['owner', 'repo', 'pull_number'], additionalProperties: false },
    readOnly: true,
    run: (forge, base, args) => {
      const repo = repoOf(forge, args);
      return pullJson(forge, base, repo, forge.requirePull(repo, requireInt(args, 'pull_number')));
    },
  },
  {
    name: 'list_pull_requests',
    title: 'List pull requests',
    description: 'List the pull requests of a repository, newest first. state is open (default), closed or all.',
    inputSchema: { type: 'object', properties: { owner: OWNER, repo: REPO, state: { type: 'string', enum: ['open', 'closed', 'all'], description: 'Pull request state filter' } }, required: ['owner', 'repo'], additionalProperties: false },
    readOnly: true,
    run: (forge, base, args) => {
      const repo = repoOf(forge, args);
      const state = typeof args.state === 'string' ? args.state.toLowerCase() : 'open';
      return forge.listPulls(repo, { state, direction: 'desc' }).map((p) => pullJson(forge, base, repo, p));
    },
  },
  {
    name: 'merge_pull_request',
    title: 'Merge pull request',
    description: 'Squash-merge an open pull request into its base branch. Fails when the pull request conflicts.',
    inputSchema: {
      type: 'object',
      properties: { owner: OWNER, repo: REPO, pull_number: int('Pull request number'), commit_title: str('Title of the squash commit'), commit_message: str('Extra detail for the squash commit'), merge_method: { type: 'string', enum: ['squash', 'merge', 'rebase'], description: 'Merge method (squash is used)' } },
      required: ['owner', 'repo', 'pull_number'],
      additionalProperties: false,
    },
    readOnly: false,
    run: (forge, _base, args) => {
      const repo = repoOf(forge, args);
      const pull = forge.requirePull(repo, requireInt(args, 'pull_number'));
      const result = forge.mergePull(repo, pull, { commit_title: args.commit_title, commit_message: args.commit_message });
      return { sha: result.sha, merged: true, message: result.message, number: pull.number };
    },
  },
  {
    name: 'get_file_contents',
    title: 'Get file contents',
    description: 'Read a file (UTF-8 text) from a repository at a branch, tag or commit (default branch when ref is omitted).',
    inputSchema: { type: 'object', properties: { owner: OWNER, repo: REPO, path: str('File path in the repository'), ref: str('Branch, tag or commit sha') }, required: ['owner', 'repo', 'path'], additionalProperties: false },
    readOnly: true,
    run: (forge, _base, args) => {
      const repo = repoOf(forge, args);
      const path = requireString(args, 'path');
      const tip = forge.resolve(repo, typeof args.ref === 'string' ? args.ref : undefined);
      if (tip === null) throw new ToolError('ref not found');
      const found = forge.lookup(tip, path);
      if (found === null || found.kind !== 'blob') throw new ToolError(`${path} is not a file at that ref`);
      const data = forge.objects.get(found.oid)?.data ?? Buffer.alloc(0);
      return { path, sha: found.oid, size: data.length, content: data.toString('utf8') };
    },
  },
];

const TOOL_LIST = TOOLS.map((t) => ({
  name: t.name,
  title: t.title,
  description: t.description,
  inputSchema: t.inputSchema,
  annotations: { title: t.title, readOnlyHint: t.readOnly },
}));

function findTool(name: unknown): ToolDef | undefined {
  if (typeof name !== 'string') return undefined;
  const lower = name.toLowerCase();
  return TOOLS.find((t) => t.name === lower || lower.endsWith(`__${t.name}`));
}

function callTool(forge: Forge, base: string, params: Obj): Obj {
  const tool = findTool(params.name);
  if (tool === undefined) throw new RpcError(INVALID_PARAMS, `Unknown tool: ${String(params.name)}`);
  const args = params.arguments === undefined || params.arguments === null ? {} : params.arguments;
  if (typeof args !== 'object' || Array.isArray(args)) throw new RpcError(INVALID_PARAMS, 'arguments must be an object');
  const required = (tool.inputSchema.required as string[] | undefined) ?? [];
  const missing = required.filter((key) => (args as Obj)[key] === undefined || (args as Obj)[key] === null || (args as Obj)[key] === '');
  if (missing.length > 0) {
    return { content: [{ type: 'text', text: `missing required parameter(s): ${missing.join(', ')}` }], isError: true };
  }
  try {
    const value = tool.run(forge, base, args as Obj);
    const text = JSON.stringify(value);
    const result: Obj = { content: [{ type: 'text', text }], isError: false };
    if (value !== null && typeof value === 'object' && !Array.isArray(value)) result.structuredContent = value;
    return result;
  } catch (err) {
    if (err instanceof ToolError || err instanceof ForgeError) {
      return { content: [{ type: 'text', text: err.message }], isError: true };
    }
    throw err;
  }
}

function negotiate(requested: unknown): string {
  return typeof requested === 'string' && SUPPORTED_VERSIONS.includes(requested) ? requested : (SUPPORTED_VERSIONS[1] as string);
}

/** Answer one JSON-RPC message; null for a notification (no id). */
function dispatch(forge: Forge, base: string, message: unknown): Obj | null {
  if (typeof message !== 'object' || message === null || Array.isArray(message)) {
    return { jsonrpc: '2.0', id: null, error: { code: INVALID_REQUEST, message: 'Invalid Request' } };
  }
  const msg = message as Obj;
  const id = msg.id;
  const isNotification = id === undefined;
  const method = msg.method;
  if (typeof method !== 'string') {
    if (isNotification) return null; // a response from the client: nothing to answer
    return { jsonrpc: '2.0', id: id ?? null, error: { code: INVALID_REQUEST, message: 'Invalid Request: method is required' } };
  }
  const params = typeof msg.params === 'object' && msg.params !== null && !Array.isArray(msg.params) ? (msg.params as Obj) : {};
  try {
    let result: Obj;
    switch (method) {
      case 'server/discover':
        result = {
          supportedVersions: SUPPORTED_VERSIONS,
          capabilities: CAPABILITIES,
          serverInfo: SERVER_INFO,
          instructions: 'A GitHub-compatible forge: repositories, issues and pull requests.',
          _meta: { 'io.modelcontextprotocol/serverInfo': SERVER_INFO },
        };
        break;
      case 'initialize':
        result = { protocolVersion: negotiate(params.protocolVersion), capabilities: CAPABILITIES, serverInfo: SERVER_INFO, instructions: 'A GitHub-compatible forge: repositories, issues and pull requests.' };
        break;
      case 'ping':
        result = {};
        break;
      case 'tools/list':
        result = { tools: TOOL_LIST };
        break;
      case 'tools/call':
        result = callTool(forge, base, params);
        break;
      case 'resources/list':
        result = { resources: [] };
        break;
      case 'prompts/list':
        result = { prompts: [] };
        break;
      default:
        if (method.startsWith('notifications/')) return null;
        throw new RpcError(METHOD_NOT_FOUND, `Method not found: ${method}`);
    }
    if (isNotification) return null;
    return { jsonrpc: '2.0', id, result };
  } catch (err) {
    if (isNotification) return null;
    if (err instanceof RpcError) return { jsonrpc: '2.0', id, error: { code: err.code, message: err.message } };
    throw err;
  }
}

function sendRpc(res: ServerResponse, status: number, body: unknown): void {
  const text = JSON.stringify(body);
  res.writeHead(status, { 'content-type': 'application/json', 'content-length': Buffer.byteLength(text) });
  res.end(text);
}

export async function handleMcp(forge: Forge, req: IncomingMessage, res: ServerResponse, base: string, authorized: boolean): Promise<void> {
  const method = req.method ?? 'GET';
  if (method === 'DELETE') {
    res.writeHead(authorized ? 405 : 401, { 'content-length': 0 });
    res.end();
    return;
  }
  if (method !== 'POST') {
    res.writeHead(405, { allow: 'POST', 'content-type': 'application/json' });
    res.end(JSON.stringify({ jsonrpc: '2.0', id: null, error: { code: INVALID_REQUEST, message: 'Use POST' } }));
    return;
  }
  const raw = await readBody(req);
  if (!authorized) {
    res.writeHead(401, { 'content-type': 'application/json', 'www-authenticate': 'Bearer realm="forge"' });
    res.end(JSON.stringify({ jsonrpc: '2.0', id: null, error: { code: -32001, message: 'Unauthorized: a bearer token is required' } }));
    return;
  }
  let body: unknown;
  try {
    body = JSON.parse(raw.toString('utf8'));
  } catch {
    sendRpc(res, 400, { jsonrpc: '2.0', id: null, error: { code: PARSE_ERROR, message: 'Parse error' } });
    return;
  }
  if (Array.isArray(body)) {
    if (body.length === 0) {
      sendRpc(res, 400, { jsonrpc: '2.0', id: null, error: { code: INVALID_REQUEST, message: 'Invalid Request: empty batch' } });
      return;
    }
    const answers = body.map((m) => dispatch(forge, base, m)).filter((a): a is Obj => a !== null);
    if (answers.length === 0) {
      res.writeHead(202, { 'content-length': 0 });
      res.end();
      return;
    }
    sendRpc(res, 200, answers);
    return;
  }
  const answer = dispatch(forge, base, body);
  if (answer === null) {
    res.writeHead(202, { 'content-length': 0 });
    res.end();
    return;
  }
  sendRpc(res, 200, answer);
}

