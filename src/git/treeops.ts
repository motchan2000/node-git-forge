// Tree and history operations over the ObjectStore: flatten/rebuild trees, walk paths, ancestry,
// merge bases and file-level three-way merges. REST contents, pulls and GraphQL all build on these.

import { isTreeMode, serializeTree, type ObjectStore, type Oid, type TreeEntry } from './objects.ts';

export interface FlatEntry {
  readonly mode: string;
  readonly oid: Oid;
}

/** Every blob (and submodule) path under a tree, as path -> {mode, oid}. */
export function flattenTree(store: ObjectStore, treeOid: Oid, prefix = '', out = new Map<string, FlatEntry>()): Map<string, FlatEntry> {
  const entries = store.tree(treeOid);
  if (entries === null) return out;
  for (const e of entries) {
    const path = prefix === '' ? e.name : `${prefix}/${e.name}`;
    if (isTreeMode(e.mode)) flattenTree(store, e.oid, path, out);
    else out.set(path, { mode: e.mode, oid: e.oid });
  }
  return out;
}

interface DirNode {
  files: Map<string, FlatEntry>;
  dirs: Map<string, DirNode>;
}

/** Build (and store) the tree for a flat path map; returns the root tree oid. */
export function buildTree(store: ObjectStore, flat: ReadonlyMap<string, FlatEntry>): Oid {
  const root: DirNode = { files: new Map(), dirs: new Map() };
  for (const [path, entry] of flat) {
    const parts = path.split('/');
    let node = root;
    for (let i = 0; i < parts.length - 1; i += 1) {
      const part = parts[i] as string;
      let child = node.dirs.get(part);
      if (child === undefined) {
        child = { files: new Map(), dirs: new Map() };
        node.dirs.set(part, child);
      }
      node = child;
    }
    node.files.set(parts[parts.length - 1] as string, entry);
  }
  const write = (node: DirNode): Oid => {
    const entries: TreeEntry[] = [];
    for (const [name, child] of node.dirs) entries.push({ mode: '40000', name, oid: write(child) });
    for (const [name, f] of node.files) entries.push({ mode: f.mode, name, oid: f.oid });
    return store.put('tree', serializeTree(entries));
  };
  return write(root);
}

export type PathLookup = { kind: 'blob'; mode: string; oid: Oid } | { kind: 'tree'; oid: Oid } | null;

/** Resolve a repository-relative path under a root tree. '' is the root itself. */
export function lookupPath(store: ObjectStore, rootTree: Oid, path: string): PathLookup {
  const parts = path.split('/').filter((p) => p !== '');
  let current: Oid = rootTree;
  for (let i = 0; i < parts.length; i += 1) {
    const entries = store.tree(current);
    if (entries === null) return null;
    const found = entries.find((e) => e.name === parts[i]);
    if (found === undefined) return null;
    if (i === parts.length - 1) {
      return isTreeMode(found.mode) ? { kind: 'tree', oid: found.oid } : { kind: 'blob', mode: found.mode, oid: found.oid };
    }
    if (!isTreeMode(found.mode)) return null;
    current = found.oid;
  }
  return { kind: 'tree', oid: current };
}

/** Whether `ancestor` is reachable from `descendant` (inclusive). */
export function isAncestor(store: ObjectStore, ancestor: Oid, descendant: Oid): boolean {
  if (ancestor === descendant) return true;
  const seen = new Set<Oid>();
  const stack: Oid[] = [descendant];
  while (stack.length > 0) {
    const oid = stack.pop() as Oid;
    if (oid === ancestor) return true;
    if (seen.has(oid)) continue;
    seen.add(oid);
    const c = store.commit(oid);
    if (c === null) continue;
    for (const p of c.parents) stack.push(p);
  }
  return false;
}

/** All commits reachable from `tip`, in breadth-first order (newest first for linear history). */
export function ancestors(store: ObjectStore, tip: Oid, limit = Infinity): Oid[] {
  const out: Oid[] = [];
  const seen = new Set<Oid>([tip]);
  const queue: Oid[] = [tip];
  for (let i = 0; i < queue.length && out.length < limit; i += 1) {
    const oid = queue[i] as Oid;
    const c = store.commit(oid);
    if (c === null) continue;
    out.push(oid);
    for (const p of c.parents) {
      if (!seen.has(p)) {
        seen.add(p);
        queue.push(p);
      }
    }
  }
  return out;
}

/** A best common ancestor of two commits (the first of a's ancestors that b also reaches), or null. */
export function mergeBase(store: ObjectStore, a: Oid, b: Oid): Oid | null {
  const reachB = new Set(ancestors(store, b));
  for (const oid of ancestors(store, a)) if (reachB.has(oid)) return oid;
  return null;
}

export interface FileChange {
  readonly path: string;
  readonly status: 'added' | 'modified' | 'removed';
  readonly before: FlatEntry | undefined;
  readonly after: FlatEntry | undefined;
}

/** The file-level difference between two flat trees, sorted by path. */
export function diffFlat(before: ReadonlyMap<string, FlatEntry>, after: ReadonlyMap<string, FlatEntry>): FileChange[] {
  const out: FileChange[] = [];
  for (const [path, entry] of after) {
    const old = before.get(path);
    if (old === undefined) out.push({ path, status: 'added', before: undefined, after: entry });
    else if (old.oid !== entry.oid || old.mode !== entry.mode) out.push({ path, status: 'modified', before: old, after: entry });
  }
  for (const [path, entry] of before) if (!after.has(path)) out.push({ path, status: 'removed', before: entry, after: undefined });
  out.sort((x, y) => (x.path < y.path ? -1 : x.path > y.path ? 1 : 0));
  return out;
}

/** Line additions/deletions between two texts: a multiset line diff (cheap and adequate for counts). */
export function lineCounts(before: string, after: string): { additions: number; deletions: number } {
  const split = (t: string): string[] => (t === '' ? [] : t.replace(/\n$/, '').split('\n'));
  const a = split(before);
  const b = split(after);
  const counts = new Map<string, number>();
  for (const line of a) counts.set(line, (counts.get(line) ?? 0) + 1);
  let additions = 0;
  for (const line of b) {
    const n = counts.get(line) ?? 0;
    if (n > 0) counts.set(line, n - 1);
    else additions += 1;
  }
  let deletions = 0;
  for (const n of counts.values()) deletions += n;
  return { additions, deletions };
}

/**
 * A file-level three-way merge of `ours` and `theirs` over `base` trees. Returns the merged flat
 * map, or null when both sides changed one path differently (a conflict).
 */
export function mergeFlat(
  base: ReadonlyMap<string, FlatEntry>,
  ours: ReadonlyMap<string, FlatEntry>,
  theirs: ReadonlyMap<string, FlatEntry>,
): Map<string, FlatEntry> | null {
  const result = new Map(ours);
  const paths = new Set<string>([...base.keys(), ...theirs.keys()]);
  for (const path of paths) {
    const b = base.get(path);
    const o = ours.get(path);
    const t = theirs.get(path);
    const same = (x: FlatEntry | undefined, y: FlatEntry | undefined): boolean =>
      x === undefined ? y === undefined : y !== undefined && x.oid === y.oid && x.mode === y.mode;
    if (same(b, t)) continue; // theirs did not change it
    if (same(b, o) || same(o, t)) {
      if (t === undefined) result.delete(path);
      else result.set(path, t);
      continue;
    }
    return null; // both changed it, differently
  }
  return result;
}
