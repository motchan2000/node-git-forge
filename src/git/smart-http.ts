// git smart HTTP: /{owner}/{repo}.git/info/refs, git-upload-pack (protocol v2, with a v0 fallback)
// and git-receive-pack (v0). Objects go into the forge's ObjectStore; refs move through
// Forge.setRef, so REST and GraphQL see every push immediately.

import type { Req as IncomingMessage, Res as ServerResponse } from '../server.ts';
import { gunzipSync } from 'node:zlib';
import { Forge, type Repo } from '../store.ts';
import { readBody } from '../http.ts';
import { FLUSH, DELIM, PktError, lineOf, parsePackets, pkt, sideband, type Packet } from './pktline.ts';
import { PackError, readPack, writePack } from './pack.ts';
import { ZERO_OID, isTreeMode, parseCommit, tagTarget, type GitObject, type Oid } from './objects.ts';

const AGENT = 'agent=forge/1.0';
const OID_RE = /^[0-9a-f]{40}$/;

type Service = 'git-upload-pack' | 'git-receive-pack';

function sendPlain(res: ServerResponse, status: number, text: string, headers: Record<string, string> = {}): void {
  const body = Buffer.from(text);
  res.writeHead(status, { 'content-type': 'text/plain; charset=utf-8', 'content-length': body.length, ...headers });
  res.end(body);
}

function sendGit(res: ServerResponse, type: string, parts: readonly Buffer[]): void {
  const body = parts.length === 1 ? (parts[0] as Buffer) : Buffer.concat(parts);
  res.writeHead(200, {
    'content-type': type,
    'content-length': body.length,
    'cache-control': 'no-cache, max-age=0, must-revalidate',
    expires: 'Fri, 01 Jan 1980 00:00:00 GMT',
    pragma: 'no-cache',
  });
  res.end(body);
}

function wantsV2(req: IncomingMessage): boolean {
  const header = req.headers['git-protocol'];
  const value = Array.isArray(header) ? header.join(':') : (header ?? '');
  return value.split(':').some((part) => part.trim() === 'version=2');
}

/** The refs a repository advertises, sorted, with HEAD's target. */
function sortedRefs(repo: Repo): [string, Oid][] {
  return [...repo.refs.entries()].sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
}

/**
 * Route a smart-HTTP request. Returns false when the path is not a git route (the caller answers
 * 404 JSON); every git route answers here.
 */
export async function handleGit(forge: Forge, req: IncomingMessage, res: ServerResponse, url: URL, authorized: boolean): Promise<boolean> {
  const m = /^\/([^/]+)\/([^/]+?)(?:\.git)\/(info\/refs|git-upload-pack|git-receive-pack)$/.exec(url.pathname);
  if (m === null) return false;
  if (!authorized) {
    sendPlain(res, 401, 'Authentication required\n', { 'www-authenticate': 'Basic realm="forge"' });
    return true;
  }
  let owner: string;
  let name: string;
  try {
    owner = decodeURIComponent(m[1] as string);
    name = decodeURIComponent(m[2] as string);
  } catch {
    sendPlain(res, 404, 'Repository not found\n');
    return true;
  }
  const repo = forge.getRepo(owner, name);
  const action = m[3] as string;
  const method = req.method ?? 'GET';

  if (action === 'info/refs') {
    if (method !== 'GET' && method !== 'HEAD') {
      sendPlain(res, 405, 'Method Not Allowed\n');
      return true;
    }
    const service = url.searchParams.get('service');
    if (service !== 'git-upload-pack' && service !== 'git-receive-pack') {
      sendPlain(res, 403, 'dumb HTTP is not supported; pass ?service=\n');
      return true;
    }
    if (repo === null) {
      sendPlain(res, 404, 'Repository not found\n');
      return true;
    }
    advertise(forge, repo, service, wantsV2(req), res);
    return true;
  }

  if (method !== 'POST') {
    sendPlain(res, 405, 'Method Not Allowed\n');
    return true;
  }
  if (repo === null) {
    sendPlain(res, 404, 'Repository not found\n');
    return true;
  }
  let body = await readBody(req);
  const encoding = req.headers['content-encoding'];
  if (encoding === 'gzip' || encoding === 'x-gzip') {
    try {
      body = gunzipSync(body);
    } catch {
      sendPlain(res, 400, 'bad gzip body\n');
      return true;
    }
  }
  try {
    if (action === 'git-upload-pack') {
      if (wantsV2(req)) uploadPackV2(forge, repo, body, res);
      else uploadPackV0(forge, repo, body, res);
    } else {
      receivePack(forge, repo, body, res);
    }
  } catch (err) {
    if (err instanceof PktError || err instanceof ProtocolError) {
      sendPlain(res, 400, `${err.message}\n`);
      return true;
    }
    throw err;
  }
  return true;
}

class ProtocolError extends Error {}

// ---------------------------------------------------------------------------------------------
// Advertisements

/** The v2 capability advertisement never varies: built once. */
const V2_ADVERTISEMENT = Buffer.concat([
  pkt('version 2\n'),
  pkt(`${AGENT}\n`),
  pkt('ls-refs=unborn\n'),
  pkt('fetch=shallow\n'),
  pkt('server-option\n'),
  pkt('object-format=sha1\n'),
  FLUSH,
]);

/**
 * Whole v2 command responses, keyed by repository, ref version and the exact request bytes. A response
 * depends only on those (objects are append-only), so concurrent identical clones share one answer.
 */
const v2ResponseCache = new Map<string, Buffer>();
const V2_CACHE_MAX = 256;

function advertise(forge: Forge, repo: Repo, service: Service, v2: boolean, res: ServerResponse): void {
  const type = `application/x-${service}-advertisement`;
  if (service === 'git-upload-pack' && v2) {
    sendGit(res, type, [V2_ADVERTISEMENT]);
    return;
  }
  const parts: Buffer[] = [pkt(`# service=${service}\n`), FLUSH];
  const head = forge.headBranch(repo);
  const caps =
    service === 'git-upload-pack'
      ? `multi_ack_detailed side-band-64k side-band ofs-delta shallow deepen-since deepen-not deepen-relative no-progress include-tag thin-pack allow-tip-sha1-in-want allow-reachable-sha1-in-want symref=HEAD:refs/heads/${head} object-format=sha1 ${AGENT}`
      : `report-status delete-refs side-band-64k quiet ofs-delta object-format=sha1 ${AGENT}`;
  const lines: string[] = [];
  if (service === 'git-upload-pack') {
    const headOid = repo.refs.get(`refs/heads/${head}`);
    if (headOid !== undefined) lines.push(`${headOid} HEAD`);
  }
  for (const [ref, oid] of sortedRefs(repo)) {
    lines.push(`${oid} ${ref}`);
    if (service === 'git-upload-pack' && ref.startsWith('refs/tags/')) {
      const peeled = forge.peelToCommit(oid);
      if (peeled !== null && peeled !== oid) lines.push(`${peeled} ${ref}^{}`);
    }
  }
  if (lines.length === 0) lines.push(`${ZERO_OID} capabilities^{}`);
  lines.forEach((line, i) => parts.push(pkt(i === 0 ? `${line}\0${caps}\n` : `${line}\n`)));
  parts.push(FLUSH);
  sendGit(res, type, parts);
}

// ---------------------------------------------------------------------------------------------
// upload-pack v2

function uploadPackV2(forge: Forge, repo: Repo, body: Buffer, res: ServerResponse): void {
  const type = 'application/x-git-upload-pack-result';
  const key = body.length <= 8192 ? `${repo.id}:${repo.refVersion}:${body.toString('latin1')}` : null;
  if (key !== null) {
    const hit = v2ResponseCache.get(key);
    if (hit !== undefined) {
      sendGit(res, type, [hit]);
      return;
    }
  }
  const parts = uploadPackV2Parts(forge, repo, body);
  const response = Buffer.concat(parts);
  if (key !== null) {
    if (v2ResponseCache.size >= V2_CACHE_MAX) v2ResponseCache.delete(v2ResponseCache.keys().next().value as string);
    v2ResponseCache.set(key, response);
  }
  sendGit(res, type, [response]);
}

function uploadPackV2Parts(forge: Forge, repo: Repo, body: Buffer): Buffer[] {
  const { packets } = parsePackets(body);
  let command = '';
  const args: string[] = [];
  let inArgs = false;
  for (const p of packets) {
    if (p.kind === 'flush') break;
    if (p.kind === 'delim') {
      inArgs = true;
      continue;
    }
    const line = lineOf(p);
    if (line === null) continue;
    if (!inArgs) {
      if (line.startsWith('command=')) command = line.slice(8);
    } else {
      args.push(line);
    }
  }
  if (command === 'ls-refs') return lsRefs(forge, repo, args);
  if (command === 'fetch') return fetchV2(forge, repo, args);
  throw new ProtocolError(`unknown command ${JSON.stringify(command)}`);
}

function lsRefs(forge: Forge, repo: Repo, args: readonly string[]): Buffer[] {
  const symrefs = args.includes('symrefs');
  const peel = args.includes('peel');
  const unborn = args.includes('unborn');
  const prefixes = args.filter((a) => a.startsWith('ref-prefix ')).map((a) => a.slice(11));
  const keep = (name: string): boolean => prefixes.length === 0 || prefixes.some((p) => name.startsWith(p));
  const out: Buffer[] = [];
  const head = forge.headBranch(repo);
  if (keep('HEAD')) {
    const target = `refs/heads/${head}`;
    const oid = repo.refs.get(target);
    if (oid !== undefined) out.push(pkt(`${oid} HEAD${symrefs ? ` symref-target:${target}` : ''}\n`));
    else if (unborn) out.push(pkt(`unborn HEAD${symrefs ? ` symref-target:${target}` : ''}\n`));
  }
  for (const [ref, oid] of sortedRefs(repo)) {
    if (!keep(ref)) continue;
    let line = `${oid} ${ref}`;
    if (peel && ref.startsWith('refs/tags/')) {
      const peeled = forge.peelToCommit(oid);
      if (peeled !== null && peeled !== oid) line += ` peeled:${peeled}`;
    }
    out.push(pkt(`${line}\n`));
  }
  out.push(FLUSH);
  return out;
}

interface FetchArgs {
  wants: Oid[];
  haves: Oid[];
  shallow: Set<Oid>;
  depth: number | null;
  deepenRelative: boolean;
  done: boolean;
  includeTag: boolean;
  sideband: boolean;
}

function fetchV2(forge: Forge, repo: Repo, args: readonly string[]): Buffer[] {
  const f: FetchArgs = { wants: [], haves: [], shallow: new Set(), depth: null, deepenRelative: false, done: false, includeTag: false, sideband: true };
  for (const a of args) {
    if (a.startsWith('want ')) f.wants.push(a.slice(5).trim().toLowerCase());
    else if (a.startsWith('want-ref ')) {
      const oid = repo.refs.get(a.slice(9).trim());
      if (oid === undefined) throw new ProtocolError(`unknown ref ${a.slice(9)}`);
      f.wants.push(oid);
    } else if (a.startsWith('have ')) f.haves.push(a.slice(5).trim().toLowerCase());
    else if (a.startsWith('shallow ')) f.shallow.add(a.slice(8).trim().toLowerCase());
    else if (a.startsWith('deepen ')) f.depth = Number(a.slice(7));
    else if (a === 'deepen-relative') f.deepenRelative = true;
    else if (a === 'done') f.done = true;
    else if (a === 'include-tag') f.includeTag = true;
  }
  if (f.depth !== null && (!Number.isInteger(f.depth) || f.depth < 1)) throw new ProtocolError('bad deepen value');
  for (const w of f.wants) if (!OID_RE.test(w)) throw new ProtocolError(`bad want ${w}`);

  const out: Buffer[] = [];
  const common = f.haves.filter((h) => forge.objects.has(h));
  if (!f.done) {
    out.push(pkt('acknowledgments\n'));
    if (common.length === 0) out.push(pkt('NAK\n'));
    for (const h of common) out.push(pkt(`ACK ${h}\n`));
    out.push(pkt('ready\n'));
    out.push(DELIM);
  }
  const plan = planFetch(forge, repo, f);
  if (plan.shallowLines.length > 0) {
    out.push(pkt('shallow-info\n'));
    for (const line of plan.shallowLines) out.push(pkt(`${line}\n`));
    out.push(DELIM);
  }
  out.push(pkt('packfile\n'));
  for (const b of sideband(1, plan.pack)) out.push(b);
  out.push(FLUSH);
  return out;
}

// ---------------------------------------------------------------------------------------------
// upload-pack v0

function uploadPackV0(forge: Forge, repo: Repo, body: Buffer, res: ServerResponse): void {
  const { packets } = parsePackets(body);
  const f: FetchArgs = { wants: [], haves: [], shallow: new Set(), depth: null, deepenRelative: false, done: false, includeTag: false, sideband: false };
  let sidebandMode: 0 | 1 | 2 = 0; // 2 = side-band-64k
  let first = true;
  for (const p of packets) {
    const line = lineOf(p);
    if (line === null) continue;
    if (line.startsWith('want ')) {
      const parts = line.slice(5).split(' ');
      f.wants.push((parts[0] ?? '').toLowerCase());
      if (first) {
        const caps = parts.slice(1);
        if (caps.includes('side-band-64k')) sidebandMode = 2;
        else if (caps.includes('side-band')) sidebandMode = 1;
        if (caps.includes('include-tag')) f.includeTag = true;
        if (caps.includes('deepen-relative')) f.deepenRelative = true;
        first = false;
      }
    } else if (line.startsWith('have ')) f.haves.push(line.slice(5).trim().toLowerCase());
    else if (line.startsWith('shallow ')) f.shallow.add(line.slice(8).trim().toLowerCase());
    else if (line.startsWith('deepen ')) f.depth = Number(line.slice(7));
    else if (line === 'done') f.done = true;
  }
  if (f.wants.length === 0) {
    sendGit(res, 'application/x-git-upload-pack-result', [FLUSH]);
    return;
  }
  for (const w of f.wants) if (!OID_RE.test(w)) throw new ProtocolError(`bad want ${w}`);
  if (f.depth !== null && (!Number.isInteger(f.depth) || f.depth < 1)) throw new ProtocolError('bad deepen value');
  const plan = planFetch(forge, repo, f);
  const out: Buffer[] = [];
  if (f.depth !== null || f.shallow.size > 0) {
    for (const line of plan.shallowLines) out.push(pkt(`${line}\n`));
    out.push(FLUSH);
  }
  const common = f.haves.find((h) => forge.objects.has(h));
  out.push(pkt(common === undefined ? 'NAK\n' : `ACK ${common}\n`));
  if (sidebandMode === 0) out.push(plan.pack);
  else {
    for (const b of sideband(1, plan.pack, sidebandMode === 2 ? 65515 : 999)) out.push(b);
    out.push(FLUSH);
  }
  sendGit(res, 'application/x-git-upload-pack-result', out);
}

// ---------------------------------------------------------------------------------------------
// What to send

interface FetchPlan {
  shallowLines: string[];
  pack: Buffer;
}

const planCache = new Map<string, FetchPlan>();
const PLAN_CACHE_MAX = 64;

/** Whether `oid` may be served from this repository: a ref tip, a peeled tip, or reachable from one. */
function servable(forge: Forge, repo: Repo, wants: readonly Oid[]): boolean {
  const tips = new Set<Oid>();
  for (const oid of repo.refs.values()) {
    tips.add(oid);
    const peeled = forge.peelToCommit(oid);
    if (peeled !== null) tips.add(peeled);
  }
  const rest = wants.filter((w) => !tips.has(w));
  if (rest.length === 0) return true;
  const reach = new Set<Oid>();
  for (const tip of tips) for (const c of forge.history(tip)) reach.add(c);
  return rest.every((w) => reach.has(w));
}

function planFetch(forge: Forge, repo: Repo, f: FetchArgs): FetchPlan {
  const key = `${repo.id}:${repo.refVersion}:${[...f.wants].sort().join(',')}|${[...f.haves].sort().join(',')}|${[...f.shallow].sort().join(',')}|${f.depth ?? ''}|${f.includeTag ? 1 : 0}`;
  const cached = planCache.get(key);
  if (cached !== undefined) return cached;
  for (const w of f.wants) if (!forge.objects.has(w)) throw new ProtocolError(`not our ref ${w}`);
  if (!servable(forge, repo, f.wants)) throw new ProtocolError('want is not reachable from any ref');

  const store = forge.objects;
  const send = new Map<Oid, GitObject>();
  const add = (oid: Oid): GitObject | undefined => {
    const obj = store.get(oid);
    if (obj !== undefined) send.set(oid, obj);
    return obj;
  };

  // The client's commits: reachable from its haves, not past its shallow boundary.
  const haveCommits = new Set<Oid>();
  const stack = f.haves.filter((h) => store.commit(h) !== null);
  while (stack.length > 0) {
    const oid = stack.pop() as Oid;
    if (haveCommits.has(oid)) continue;
    haveCommits.add(oid);
    if (f.shallow.has(oid)) continue;
    for (const p of store.commit(oid)?.parents ?? []) if (!haveCommits.has(p)) stack.push(p);
  }

  // Peel wanted tags into commits.
  const wantCommits: Oid[] = [];
  for (const w of f.wants) {
    let oid: Oid | null = w;
    while (oid !== null) {
      const obj = store.get(oid);
      if (obj === undefined) break;
      if (obj.type === 'commit') {
        wantCommits.push(oid);
        break;
      }
      if (obj.type === 'tag') {
        if (!haveCommits.has(oid)) add(oid);
        oid = tagTarget(obj.data);
        continue;
      }
      // a wanted tree or blob
      if (obj.type === 'tree') addTree(store, oid, send, new Set());
      else add(oid);
      break;
    }
  }

  // Walk commits from the wants.
  const shallowLines: string[] = [];
  const commits: Oid[] = [];
  if (f.depth !== null) {
    const depthOf = new Map<Oid, number>();
    const queue: Oid[] = [];
    for (const c of wantCommits) if (!depthOf.has(c)) {
      depthOf.set(c, 1);
      queue.push(c);
    }
    for (let i = 0; i < queue.length; i += 1) {
      const oid = queue[i] as Oid;
      const d = depthOf.get(oid) as number;
      const parents = store.commit(oid)?.parents ?? [];
      if (d >= (f.depth as number)) {
        if (parents.length > 0 && !f.shallow.has(oid)) shallowLines.push(`shallow ${oid}`);
        continue;
      }
      if (f.shallow.has(oid) && parents.length > 0) shallowLines.push(`unshallow ${oid}`);
      for (const p of parents) {
        if (!depthOf.has(p) && store.commit(p) !== null) {
          depthOf.set(p, d + 1);
          queue.push(p);
        }
      }
    }
    for (const oid of queue) {
      if (haveCommits.has(oid) && !f.shallow.has(oid)) continue;
      if (haveCommits.has(oid)) continue;
      commits.push(oid);
    }
  } else {
    const seen = new Set<Oid>();
    const walk = [...wantCommits];
    while (walk.length > 0) {
      const oid = walk.pop() as Oid;
      if (seen.has(oid) || haveCommits.has(oid)) continue;
      seen.add(oid);
      commits.push(oid);
      if (f.shallow.has(oid)) continue;
      for (const p of store.commit(oid)?.parents ?? []) walk.push(p);
    }
  }

  // Trees and blobs the client lacks: everything under the sent commits minus the haves' trees.
  const haveObjects = new Set<Oid>();
  if (commits.length > 0 && haveCommits.size > 0) {
    for (const c of haveCommits) {
      const tree = store.commit(c)?.tree;
      if (tree !== undefined) markTree(store, tree, haveObjects);
    }
  }
  for (const c of commits) {
    const commit = add(c);
    if (commit === undefined) continue;
    const tree = parseCommit(commit.data).tree;
    addTree(store, tree, send, haveObjects);
  }

  // include-tag: annotated tags pointing at commits we send.
  if (f.includeTag) {
    const sentCommits = new Set(commits);
    for (const [ref, oid] of repo.refs) {
      if (!ref.startsWith('refs/tags/') || send.has(oid)) continue;
      const obj = store.get(oid);
      if (obj?.type === 'tag') {
        const target = tagTarget(obj.data);
        if (target !== null && sentCommits.has(target)) send.set(oid, obj);
      }
    }
  }

  const ordered = [...send.entries()].sort((a, b) => rank(a[1]) - rank(b[1]));
  const plan: FetchPlan = { shallowLines, pack: writePack(ordered) };
  if (planCache.size >= PLAN_CACHE_MAX) planCache.delete(planCache.keys().next().value as string);
  planCache.set(key, plan);
  return plan;
}

function rank(obj: GitObject): number {
  return obj.type === 'commit' ? 0 : obj.type === 'tag' ? 1 : obj.type === 'tree' ? 2 : 3;
}

function markTree(store: Forge['objects'], tree: Oid, into: Set<Oid>): void {
  if (into.has(tree)) return;
  into.add(tree);
  for (const e of store.tree(tree) ?? []) {
    if (isTreeMode(e.mode)) markTree(store, e.oid, into);
    else into.add(e.oid);
  }
}

function addTree(store: Forge['objects'], tree: Oid, send: Map<Oid, GitObject>, skip: ReadonlySet<Oid>): void {
  if (skip.has(tree) || send.has(tree)) return;
  const obj = store.get(tree);
  if (obj === undefined) return;
  send.set(tree, obj);
  for (const e of store.tree(tree) ?? []) {
    if (e.mode === '160000') continue; // submodule commit: not ours to send
    if (isTreeMode(e.mode)) addTree(store, e.oid, send, skip);
    else if (!skip.has(e.oid) && !send.has(e.oid)) {
      const blob = store.get(e.oid);
      if (blob !== undefined) send.set(e.oid, blob);
    }
  }
}

// ---------------------------------------------------------------------------------------------
// receive-pack

interface Command {
  readonly oldOid: Oid;
  readonly newOid: Oid;
  readonly ref: string;
}

function receivePack(forge: Forge, repo: Repo, body: Buffer, res: ServerResponse): void {
  const { packets, offset } = parsePackets(body, 0, 1);
  const commands: Command[] = [];
  let caps: string[] = [];
  for (const p of packets as Packet[]) {
    if (p.kind !== 'data') continue;
    let text = p.payload.toString('utf8');
    const nul = text.indexOf('\0');
    if (nul >= 0) {
      caps = text.slice(nul + 1).trim().split(' ');
      text = text.slice(0, nul);
    }
    const m = /^([0-9a-f]{40}) ([0-9a-f]{40}) (\S+)\n?$/.exec(text);
    if (m === null) throw new ProtocolError(`bad command line ${JSON.stringify(text.slice(0, 100))}`);
    commands.push({ oldOid: m[1] as string, newOid: m[2] as string, ref: m[3] as string });
  }
  const type = 'application/x-git-receive-pack-result';
  if (commands.length === 0) {
    sendGit(res, type, [FLUSH]);
    return;
  }
  const useSideband = caps.includes('side-band-64k');
  const report = caps.includes('report-status') || caps.includes('report-status-v2');

  const lines: string[] = [];
  const packBytes = body.subarray(offset);
  let unpackError: string | null = null;
  let received = new Map<Oid, GitObject>();
  if (packBytes.length > 0) {
    try {
      received = readPack(packBytes, (oid) => forge.objects.get(oid)).objects;
    } catch (err) {
      unpackError = err instanceof PackError ? err.message : `unpack failed: ${err instanceof Error ? err.message : String(err)}`;
    }
  } else if (commands.some((c) => c.newOid !== ZERO_OID)) {
    // A pack-less update is fine when every new tip is already stored (git sends an empty pack).
  }

  if (unpackError !== null) {
    lines.push(`unpack ${unpackError.replace(/\n/g, ' ')}`);
    for (const c of commands) lines.push(`ng ${c.ref} unpacker error`);
  } else {
    for (const [oid, obj] of received) forge.objects.putKnown(oid, obj.type, obj.data);
    const incomplete = missingLinks(forge, received);
    lines.push('unpack ok');
    for (const c of commands) {
      const reason = applyCommand(forge, repo, c, incomplete);
      lines.push(reason === null ? `ok ${c.ref}` : `ng ${c.ref} ${reason}`);
    }
  }

  const out: Buffer[] = [];
  if (report) {
    const inner = Buffer.concat([...lines.map((l) => pkt(`${l}\n`)), FLUSH]);
    if (useSideband) {
      out.push(...sideband(1, inner));
      out.push(FLUSH);
    } else {
      out.push(inner);
    }
  } else if (useSideband) {
    out.push(FLUSH);
  }
  sendGit(res, type, out.length > 0 ? out : [Buffer.alloc(0)]);
}

/** Objects that the received objects name but neither the pack nor the store holds. */
function missingLinks(forge: Forge, received: ReadonlyMap<Oid, GitObject>): Set<Oid> {
  const missing = new Set<Oid>();
  const need = (oid: Oid): void => {
    if (!forge.objects.has(oid)) missing.add(oid);
  };
  for (const [oid, obj] of received) {
    if (obj.type === 'commit') {
      const c = forge.objects.commit(oid);
      if (c === null) continue;
      need(c.tree);
      for (const p of c.parents) need(p);
    } else if (obj.type === 'tree') {
      for (const e of forge.objects.tree(oid) ?? []) if (e.mode !== '160000') need(e.oid);
    } else if (obj.type === 'tag') {
      const t = tagTarget(obj.data);
      if (t !== null) need(t);
    }
  }
  return missing;
}

function applyCommand(forge: Forge, repo: Repo, c: Command, incomplete: ReadonlySet<Oid>): string | null {
  if (!c.ref.startsWith('refs/') || c.ref.includes('..') || /[\s~^:?*[\\]/.test(c.ref) || c.ref.endsWith('/') || c.ref.endsWith('.lock')) return 'funny refname';
  const current = repo.refs.get(c.ref) ?? ZERO_OID;
  if (c.oldOid !== current) return current === ZERO_OID ? 'stale info' : 'fetch first';
  if (c.newOid === ZERO_OID) {
    if (current === ZERO_OID) return 'no such ref';
    forge.setRef(repo, c.ref, null);
    return null;
  }
  const obj = forge.objects.get(c.newOid);
  if (obj === undefined || incomplete.size > 0) return 'missing necessary objects';
  if (c.ref.startsWith('refs/heads/') && obj.type !== 'commit') return 'branch must point at a commit';
  if (current !== ZERO_OID) {
    if (c.ref.startsWith('refs/tags/')) return 'already exists';
    const oldCommit = forge.peelToCommit(current);
    const newCommit = forge.peelToCommit(c.newOid);
    if (oldCommit === null || newCommit === null || !forge.isAncestor(oldCommit, newCommit)) return 'non-fast-forward';
  }
  forge.setRef(repo, c.ref, c.newOid);
  return null;
}
