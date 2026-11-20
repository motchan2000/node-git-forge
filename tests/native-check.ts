// Start the forge exactly as main.ts does (native.ts startForge) and report which forge answered:
//   node tests/native-check.ts        -> "kind native" when the Rust binary serves, "kind node" on fallback
// Exits 1 when the Node fallback took over, so a broken binary is caught before submitting.
import { startForge } from '../native.ts';

const forge = await startForge();
const res = await fetch(`http://127.0.0.1:${forge.port}/healthz`);
console.log(`kind ${forge.kind} port ${forge.port} healthz ${res.status}`);
forge.stop();
process.exit(forge.kind === 'native' ? 0 : 1);
