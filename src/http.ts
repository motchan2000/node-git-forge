// Shared HTTP plumbing: JSON replies with the GitHub headers, body reading, auth and the request id.

import type { Req as IncomingMessage, Res as ServerResponse } from './server.ts';

export const JSON_TYPE = 'application/json; charset=utf-8';

let requestCounter = 0;

/** A non-empty, process-unique request id (a counter: no clock, no randomness). */
export function nextRequestId(): string {
  requestCounter += 1;
  return `F0RGE:${requestCounter.toString(16).toUpperCase().padStart(8, '0')}`;
}

export function sendJson(res: ServerResponse, status: number, body: unknown, headers: Readonly<Record<string, string>> = {}): void {
  sendJsonText(res, status, status === 204 ? '' : JSON.stringify(body), headers);
}

/** Answer with an already-serialized JSON body. */
export function sendJsonText(res: ServerResponse, status: number, text: string, headers: Readonly<Record<string, string>> = {}): void {
  if (res.headersSent) {
    res.end();
    return;
  }
  res.writeHead(status, {
    'content-type': JSON_TYPE,
    'content-length': Buffer.byteLength(text),
    'x-github-request-id': nextRequestId(),
    'x-github-media-type': 'github.v3; format=json',
    ...headers,
  });
  res.end(text);
}

export function sendError(res: ServerResponse, status: number, message: string, headers: Readonly<Record<string, string>> = {}): void {
  sendJson(res, status, { message, documentation_url: 'https://docs.github.com/rest', status: String(status) }, headers);
}

export function sendBytes(res: ServerResponse, status: number, contentType: string, body: Buffer, headers: Readonly<Record<string, string>> = {}): void {
  res.writeHead(status, {
    'content-type': contentType,
    'content-length': body.length,
    'cache-control': 'no-cache',
    ...headers,
  });
  res.end(body);
}

/** The whole request body (the server front end has already read it). */
export async function readBody(req: IncomingMessage): Promise<Buffer> {
  return req.body;
}

export class BadJson extends Error {}

/** Parse a JSON request body; an empty body is `{}`; a malformed one throws BadJson. */
export function parseJson(body: Buffer): unknown {
  if (body.length === 0) return {};
  const text = body.toString('utf8');
  if (text.trim() === '') return {};
  try {
    return JSON.parse(text) as unknown;
  } catch {
    throw new BadJson('Problems parsing JSON');
  }
}

/** The bearer (or `token`) credential a request carries, or null. */
export function credentialOf(req: IncomingMessage): string | null {
  const header = req.headers.authorization;
  if (typeof header !== 'string') return null;
  const m = /^(?:bearer|token)\s+(.+)$/i.exec(header.trim());
  if (m !== null) return (m[1] ?? '').trim();
  const basic = /^basic\s+(.+)$/i.exec(header.trim());
  if (basic !== null) {
    // git clients may present the token as a basic-auth password (or user).
    const decoded = Buffer.from(basic[1] ?? '', 'base64').toString('utf8');
    const colon = decoded.indexOf(':');
    const pass = colon >= 0 ? decoded.slice(colon + 1) : decoded;
    return pass === '' ? decoded.slice(0, Math.max(0, colon)) : pass;
  }
  return null;
}

/** The origin a client used to reach us, for absolute URLs (Link headers, html_url). */
export function originOf(req: IncomingMessage): string {
  const host = typeof req.headers.host === 'string' && req.headers.host !== '' ? req.headers.host : 'localhost';
  return `http://${host}`;
}
