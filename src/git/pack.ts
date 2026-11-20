// Packfiles: reading (with OFS_DELTA / REF_DELTA resolution and trailer verification) and writing
// (whole objects, deflated once and cached per oid so repeated clones reuse the work).

import { createHash } from 'node:crypto';
import { deflateSync, inflateSync, constants as zc } from 'node:zlib';
import { CODE_TYPES, TYPE_CODES, hashObject, type GitObject, type ObjectType, type Oid } from './objects.ts';

export class PackError extends Error {}

const OFS_DELTA = 6;
const REF_DELTA = 7;

interface RawEntry {
  readonly offset: number;
  readonly code: number;
  readonly data: Buffer; // inflated: the object, or the delta instructions
  readonly baseOffset?: number;
  readonly baseOid?: Oid;
}

/** Inflate one zlib stream starting at `start`; returns the bytes and how much input it consumed. */
function inflateAt(buf: Buffer, start: number, expected: number): { data: Buffer; used: number } {
  let result: { buffer: Buffer; engine: { bytesWritten: number } };
  try {
    result = inflateSync(buf.subarray(start), { info: true, finishFlush: zc.Z_SYNC_FLUSH }) as unknown as { buffer: Buffer; engine: { bytesWritten: number } };
  } catch (err) {
    throw new PackError(`zlib: ${err instanceof Error ? err.message : String(err)}`);
  }
  if (result.buffer.length !== expected) throw new PackError(`object at ${start} inflated to ${result.buffer.length} bytes, header says ${expected}`);
  return { data: result.buffer, used: result.engine.bytesWritten };
}

/** Apply a git delta to a base. */
export function applyDelta(base: Buffer, delta: Buffer): Buffer {
  let i = 0;
  const varint = (): number => {
    let result = 0;
    let shift = 0;
    let byte: number;
    do {
      if (i >= delta.length) throw new PackError('truncated delta header');
      byte = delta[i++] as number;
      result += (byte & 0x7f) * 2 ** shift;
      shift += 7;
    } while (byte & 0x80);
    return result;
  };
  const baseSize = varint();
  const resultSize = varint();
  if (baseSize !== base.length) throw new PackError(`delta base size ${baseSize} does not match base ${base.length}`);
  const out = Buffer.allocUnsafe(resultSize);
  let o = 0;
  while (i < delta.length) {
    const op = delta[i++] as number;
    if (op & 0x80) {
      let offset = 0;
      let size = 0;
      if (op & 0x01) offset |= delta[i++] as number;
      if (op & 0x02) offset |= (delta[i++] as number) << 8;
      if (op & 0x04) offset |= (delta[i++] as number) << 16;
      if (op & 0x08) offset = (offset | ((delta[i++] as number) << 24)) >>> 0;
      if (op & 0x10) size |= delta[i++] as number;
      if (op & 0x20) size |= (delta[i++] as number) << 8;
      if (op & 0x40) size |= (delta[i++] as number) << 16;
      if (size === 0) size = 0x10000;
      if (offset + size > base.length || o + size > resultSize) throw new PackError('delta copy out of range');
      base.copy(out, o, offset, offset + size);
      o += size;
    } else if (op !== 0) {
      if (i + op > delta.length || o + op > resultSize) throw new PackError('delta insert out of range');
      delta.copy(out, o, i, i + op);
      i += op;
      o += op;
    } else {
      throw new PackError('delta opcode 0 is reserved');
    }
  }
  if (o !== resultSize) throw new PackError(`delta produced ${o} bytes, expected ${resultSize}`);
  return out;
}

export interface ParsedPack {
  readonly objects: Map<Oid, GitObject>;
}

/**
 * Parse and fully resolve a pack. `external` supplies REF_DELTA bases that are not in the pack (thin
 * packs). Throws PackError on any structural problem; nothing is stored by this function.
 */
export function readPack(buf: Buffer, external: (oid: Oid) => GitObject | undefined): ParsedPack {
  if (buf.length < 32) throw new PackError('pack is too short');
  if (buf.toString('ascii', 0, 4) !== 'PACK') throw new PackError('missing PACK signature');
  const version = buf.readUInt32BE(4);
  if (version !== 2 && version !== 3) throw new PackError(`unsupported pack version ${version}`);
  const count = buf.readUInt32BE(8);
  const trailerAt = buf.length - 20;
  const actual = createHash('sha1').update(buf.subarray(0, trailerAt)).digest();
  if (!actual.equals(buf.subarray(trailerAt))) throw new PackError('pack trailer checksum mismatch');

  const entries: RawEntry[] = [];
  const byOffset = new Map<number, number>();
  let i = 12;
  for (let n = 0; n < count; n += 1) {
    if (i >= trailerAt) throw new PackError('pack ends before its declared object count');
    const offset = i;
    let byte = buf[i++] as number;
    const code = (byte >> 4) & 7;
    let size = byte & 0x0f;
    let shift = 4;
    while (byte & 0x80) {
      if (i >= trailerAt) throw new PackError('truncated object header');
      byte = buf[i++] as number;
      size += (byte & 0x7f) * 2 ** shift;
      shift += 7;
    }
    let baseOffset: number | undefined;
    let baseOid: Oid | undefined;
    if (code === OFS_DELTA) {
      byte = buf[i++] as number;
      let ofs = byte & 0x7f;
      while (byte & 0x80) {
        byte = buf[i++] as number;
        ofs = (ofs + 1) * 128 + (byte & 0x7f);
      }
      baseOffset = offset - ofs;
      if (baseOffset < 12) throw new PackError('ofs-delta base before the pack start');
    } else if (code === REF_DELTA) {
      baseOid = buf.toString('hex', i, i + 20);
      i += 20;
    } else if (CODE_TYPES[code] === undefined) {
      throw new PackError(`bad object type ${code} at ${offset}`);
    }
    const { data, used } = inflateAt(buf, i, size);
    i += used;
    byOffset.set(offset, entries.length);
    entries.push({ offset, code, data, baseOffset, baseOid });
  }

  const resolved: (GitObject | undefined)[] = new Array(entries.length);
  const oids: (Oid | undefined)[] = new Array(entries.length);
  const objects = new Map<Oid, GitObject>();
  // Whole objects first.
  entries.forEach((e, idx) => {
    if (e.code !== OFS_DELTA && e.code !== REF_DELTA) {
      const type = CODE_TYPES[e.code] as ObjectType;
      const obj = { type, data: e.data };
      resolved[idx] = obj;
      const oid = hashObject(type, e.data);
      oids[idx] = oid;
      objects.set(oid, obj);
    }
  });
  // Then deltas, repeatedly, until no progress (chains resolve in any order).
  let pending = entries.map((_, idx) => idx).filter((idx) => resolved[idx] === undefined);
  while (pending.length > 0) {
    const next: number[] = [];
    for (const idx of pending) {
      const e = entries[idx] as RawEntry;
      let base: GitObject | undefined;
      if (e.baseOffset !== undefined) {
        const bi = byOffset.get(e.baseOffset);
        if (bi === undefined) throw new PackError(`ofs-delta base at ${e.baseOffset} is not an object start`);
        base = resolved[bi];
      } else if (e.baseOid !== undefined) {
        base = objects.get(e.baseOid) ?? external(e.baseOid);
      }
      if (base === undefined) {
        next.push(idx);
        continue;
      }
      const data = applyDelta(base.data, e.data);
      const obj = { type: base.type, data };
      resolved[idx] = obj;
      const oid = hashObject(base.type, data);
      oids[idx] = oid;
      objects.set(oid, obj);
    }
    if (next.length === pending.length) throw new PackError(`${next.length} delta object(s) have no base`);
    pending = next;
  }
  return { objects };
}

// ---------------------------------------------------------------------------------------------
// Writing

const deflateCache = new Map<Oid, Buffer>();

function objectHeader(type: ObjectType, size: number): Buffer {
  const bytes: number[] = [];
  let byte = (TYPE_CODES[type] << 4) | (size & 0x0f);
  let rest = Math.floor(size / 16);
  while (rest > 0) {
    bytes.push(byte | 0x80);
    byte = rest & 0x7f;
    rest = Math.floor(rest / 128);
  }
  bytes.push(byte);
  return Buffer.from(bytes);
}

/** The pack entry (header + deflated body) for one object, cached by oid. */
export function packEntry(oid: Oid, obj: GitObject): Buffer {
  let entry = deflateCache.get(oid);
  if (entry === undefined) {
    entry = Buffer.concat([objectHeader(obj.type, obj.data.length), deflateSync(obj.data, { level: 1 })]);
    deflateCache.set(oid, entry);
  }
  return entry;
}

/** A version-2 pack of whole objects, in the order given. */
export function writePack(list: readonly (readonly [Oid, GitObject])[]): Buffer {
  const header = Buffer.allocUnsafe(12);
  header.write('PACK', 0, 'ascii');
  header.writeUInt32BE(2, 4);
  header.writeUInt32BE(list.length, 8);
  const parts: Buffer[] = [header];
  for (const [oid, obj] of list) parts.push(packEntry(oid, obj));
  const body = Buffer.concat(parts);
  const trailer = createHash('sha1').update(body).digest();
  return Buffer.concat([body, trailer]);
}
