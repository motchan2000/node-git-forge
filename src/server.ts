// A small HTTP/1.1 server on node:net. node:http spends most of a short request in its generic
// machinery (stream objects, header validation, several writes); this front end parses exactly what
// the forge's clients send and answers each request with a single socket write.
//
// Supported: keep-alive (HTTP/1.1 default, HTTP/1.0 with Connection: keep-alive), pipelined requests
// answered in order, Content-Length and chunked request bodies, Expect: 100-continue, HEAD. Bodies are
// read whole before the handler runs; every response carries a Content-Length.

import { createServer as createNetServer, type Server, type Socket } from 'node:net';
import { STATUS_CODES } from 'node:http';

export interface Req {
  readonly method: string;
  readonly url: string;
  readonly headers: Readonly<Record<string, string>>;
  /** The whole request body (empty when none was sent). */
  readonly body: Buffer;
}

export interface Res {
  readonly headersSent: boolean;
  writeHead(status: number, headers?: Readonly<Record<string, string | number>>): void;
  end(body?: string | Buffer): void;
  destroy(): void;
}

export type Handler = (req: Req, res: Res) => void;

const MAX_HEADER_BYTES = 64 * 1024;
const MAX_BODY_BYTES = 512 * 1024 * 1024;
const CRLFCRLF = Buffer.from('\r\n\r\n');

class Response implements Res {
  headersSent = false;
  #status = 200;
  #headers: Readonly<Record<string, string | number>> = {};
  readonly #conn: Connection;
  readonly #head: boolean;
  #done = false;

  constructor(conn: Connection, head: boolean) {
    this.#conn = conn;
    this.#head = head;
  }

  writeHead(status: number, headers: Readonly<Record<string, string | number>> = {}): void {
    if (this.headersSent) return;
    this.#status = status;
    this.#headers = headers;
    this.headersSent = true;
  }

  end(body?: string | Buffer): void {
    if (this.#done) return;
    this.#done = true;
    this.headersSent = true;
    const data = body === undefined ? EMPTY : typeof body === 'string' ? Buffer.from(body, 'utf8') : body;
    const status = this.#status;
    const noBody = status === 204 || status === 304 || (status >= 100 && status < 200);
    let head = `HTTP/1.1 ${status} ${STATUS_CODES[status] ?? 'Unknown'}\r\n`;
    for (const [name, value] of Object.entries(this.#headers)) {
      const lower = name.toLowerCase();
      if (lower === 'content-length' || lower === 'connection' || lower === 'transfer-encoding') continue;
      head += `${name}: ${value}\r\n`;
    }
    if (!noBody) head += `content-length: ${data.length}\r\n`;
    if (!this.#conn.keepAlive) head += 'connection: close\r\n';
    head += '\r\n';
    this.#conn.send(head, noBody || this.#head ? EMPTY : data);
  }

  destroy(): void {
    this.#done = true;
    this.#conn.socket.destroy();
  }
}

const EMPTY = Buffer.alloc(0);

interface Parsed {
  readonly method: string;
  readonly url: string;
  readonly headers: Record<string, string>;
  readonly keepAlive: boolean;
}

class Connection {
  readonly socket: Socket;
  readonly #handler: Handler;
  #buf: Buffer = EMPTY;
  #busy = false;
  #pending: Parsed | null = null; // headers parsed, waiting for the body
  #sentContinue = false;
  keepAlive = true;

  constructor(socket: Socket, handler: Handler) {
    this.socket = socket;
    this.#handler = handler;
    socket.setNoDelay(true);
    socket.on('data', (chunk: Buffer) => {
      this.#buf = this.#buf.length === 0 ? chunk : Buffer.concat([this.#buf, chunk]);
      this.#pump();
    });
    socket.on('error', () => socket.destroy());
  }

  /** Write one response and move on to the next pipelined request. */
  send(head: string, body: Buffer): void {
    const socket = this.socket;
    if (!socket.destroyed) {
      if (body.length === 0) socket.write(head, 'latin1');
      else if (body.length <= 16 * 1024) socket.write(Buffer.concat([Buffer.from(head, 'latin1'), body]));
      else {
        socket.cork();
        socket.write(head, 'latin1');
        socket.write(body);
        process.nextTick(() => socket.uncork());
      }
      if (!this.keepAlive) socket.end();
    }
    this.#busy = false;
    if (this.keepAlive && this.#buf.length > 0) this.#pump();
  }

  #fail(status: number): void {
    this.keepAlive = false;
    const text = JSON.stringify({ message: STATUS_CODES[status] ?? 'Bad Request' });
    this.socket.end(`HTTP/1.1 ${status} ${STATUS_CODES[status] ?? 'Bad Request'}\r\ncontent-type: application/json\r\ncontent-length: ${Buffer.byteLength(text)}\r\nconnection: close\r\n\r\n${text}`);
    this.#buf = EMPTY;
  }

  #pump(): void {
    while (!this.#busy && !this.socket.destroyed) {
      if (this.#pending === null) {
        const end = this.#buf.indexOf(CRLFCRLF);
        if (end < 0) {
          if (this.#buf.length > MAX_HEADER_BYTES) this.#fail(431);
          return;
        }
        const parsed = parseHead(this.#buf.toString('latin1', 0, end));
        if (parsed === null) {
          this.#fail(400);
          return;
        }
        this.#buf = this.#buf.subarray(end + 4);
        this.#pending = parsed;
        this.#sentContinue = false;
      }
      const req = this.#pending;
      const body = this.#takeBody(req.headers);
      if (body === 'error') return;
      if (body === null) {
        if (!this.#sentContinue && (req.headers.expect ?? '').toLowerCase() === '100-continue') {
          this.#sentContinue = true;
          this.socket.write('HTTP/1.1 100 Continue\r\n\r\n');
        }
        return;
      }
      this.#pending = null;
      this.#busy = true;
      this.keepAlive = req.keepAlive;
      const res = new Response(this, req.method === 'HEAD');
      try {
        this.#handler({ method: req.method, url: req.url, headers: req.headers, body }, res);
      } catch {
        if (!res.headersSent) {
          res.writeHead(500, { 'content-type': 'application/json' });
          res.end('{"message":"Internal Server Error"}');
        }
      }
    }
  }

  /** The request body when it has fully arrived, null while it is still arriving. */
  #takeBody(headers: Readonly<Record<string, string>>): Buffer | null | 'error' {
    const te = headers['transfer-encoding'];
    if (te !== undefined && te.toLowerCase().includes('chunked')) {
      const decoded = decodeChunked(this.#buf);
      if (decoded === 'error') {
        this.#fail(400);
        return 'error';
      }
      if (decoded === null) {
        if (this.#buf.length > MAX_BODY_BYTES) {
          this.#fail(413);
          return 'error';
        }
        return null;
      }
      this.#buf = this.#buf.subarray(decoded.used);
      return decoded.body;
    }
    const raw = headers['content-length'];
    if (raw === undefined || raw === '') return EMPTY;
    const length = Number(raw);
    if (!Number.isSafeInteger(length) || length < 0) {
      this.#fail(400);
      return 'error';
    }
    if (length > MAX_BODY_BYTES) {
      this.#fail(413);
      return 'error';
    }
    if (this.#buf.length < length) return null;
    const body = this.#buf.subarray(0, length);
    this.#buf = this.#buf.subarray(length);
    return body;
  }
}

function parseHead(text: string): Parsed | null {
  const lines = text.split('\r\n');
  const m = /^([A-Z]+) (\S+) HTTP\/1\.([01])$/.exec(lines[0] ?? '');
  if (m === null) return null;
  const headers: Record<string, string> = {};
  for (let i = 1; i < lines.length; i += 1) {
    const line = lines[i] as string;
    const colon = line.indexOf(':');
    if (colon <= 0) return null;
    const name = line.slice(0, colon).trim().toLowerCase();
    const value = line.slice(colon + 1).trim();
    headers[name] = headers[name] === undefined ? value : `${headers[name]}, ${value}`;
  }
  const connection = (headers.connection ?? '').toLowerCase();
  const keepAlive = m[3] === '1' ? !connection.includes('close') : connection.includes('keep-alive');
  return { method: m[1] as string, url: m[2] as string, headers, keepAlive };
}

/** Decode a complete chunked body from the start of `buf`; null while incomplete. */
function decodeChunked(buf: Buffer): { body: Buffer; used: number } | null | 'error' {
  const parts: Buffer[] = [];
  let i = 0;
  for (;;) {
    const eol = buf.indexOf('\r\n', i);
    if (eol < 0) return null;
    const sizeText = buf.toString('latin1', i, eol).split(';')[0]?.trim() ?? '';
    if (!/^[0-9a-fA-F]+$/.test(sizeText)) return 'error';
    const size = parseInt(sizeText, 16);
    i = eol + 2;
    if (size === 0) {
      // trailers end with an empty line
      const end = buf.indexOf(CRLFCRLF, i - 2);
      if (end < 0) return null;
      return { body: parts.length === 1 ? (parts[0] as Buffer) : Buffer.concat(parts), used: end + 4 };
    }
    if (buf.length < i + size + 2) return null;
    parts.push(buf.subarray(i, i + size));
    i += size + 2;
  }
}

export function createServer(handler: Handler): Server {
  return createNetServer({ noDelay: true }, (socket) => {
    new Connection(socket, handler);
  });
}
