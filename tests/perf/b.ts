// Selected benchmark workloads, several runs: node --experimental-strip-types b.ts <url> <workloads> <reps> (KIT=<kit dir>)
const { runBenchmark } = await import(`${process.env.KIT}/sdk/conformance/bench.ts`);
const url = process.argv[2] ?? 'http://127.0.0.1:3000';
const wl = (process.argv[3] ?? 'push,clone,read,write').split(',') as any;
const reps = Number(process.argv[4] ?? 3);
const all: Record<string, number[]> = {};
for (let r = 0; r < reps; r++) {
  const p = await runBenchmark({ baseUrl: url, token: 'tk', workloads: wl, rounds: 5 });
  for (const w of p.workloads) (all[w.name] ??= []).push(...w.rounds);
}
for (const [k, v] of Object.entries(all)) { v.sort((a,b)=>a-b); console.log(k.padEnd(6), "median", v[v.length>>1].toFixed(1), "min", v[0].toFixed(1), "all", v.map(x=>x.toFixed(0)).join(" ")); }
