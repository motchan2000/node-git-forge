// Local test launcher: listens on a fixed port without the env lifecycle (main.ts stays the entry).
import { createApp } from '../app.ts';
const port = Number(process.argv[2] ?? 3000);
createApp().listen(port, '127.0.0.1', () => process.stdout.write(`[serve] http://127.0.0.1:${port}\n`));
// Exit cleanly on a signal so --cpu-prof and similar exit hooks get to write their output.
for (const signal of ['SIGINT', 'SIGTERM'] as const) process.on(signal, () => process.exit(0));
