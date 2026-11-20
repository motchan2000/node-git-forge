// Starting the forge: the native binary (bin/forge-linux-amd64, built from rust/ as a static glibc
// executable) when this host can run it, else the Node forge from app.ts. Either way the caller gets a
// bound port and a stop function, and a forge that keeps serving on that port for the whole trial.
//
// The binary prints "FORGE_PORT <n>" once it listens. It is accepted only after /healthz answers 200
// there. If it later exits on its own, it is started again on the same port (the env dials the port
// registered first, which cannot change), and if that fails the Node forge takes the port over.

import { spawn, type ChildProcess } from 'node:child_process';
import { copyFileSync, chmodSync, existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { createApp } from './app.ts';

export interface RunningForge {
  readonly port: number;
  readonly kind: 'native' | 'node';
  stop(): void;
}

const BINARY = fileURLToPath(new URL('./bin/forge-linux-amd64', import.meta.url));
const START_TIMEOUT_MS = 5_000;

function log(line: string): void {
  process.stdout.write(`[entry] ${line}\n`);
}

/** Start the binary at `path` on `port` (0 = ephemeral); resolve its port, or reject. */
function launch(path: string, port: number): Promise<{ child: ChildProcess; port: number }> {
  return new Promise((resolve, reject) => {
    let child: ChildProcess;
    try {
      child = spawn(path, [String(port)], { stdio: ['ignore', 'pipe', 'inherit'], env: process.env });
    } catch (err) {
      reject(err);
      return;
    }
    let settled = false;
    let out = '';
    const timer = setTimeout(() => finish(new Error('the native forge did not report a port in time')), START_TIMEOUT_MS);
    const finish = (err: Error | null, bound?: number): void => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      if (err !== null) {
        child.kill('SIGKILL');
        reject(err);
      } else {
        resolve({ child, port: bound as number });
      }
    };
    child.once('error', (err) => finish(err));
    child.once('exit', (code, signal) => finish(new Error(`the native forge exited early (${code ?? signal})`)));
    child.stdout?.on('data', (chunk: Buffer) => {
      out += chunk.toString('utf8');
      const m = /FORGE_PORT (\d+)/.exec(out);
      if (m !== null) finish(null, Number(m[1]));
    });
  });
}

async function healthy(port: number): Promise<boolean> {
  for (let attempt = 0; attempt < 20; attempt += 1) {
    try {
      const res = await fetch(`http://127.0.0.1:${port}/healthz`, { signal: AbortSignal.timeout(1_000) });
      const body = await res.text();
      if (res.status === 200 && body.includes('"ok":true')) return true;
    } catch {
      // not yet listening
    }
    await new Promise((r) => setTimeout(r, 50));
  }
  return false;
}

/** Start the binary, copying it to /tmp and marking it executable when /bot's copy cannot run. */
async function startNative(port: number): Promise<{ child: ChildProcess; port: number }> {
  try {
    return await launch(BINARY, port);
  } catch (err) {
    const code = (err as NodeJS.ErrnoException).code;
    if (code !== 'EACCES' && code !== 'EPERM') throw err;
    const copy = `/tmp/forge-linux-amd64-${process.pid}`;
    copyFileSync(BINARY, copy);
    chmodSync(copy, 0o755);
    return await launch(copy, port);
  }
}

function startNode(port: number): Promise<RunningForge> {
  const server = createApp();
  return new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(port, () => {
      const address = server.address();
      if (address === null || typeof address === 'string') {
        reject(new Error('the server did not bind a TCP port'));
        return;
      }
      resolve({ port: address.port, kind: 'node', stop: () => server.close() });
    });
  });
}

export async function startForge(): Promise<RunningForge> {
  const usable = process.platform === 'linux' && process.arch === 'x64' && existsSync(BINARY) && process.env.FORGE_NATIVE !== '0';
  if (usable) {
    try {
      const first = await startNative(0);
      if (await healthy(first.port)) {
        const port = first.port;
        let child = first.child;
        let stopping = false;
        let fallback: RunningForge | null = null;
        const watch = (c: ChildProcess): void => {
          c.once('exit', (code, signal) => {
            if (stopping) return;
            log(`the native forge exited (${code ?? signal}); restarting it on port ${port}`);
            startNative(port)
              .then((next) => {
                child = next.child;
                watch(child);
              })
              .catch(async (err: unknown) => {
                log(`the native forge did not restart (${err instanceof Error ? err.message : String(err)}); the Node forge takes port ${port}`);
                fallback = await startNode(port).catch(() => null);
              });
          });
        };
        watch(child);
        return {
          port,
          kind: 'native',
          stop: () => {
            stopping = true;
            child.kill('SIGTERM');
            fallback?.stop();
          },
        };
      }
      first.child.kill('SIGKILL');
      log('the native forge did not answer /healthz; using the Node forge');
    } catch (err) {
      log(`the native forge did not start (${err instanceof Error ? err.message : String(err)}); using the Node forge`);
    }
  }
  return startNode(0);
}
