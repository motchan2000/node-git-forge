# テスト・計測の引き継ぎ書（github-core-clone-01 / node-git-forge）

作成: 2026-11-20（クラウドセッション session_01R2SyqcRXZ6z6Vxb6zdjCSV）
対象: ブランチ `claude/intelligent-maxwell-nll05r`（最新の確認済み版。HEAD を参照）

この文書は「次に変更を入れる人が、壊していないか・速くなったかを確かめる」ための手順書です。
コンペのルール・提出手順・設計の経緯は `HANDOFF.md` を参照してください。

---

## 0. 変更を入れたら必ずやること（チェックリスト）

| # | 確認 | コマンド（詳細は各章） | 合格条件 |
|---|---|---|---|
| 1 | ビルド | 2 章 | `bin/forge-linux-amd64` が更新され、git 上で mode 100755 |
| 2 | Rust 版が起動するか | `node tests/native-check.ts` | `kind native` と表示され、終了コード 0 |
| 3 | 機能 120 チェック | 3 章 | `120 passed, 0 failed` |
| 4 | 公式採点＋リプレイ一致 | 4 章 | `replay matched`、feature 600・agent 200 |
| 5 | システムコール数 | 6 章 | 定常状態で 1 リクエスト＝recvfrom 1＋sendto 1。futex/mmap/munmap が数千回出ていない |
| 6 | 性能（必要なら） | 5 章 | 旧版と**交互に**計測して悪化していない |

2〜4 のどれかが落ちたら提出しないこと。**最後の提出が採点に使われる**ので、未確認の版を最後に出さない。

---

## 1. 準備

### 必要なもの

| もの | 用途 | 入手 |
|---|---|---|
| 公式キット `github-core-clone-01-kit.tar.gz` | 機能チェック・ベンチ・公式採点 | ユーザーから受け取る。**リポジトリには入れない** |
| Node 22 | キット・main.ts | — |
| Rust（`x86_64-unknown-linux-gnu` ターゲット） | バイナリのビルド | rustup、または Docker の `rust:1` |
| gVisor `runsc` | 本番に近い性能計測 | 下記（root が必要） |
| strace | システムコール数の確認 | `apt-get install -y strace` |
| Docker | 公式採点（grade-local） | デーモンが止まっていれば `dockerd &` |

### キットの展開

```sh
mkdir -p ~/kit && tar xzf github-core-clone-01-kit.tar.gz -C ~/kit
export KIT=~/kit/github-core-clone-01-kit      # 以降のスクリプトはこの変数を使う
```

### gVisor（runsc）の入手（公式リリース、SHA-512 確認つき）

```sh
U=https://storage.googleapis.com/gvisor/releases/release/latest/x86_64
curl -fsSLO $U/gvisor.tar.bz2 && curl -fsSLO $U/gvisor.tar.bz2.sha512 && sha512sum -c gvisor.tar.bz2.sha512
tar -xjf gvisor.tar.bz2 -C /usr/local/bin        # runsc と gvisor-bin/ が並ぶ
```

`$U/runsc` を直接取ると 404 になる。tar.bz2 を使うこと。

---

## 2. ビルド

本番用バイナリは **静的リンクの glibc 版**（2026-11-20 に musl から変更。理由は 7 章）。

```sh
cd rust
RUSTFLAGS="-C target-feature=+crt-static" cargo build --release --target x86_64-unknown-linux-gnu
cp target/x86_64-unknown-linux-gnu/release/forge ../bin/forge-linux-amd64
cd .. && git update-index --chmod=+x bin/forge-linux-amd64
```

Docker で作る場合（Windows など）:

```sh
docker run --rm -v "$PWD/rust:/src" -v "$PWD/bin:/out" -w /src \
  -e RUSTFLAGS="-C target-feature=+crt-static" rust:1 \
  sh -c 'cargo build --release --target x86_64-unknown-linux-gnu && cp target/x86_64-unknown-linux-gnu/release/forge /out/forge-linux-amd64'
```

- `file bin/forge-linux-amd64` が `static-pie linked` と出ること。
- **musl ターゲットに戻さないこと**（7 章の性能退行が再発する）。
- `rust/src/main.rs` の `tune_malloc()` は glibc 専用（`cfg(target_env = "gnu")`）。musl ビルドでは何もしない。

### Rust 版が起動するかの確認

```sh
node tests/native-check.ts                                              # 直接
runsc --network=none --ignore-cgroups do node tests/native-check.ts     # gVisor 内
docker run --rm -v "$PWD:/bot:ro" node:22 node /bot/tests/native-check.ts   # 本番と同じ読み取り専用 /bot
```

`kind native` なら OK。`kind node` は Rust 版が起動できず Node 版に切り替わったことを意味する（終了コード 1）。
Node 版でも 120/120 は通るが遅いので、**チェック 3〜4 だけでは気づけない**。必ずこれで確認すること。
2026-11-20 時点で node:22（Debian）・node:22-alpine・gVisor のすべてで `kind native` を確認済み。

---

## 3. 機能テスト（120 チェック）

```sh
MAKEFUN_SHOP_TOKEN=tk ./bin/forge-linux-amd64 3000 &        # "FORGE_PORT 3000" が出る
node --experimental-strip-types $KIT/sdk/run.ts --url http://127.0.0.1:3000 --token tk          # 120/120
node --experimental-strip-types $KIT/sdk/run.ts --url http://127.0.0.1:3000 --token tk --perf   # 性能も（参考値）
```

- 合格: `checks: 120 registered of the manifest's 120; 120 passed, 0 failed`。
- `--perf` の点数は gVisor なしなので本番よりずっと高く出る（198〜200）。性能の判断には使わない。
- Node 版を試すとき: `node tests/serve.ts 3000 127.0.0.1`。

---

## 4. 公式採点（grade-local）

```sh
node $KIT/cli/makefun.mjs grade-local --kit $KIT --bot . --driver docker --worktree    # 作業ツリーをそのまま
node $KIT/cli/makefun.mjs grade-local --kit $KIT --bot . --driver docker --commit HEAD # コミットを採点
```

- 合格: `replay matched: state.json ... byte for byte` と `scores {"agent":200,"feature":600,...}`。
  リプレイ不一致は本選で失格になるので最重要。
- 2026-11-20 の結果（Docker/runc、クラウド VM）: 新版 983〜984、旧版 984。runc では両者に差が出ない
  （改善点はシステムコールの削減で、gVisor でしか効かない）。perf はこの環境では 183〜184。
- Windows では Docker ドライバがホストパスを拒否する。Linux から実行すること。
- **Docker の既定ランタイムを runsc にして grade-local を gVisor で動かす試みは失敗した**（コンテナ間の
  ネットワークが通らず止まる）。gVisor での計測は 5 章のスクリプトで行う。

---

## 5. 性能計測（gVisor）

### 大前提：計測は大きくぶれる

- クラウド VM（Firecracker）でも同じ条件で ±30% 以上ぶれる。本番は時間帯で約 50% ぶれる（同一コードで 416 ms → 626 ms）。
- **1 回ずつの比較では何も言えない**。必ず「旧版と新版を交互に」複数回動かし、中央値で比べる。
- 本番はこの環境の gVisor より 1.6〜2.4 倍遅い。

### スクリプト（`tests/perf/`）

| スクリプト | 内容 | 使い方 |
|---|---|---|
| `gv.sh` | forge＋選んだワークロードを 1 つの gVisor に入れて計測（既定 2 CPU に固定） | `KIT=$KIT sh tests/perf/gv.sh <バイナリ> push,clone,read,write <回数> [環境変数...]` |
| `gab.sh` | 複数の forge を同じ gVisor に起動し、GET/POST を短いブロックで交互に計測（ぶれが打ち消し合う） | `sh tests/perf/gab.sh "<バイナリ1> [ENV=..]" "<バイナリ2>"` |
| `b.ts` | gv.sh が使うベンチ本体（キットの `runBenchmark` を 5 ラウンド×回数） | 直接: `KIT=$KIT node --experimental-strip-types tests/perf/b.ts <url> <workloads> <回数>` |
| `ab.mjs` | gab.sh が使う交互計測クライアント | — |
| `gvisor-perf.sh` | キットの run.ts（120 チェック＋perf）を gVisor 内で 1 回 | `sh tests/perf/gvisor-perf.sh $KIT rust` |
| `syscalls.sh` | ワークロードごとのシステムコール数（6 章） | `KIT=$KIT sh tests/perf/syscalls.sh [バイナリ] [workloads]` |

- `gv.sh` の CPU 数は `CPUS` で変える（例: `CPUS=0` で 1 CPU、`CPUS=0-1` で 2 CPU）。
  本番の混雑を想定するなら **1〜2 CPU で悪化しないこと**を確認する。
- 出力例: `read   median 934.0 min 870.1 all 870 934 1153 ...`（ラウンドごとの ms）。

### 交互計測の定型（旧版と新版を 4 回ずつ）

```sh
cp bin/forge-linux-amd64 /tmp/new; git show <旧コミット>:bin/forge-linux-amd64 > /tmp/old; chmod +x /tmp/old
for i in 1 2 3 4; do for b in old new; do echo "== $b"; KIT=$KIT sh tests/perf/gv.sh /tmp/$b push,clone,read,write 1; done; done
```

ワークロードごとに 4 回の中央値を並べて比べる。差がばらつきの範囲に収まるなら「差なし」と判断する。

---

## 6. システムコール数（いちばん確実な指標）

時間はぶれるが、システムコールの回数はぶれない。gVisor ではシステムコール（特に futex・mmap・munmap・mprotect）が
高価なので、**回数が減れば本番でも速くなる、増えれば遅くなる**と考えてよい。

```sh
KIT=$KIT sh tests/perf/syscalls.sh bin/forge-linux-amd64
```

2026-11-20 の基準値（現行版、各ワークロード 1 回分）:

| ワークロード | 主なシステムコール |
|---|---|
| push | recvfrom 2014 / sendto 2012 / mprotect 161 / mmap 160（後ろ 2 つはスレッド起動時のみ） |
| clone | recvfrom 1378 / sendto 1279 / mprotect 229 / mmap 228 / setsockopt 197 / futex 約 180 |
| read | recvfrom 10010 / sendto 10008 / mprotect 161 / mmap 160 |
| write | recvfrom 1009 / sendto 1007 / mprotect 161 / mmap 160 |

退行の目安: clone の futex が数千〜数万、write/push の mmap・munmap がリクエスト数と同程度以上に増えたら要調査。
（musl 版では clone の futex が約 40,000、write/push で 1 操作あたり mmap/munmap 約 5 回だった。）

個別の確認には `strace -f -tt -e trace=recvfrom,sendto` で、受信から送信までの時間（サーバー処理）と、
送信から次の受信までの時間（クライアント側）を分けて見られる。

---

## 7. これまでの計測結果と結論（やらなくてよいこと）

| 試したこと | 結果 | 結論 |
|---|---|---|
| musl → 静的 glibc＋`tune_malloc()` | push 448→350 ms（明確）、write 196→181 ms、clone/read はぶれの範囲 | **採用済み** |
| 不要な応答ヘッダーの削除 | 計測できる差はなし（害もない） | 採用済み |
| ビジーポーリング（`FORGE_SPIN`） | 2 CPU 共有で read 934→5042 ms（スピン 2000 回）。300 回でも約 2 倍遅い | **使わない** |
| epoll サーバー（`FORGE_SERVER=epoll`） | 2 CPU では速く、1 CPU では遅い。一貫した差なし | 不採用（スレッド版のまま） |
| Go `net/http` の最小サーバー（固定 JSON）との比較 | forge のほうが 1 リクエストあたり 30〜40% 速い | サーバーの作りで上位に負けている形跡なし |
| 応答 JSON を小さくする | 差なし | やらない（互換性が下がるだけ） |
| git プロトコル v0 で clone の往復を 3→2 回に | ベンチは v2 を要求し、チェック `advertise-v2` が v2 を必須にしている | 不可 |
| ベンチのリポジトリ名を見分けて応答を変える | — | 不正なのでやらない |

### 時間の内訳（なぜこれ以上速くならないか）

- read の 1 リクエストは、gVisor のネットワーク処理・スレッド起床・計測側 Node の処理がほぼすべて。
  `/healthz` と Issue GET がほぼ同じ時間なので、forge の処理はほぼゼロ。
- 本番の往復の下限は約 700 µs（gVisor 内の下限 460 µs × 1.6）。そこから見積もる本番の下限は
  push 約 280 / clone 約 150 / read 約 1,400 / write 約 140 ms、幾何平均（perf_p50_us）約 300 ms。
  リーダーボード 1 位の 306 ms はほぼこの下限で、空いた時間帯の 1 回と考えられる。

---

## 8. まだ試していない候補（期待値は小さい）

- CPU が極端に足りないとき、clone の 50 並列でスレッドが一斉に起きる影響を抑える（同時処理数の上限など）。
  1 CPU の計測（`CPUS=0`）で clone が改善し、他が悪化しないことを確認してから採用すること。

---

## 9. 本番提出の前に

1. 0 章のチェックリストをすべて通す。
2. 新しいバイナリを入れた版は、**早めの提出で一度本番で動かし**、perf が以前（118〜148）と同等以上であることを確認する
   （本番のログは見えないので、Node 版に落ちていると perf が下がることで気づく）。
3. 最後の提出は確認済みの版にする。提出手順・トランスクリプトの扱いは `HANDOFF.md` 7 章。
   **このクラウドセッションでもコードを書いたので、その記録も bundle に含めること。**
