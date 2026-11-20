// Git objects: hashing, a content-addressed store, and the tree/commit codecs every surface shares.
// One ObjectStore holds every repository's objects (content addressing makes sharing safe); refs are
// per repository and live in the forge store.

import { createHash } from 'node:crypto';

export type Oid = string;
export type ObjectType = 'blob' | 'tree' | 'commit' | 'tag';

export interface GitObject {
  readonly type: ObjectType;
  readonly data: Buffer;
}

export const ZERO_OID = '0000000000000000000000000000000000000000';

export const TYPE_CODES: Readonly<Record<ObjectType, number>> = { commit: 1, tree: 2, blob: 3, tag: 4 };
export const CODE_TYPES: Readonly<Record<number, ObjectType>> = { 1: 'commit', 2: 'tree', 3: 'blob', 4: 'tag' };

export function hashObject(type: ObjectType, data: Buffer): Oid {
  const h = createHash('sha1');
  h.update(`${type} ${data.length}\0`);
  h.update(data);
  return h.digest('hex');
}

export class ObjectStore {
  readonly objects = new Map<Oid, GitObject>();

  put(type: ObjectType, data: Buffer): Oid {
    const oid = hashObject(type, data);
    if (!this.objects.has(oid)) this.objects.set(oid, { type, data });
    return oid;
  }

  /** Store an object whose oid the caller already computed (pack ingestion). */
  putKnown(oid: Oid, type: ObjectType, data: Buffer): void {
    if (!this.objects.has(oid)) this.objects.set(oid, { type, data });
  }

  get(oid: Oid): GitObject | undefined {
    return this.objects.get(oid);
  }

  has(oid: Oid): boolean {
    return this.objects.has(oid);
  }

  // ---- typed readers, with small caches since commits and trees are re-read constantly

  readonly #commitCache = new Map<Oid, Commit>();
  readonly #treeCache = new Map<Oid, TreeEntry[]>();

  commit(oid: Oid): Commit | null {
    const cached = this.#commitCache.get(oid);
    if (cached !== undefined) return cached;
    const obj = this.objects.get(oid);
    if (obj === undefined || obj.type !== 'commit') return null;
    const parsed = parseCommit(obj.data);
    this.#commitCache.set(oid, parsed);
    return parsed;
  }

  tree(oid: Oid): TreeEntry[] | null {
    const cached = this.#treeCache.get(oid);
    if (cached !== undefined) return cached;
    const obj = this.objects.get(oid);
    if (obj === undefined || obj.type !== 'tree') return null;
    const parsed = parseTree(obj.data);
    this.#treeCache.set(oid, parsed);
    return parsed;
  }
}

// ---------------------------------------------------------------------------------------------
// Trees

export interface TreeEntry {
  readonly mode: string; // '100644', '100755', '40000', '120000', '160000'
  readonly name: string;
  readonly oid: Oid;
}

export function isTreeMode(mode: string): boolean {
  return mode === '40000' || mode === '040000';
}

export function parseTree(data: Buffer): TreeEntry[] {
  const out: TreeEntry[] = [];
  let i = 0;
  while (i < data.length) {
    const space = data.indexOf(0x20, i);
    const nul = data.indexOf(0, space + 1);
    if (space < 0 || nul < 0 || nul + 21 > data.length) throw new Error('malformed tree');
    const mode = data.toString('latin1', i, space);
    const name = data.toString('utf8', space + 1, nul);
    const oid = data.toString('hex', nul + 1, nul + 21);
    out.push({ mode, name, oid });
    i = nul + 21;
  }
  return out;
}

/** Git's tree order: byte order of names, with directories compared as if they ended in '/'. */
function treeSortKey(entry: TreeEntry): string {
  return isTreeMode(entry.mode) ? `${entry.name}/` : entry.name;
}

export function serializeTree(entries: readonly TreeEntry[]): Buffer {
  const sorted = [...entries].sort((a, b) => {
    const ka = Buffer.from(treeSortKey(a));
    const kb = Buffer.from(treeSortKey(b));
    return Buffer.compare(ka, kb);
  });
  const parts: Buffer[] = [];
  for (const e of sorted) {
    parts.push(Buffer.from(`${isTreeMode(e.mode) ? '40000' : e.mode} ${e.name}\0`));
    parts.push(Buffer.from(e.oid, 'hex'));
  }
  return Buffer.concat(parts);
}

// ---------------------------------------------------------------------------------------------
// Commits

export interface Signature {
  readonly name: string;
  readonly email: string;
  readonly time: number; // seconds since epoch
  readonly tz: string; // '+0000'
}

export interface Commit {
  readonly tree: Oid;
  readonly parents: readonly Oid[];
  readonly author: Signature;
  readonly committer: Signature;
  readonly message: string;
}

function parseSignature(text: string): Signature {
  const m = /^(.*) <([^>]*)> (-?\d+) ([+-]\d{4})$/.exec(text);
  if (m === null) return { name: text, email: '', time: 0, tz: '+0000' };
  return { name: m[1] ?? '', email: m[2] ?? '', time: Number(m[3]), tz: m[4] ?? '+0000' };
}

function formatSignature(s: Signature): string {
  return `${s.name} <${s.email}> ${s.time} ${s.tz}`;
}

export function parseCommit(data: Buffer): Commit {
  const text = data.toString('utf8');
  const split = text.indexOf('\n\n');
  const header = split < 0 ? text : text.slice(0, split);
  const message = split < 0 ? '' : text.slice(split + 2);
  let tree = '';
  const parents: Oid[] = [];
  let author: Signature = { name: '', email: '', time: 0, tz: '+0000' };
  let committer = author;
  for (const line of header.split('\n')) {
    if (line.startsWith('tree ')) tree = line.slice(5);
    else if (line.startsWith('parent ')) parents.push(line.slice(7));
    else if (line.startsWith('author ')) author = parseSignature(line.slice(7));
    else if (line.startsWith('committer ')) committer = parseSignature(line.slice(10));
  }
  return { tree, parents, author, committer, message };
}

export function serializeCommit(c: Commit): Buffer {
  let text = `tree ${c.tree}\n`;
  for (const p of c.parents) text += `parent ${p}\n`;
  text += `author ${formatSignature(c.author)}\n`;
  text += `committer ${formatSignature(c.committer)}\n\n`;
  text += c.message;
  return Buffer.from(text, 'utf8');
}

/** The peeled target of an annotated tag object, or null. */
export function tagTarget(data: Buffer): Oid | null {
  const m = /^object ([0-9a-f]{40})/.exec(data.toString('latin1', 0, Math.min(data.length, 64)));
  return m === null ? null : (m[1] ?? null);
}
