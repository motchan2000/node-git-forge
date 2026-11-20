// pkt-line framing: 4 hex digits of total length, then the payload; 0000 flush, 0001 delim,
// 0002 response-end. Lengths count bytes, never characters.

export type Packet = { kind: 'data'; payload: Buffer } | { kind: 'flush' } | { kind: 'delim' } | { kind: 'end' };

export const PKT_MAX_PAYLOAD = 65516;
export const FLUSH = Buffer.from('0000');
export const DELIM = Buffer.from('0001');

export class PktError extends Error {}

export function pkt(payload: string | Buffer): Buffer {
  const body = typeof payload === 'string' ? Buffer.from(payload, 'utf8') : payload;
  const header = Buffer.from((body.length + 4).toString(16).padStart(4, '0'), 'ascii');
  return Buffer.concat([header, body]);
}

/** Parse a whole body into packets. Returns the packets and the offset just past the last one read. */
export function parsePackets(buf: Buffer, start = 0, stopAfterFlushes = Infinity): { packets: Packet[]; offset: number } {
  const packets: Packet[] = [];
  let i = start;
  let flushes = 0;
  while (i < buf.length && flushes < stopAfterFlushes) {
    if (i + 4 > buf.length) throw new PktError('truncated pkt-line header');
    const hex = buf.toString('ascii', i, i + 4);
    if (!/^[0-9a-fA-F]{4}$/.test(hex)) throw new PktError(`bad pkt-line length ${JSON.stringify(hex)}`);
    const len = parseInt(hex, 16);
    if (len === 0) {
      packets.push({ kind: 'flush' });
      flushes += 1;
      i += 4;
      continue;
    }
    if (len === 1) {
      packets.push({ kind: 'delim' });
      i += 4;
      continue;
    }
    if (len === 2) {
      packets.push({ kind: 'end' });
      i += 4;
      continue;
    }
    if (len < 4) throw new PktError(`bad pkt-line length ${len}`);
    if (i + len > buf.length) throw new PktError('truncated pkt-line payload');
    packets.push({ kind: 'data', payload: buf.subarray(i + 4, i + len) });
    i += len;
  }
  return { packets, offset: i };
}

/** A data packet's text without its trailing newline. */
export function lineOf(p: Packet): string | null {
  if (p.kind !== 'data') return null;
  const text = p.payload.toString('utf8');
  return text.endsWith('\n') ? text.slice(0, -1) : text;
}

/** Frame bytes into side-band packets on `band`, each within the pkt-line limit. */
export function sideband(band: 1 | 2 | 3, data: Buffer, chunk = PKT_MAX_PAYLOAD - 1): Buffer[] {
  const out: Buffer[] = [];
  for (let i = 0; i < data.length; i += chunk) {
    const part = data.subarray(i, Math.min(data.length, i + chunk));
    const header = Buffer.from((part.length + 5).toString(16).padStart(4, '0'), 'ascii');
    out.push(header, Buffer.from([band]), part);
  }
  return out;
}
