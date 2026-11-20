// The forge's HTTP server: one store (src/store.ts) behind four surfaces — git smart HTTP at
// /{owner}/{repo}.git, REST v3 under /api/v3, GraphQL at /api/graphql and MCP at /mcp — plus the
// /healthz probe. main.ts owns the lifecycle; this file builds the server it listens with.

import type { Server } from 'node:net';
import { createServer, type Req as IncomingMessage, type Res as ServerResponse } from './src/server.ts';
import { Forge, ForgeError, type User } from './src/store.ts';
import { BadJson, credentialOf, originOf, sendError, sendJson } from './src/http.ts';
import { handleRest } from './src/api/rest.ts';
import { handleGit } from './src/git/smart-http.ts';
import { handleGraphQL } from './src/api/graphql.ts';
import { handleMcp } from './src/api/mcp.ts';

/**
 * The account the trial's token names. The suite's scenario seeds this login as the token's owner;
 * FORGE_LOGIN overrides it for other deployments.
 */
function viewerFromEnv(): User {
  const login = process.env.FORGE_LOGIN ?? 'adaiversen';
  const name = process.env.FORGE_NAME ?? (login === 'adaiversen' ? 'ada iversen' : login);
  return { id: 1000, login, name, email: `${login}@example.invalid` };
}

/** Whether a request carries the trial's token (any non-empty token when none is configured). */
function authorized(req: IncomingMessage): boolean {
  const presented = credentialOf(req);
  if (presented === null || presented === '') return false;
  const expected = process.env.MAKEFUN_SHOP_TOKEN;
  return expected === undefined || expected === '' || presented === expected;
}

async function handle(forge: Forge, req: IncomingMessage, res: ServerResponse): Promise<void> {
  let url: URL;
  try {
    url = new URL(req.url ?? '/', 'http://forge');
  } catch {
    sendError(res, 404, 'Not Found');
    return;
  }
  const path = url.pathname;
  const method = req.method ?? 'GET';

  if (path === '/healthz') {
    if (method === 'GET' || method === 'HEAD') sendJson(res, 200, { ok: true });
    else sendError(res, 404, 'Not Found');
    return;
  }

  if (path === '/api/v3' || path.startsWith('/api/v3/')) {
    if (!authorized(req)) {
      sendError(res, 401, credentialOf(req) === null ? 'Requires authentication' : 'Bad credentials');
      return;
    }
    await handleRest(forge, req, res, url, originOf(req));
    return;
  }

  if (path === '/api/graphql' || path === '/graphql') {
    if (!authorized(req)) {
      sendError(res, 401, 'This endpoint requires you to be authenticated.');
      return;
    }
    if (method === 'POST') await handleGraphQL(forge, req, res, originOf(req));
    else sendError(res, 404, 'Not Found');
    return;
  }

  if (path === '/mcp' || path === '/mcp/') {
    await handleMcp(forge, req, res, originOf(req), authorized(req));
    return;
  }

  if (path.includes('.git/')) {
    if (await handleGit(forge, req, res, url, authorized(req))) return;
  }

  sendError(res, 404, 'Not Found');
}

/** Build the server. It binds nothing until the caller listens; the entry listens on an ephemeral port. */
export function createApp(): Server {
  const forge = new Forge(viewerFromEnv());
  return createServer((req, res) => {
    handle(forge, req, res).catch((err: unknown) => {
      if (err instanceof BadJson) {
        sendError(res, 400, 'Problems parsing JSON');
        return;
      }
      if (err instanceof ForgeError) {
        sendError(res, err.status, err.message);
        return;
      }
      process.stderr.write(`[forge] ${req.method} ${req.url}: ${err instanceof Error ? (err.stack ?? err.message) : String(err)}\n`);
      if (!res.headersSent) sendError(res, 500, 'Internal Server Error');
      else res.destroy();
    });
  });
}
