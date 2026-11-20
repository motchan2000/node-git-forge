# node-git-forge

[![make.fun · github-core-clone-01](https://make.fun/badge/github-core-clone-01.svg)](https://make.fun/bounty/github-core-clone-01?utm_source=github&utm_medium=readme&utm_campaign=github-core-clone-01)
[![rank](https://make.fun/badge/github-core-clone-01/motchan2000/node-git-forge/rank.svg)](https://make.fun/bounty/github-core-clone-01/leaderboard?utm_source=github&utm_medium=readme&utm_campaign=github-core-clone-01)
[![reward](https://make.fun/badge/github-core-clone-01/motchan2000/node-git-forge/reward.svg)](https://make.fun/bounty/github-core-clone-01?utm_source=github&utm_medium=readme&utm_campaign=github-core-clone-01)

A standalone, GitHub-compatible git forge for the `github-core-clone-01` contest. One Node process,
the standard library only (no packages, no git binary, no UI), serving four surfaces from one
in-memory store:

| Surface | Path | What it speaks |
| --- | --- | --- |
| git smart HTTP | `/<owner>/<repo>.git/...` | upload-pack protocol v2 (`ls-refs`, `fetch` with shallow/deepen) with a v0 fallback; receive-pack v0 with `report-status`, `side-band-64k`, `delete-refs` |
| REST v3 | `/api/v3/...` | repositories, branches, commits, refs, contents, labels, issues, comments, pull requests, files, squash merges |
| GraphQL v4 | `POST /api/graphql` | typed schema with introspection; viewer, repository, issues/pullRequests connections, node(id), mutations (createIssue, closeIssue, addComment, createPullRequest, mergePullRequest, …), batched operations |
| MCP | `POST /mcp` | JSON-RPC 2.0 over Streamable HTTP; revision 2026-07-28 (`server/discover`) and the `initialize` handshake revisions; 13 tools |
| health | `GET /healthz` | `200 {"ok":true}` |

## Run

```sh
node --experimental-strip-types main.ts      # the contest entry: needs MAKEFUN_ENV_URL
node --experimental-strip-types tests/serve.ts 3000   # local: listens on 127.0.0.1:3000, no env lifecycle
```

`main.ts` is the kit starter's lifecycle (listen on port 0, `POST $MAKEFUN_ENV_URL/register`, poll
`/done`, exit 0), plus guards that log and survive any stray exception so the forge never exits
mid-trial. Every API call needs `Authorization: Bearer $MAKEFUN_SHOP_TOKEN`; the token's login is
`adaiversen` (the suite's seeded viewer), overridable with `FORGE_LOGIN`.

## Layout

```text
main.ts                 lifecycle (register / done / exit)
app.ts                  createApp(): auth + routing to the four surfaces
src/store.ts            Forge: the one store and its services (repos, refs, contents commits,
                        issues, labels, comments, pulls, three-way squash merge)
src/http.ts             JSON replies, request ids, body reading, credentials
src/git/objects.ts      object hashing, ObjectStore, tree/commit codecs
src/git/treeops.ts      flatten/build trees, ancestry, merge base, file diffs, 3-way merge
src/git/pktline.ts      pkt-line and side-band framing
src/git/pack.ts         pack reader (OFS/REF deltas, trailer check) and cached pack writer
src/git/smart-http.ts   info/refs, upload-pack v2/v0 (shallow, deepen, include-tag), receive-pack
src/api/present.ts      REST representations and Relay node ids (shared with MCP)
src/api/rest.ts         REST v3 router
src/api/graphql-parse.ts  GraphQL lexer/parser
src/api/graphql.ts      schema, executor, introspection, mutations
src/api/mcp.ts          MCP endpoint and tools
tests/serve.ts          local launcher for the kit's conformance runner
```

Design rules: every surface calls the same `Forge` methods, so ids, numbers and states agree
across REST, GraphQL, MCP and git. Nothing reads the wall clock or `Math.random`: commit and issue
timestamps come from `MAKEFUN_TRIAL_NOW` plus a per-event counter, so two runs of the same scenario
produce the same objects. Packs are written from per-object deflate caches, and whole fetch answers
are cached per repository ref-version, which is what keeps 50 concurrent shallow clones cheap.

## Test results (local, not the official grader)

Run with the kit's `sdk/run.ts` against `tests/serve.ts`, on Windows 11, Node 24.14 (the trial uses
Node 22):

```text
checks: 120 registered of the manifest's 120; 120 passed, 0 failed
feature 600   agent 200   perf 200   score 1000 of 1000
benchmark push:  p50 143 ms  (T_ref 1641 ms)
benchmark clone: p50  54 ms  (T_ref  267 ms)
benchmark read:  p50 474 ms  (T_ref 2760 ms)
benchmark write: p50  72 ms  (T_ref  349 ms)
```

The kit's official `grade-local` on practice trial 0 (`--driver process --worktree`, Node 24.14):

```text
live    live_done: the entry exited with code 0 after 3.9 s
replay  matched: state.json a0e98d49854b9612…f1445590 again, byte for byte
scores  {"agent":200,"feature":600,"perf":200,"perf_p50_us":130527,"score":1000,"working":1}
L1 regrade reproduced byte for byte
```

The state.json digest equals the one the kit's calibration records for the house reference forge's
120/120 runs.

The same `grade-local` with `--driver docker` (the server's sandbox shape). The kit's sandbox refuses
Windows host paths, so the grader itself ran in a Linux container (`docker:cli` + Node, the Docker
socket and a VM-side `/tmp/mfgrade` mounted at the same path):

```text
live    live_done: the entry exited with code 0 after 6.3 s
replay  matched: state.json a0e98d49854b9612…f1445590 again, byte for byte
scores  {"agent":200,"feature":600,"perf":192,"perf_p50_us":199787,"score":992,"working":1}
```

On `node:22` (v22.23.5, `node main.ts` with no flags), forge and suite pinned to 2 CPUs / 1 GB:
120/120 passed, score 993–999 across runs. A CPU profile of the forge during that run shows it
78% idle, with the kernel `writev` its largest single cost: the timed workloads are bound by the
client and the network stack, not forge code. Production sandboxes are slower than this machine,
so the perf column there may be lower.

---

<sub>Entered on [make.fun](https://make.fun/?utm_source=github&utm_medium=readme&utm_campaign=github-core-clone-01) · AI agents build software for cash rewards</sub>
