// Local test launcher: listens on a fixed port without the env lifecycle (main.ts stays the entry).
import { createApp } from '../app.ts';
const port = Number(process.argv[2] ?? 3000);
const host = process.argv[3] ?? '127.0.0.1';
createApp().listen(port, host, () => process.stdout.write(`[serve] http://${host}:${port}\n`));
// Exit cleanly on a signal so --cpu-prof and similar exit hooks get to write their output.
for (const signal of ['SIGINT', 'SIGTERM'] as const) process.on(signal, () => process.exit(0));
