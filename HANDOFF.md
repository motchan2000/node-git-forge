# 引き継ぎ書：github-core-clone-01（make.fun）— 1000 点を目指す設計セッション向け

作成: 2026-11-20 14:20 JST（ローカルの Claude Code セッションより）
対象リポジトリ: https://github.com/motchan2000/node-git-forge （main）

---

## 0. まず読むこと（締切・ルールの要点）

| 項目 | 内容 |
|---|---|
| 締切 | **2026-11-21 06:30 UTC（日本時間 11/21 土 15:30）** |
| 採点に使われる提出 | **最後の提出**（リーダーボードは `Force_Last`）。最後に出すものを必ず最良版にすること |
| 提出回数 | 1 日（UTC）5 回、コンペ全体 10 回。**使用済み 5 回**（11/20 UTC 分は使い切り）→ 残り 5 回（11/21 UTC 00:00 = 日本時間 9:00 以降） |
| 本番環境 | gVisor サンドボックス。entry コンテナ 2 CPU / 1 GB / 256 プロセス、env（計測スイート）2 CPU / 256 MiB。混雑した共有ノード |
| 起動 | `node /bot/main.ts`（Node 22）。`/bot` は読み取り専用、書き込みは `/tmp` のみ |
| 配点 | feature 600（120 チェック）＋ agent 200（MCP 15 チェック）＋ perf 200（4 ワークロード × 50） |
| perf の式 | 各ワークロード `clamp(25 − 12.5·log2(t/T_ref), 0, 50)`、t は本番での 3 ラウンド中央値 |
| T_ref | push 1640.82 ms / clone 266.941 ms / read 2759.715 ms / write 348.965 ms |
| 本選 | 締切後に 3 インスタンスで採点、平均点で順位。上位 60 は各試行がリプレイされ、state.json がバイト一致しないと失格 |

**機能面（feature 600 + agent 200）はすでに満点**。残る差はすべて perf。

---

## 1. 現状

### 本番スコアの履歴（practice trial 0）

| # | コミット | 内容 | 合計 | perf | perf_p50_us |
|---|---|---|---|---|---|
| 1 | `e2f37e9` | Node（node:http＋キャッシュ） | 937 | 137 | 486,161 |
| 2 | `fe53ee1` | Node（node:net 自作 HTTP） | 934 | 134 | 509,045 |
| 3 | `1a85f02` | **Rust ネイティブ（std のみ）** | **948** | **148** | **416,004** |
| 4 | `41ebb60` | Rust＋ビジーポーリング | 924 | 124 | 582,950 |
| 5 | `1a85f02` | #3 の**まったく同じコード**を再提出 | 918 | 118 | 626,311 |

- **最重要の発見: 本番の計測は時間帯によって約 50% ぶれる**（同一コードで 416 ms → 626 ms）。#4 の 924 もビジーポーリングのせいとは断定できない。1 回の提出結果で改善の良し悪しを判断できない。
- 提出は 11/20 の 03:07〜05:20 UTC。後半ほど遅く、混雑（同時に多数のエージェントが採点中）の影響と考えられる。
- 現在のリーダーボード表示は最後の提出 #5 の 918。
- 1 位は 970（perf 170、perf_p50_us ≈ 306,000）。上位 3 つは Rust / Go の静的バイナリ。上位の値も「空いている時間帯に出た幸運な 1 回」の可能性がある。
- リーダーボード API: `GET https://make.fun/api/competitions/github-core-clone-01/leaderboard`（`entries[].values`）

### 2026-11-20 クラウドセッション（session_01R2SyqcRXZ6z6Vxb6zdjCSV）での変更

- **静的 glibc ビルド＋ malloc 調整**（3 章参照）。musl の malloc がシステムコールを大量に出していた（clone で futex 約 40,000 回、
  write/push で 1 操作あたり mmap/munmap 約 5 回）。変更後は全ワークロードで定常状態 1 リクエスト＝recvfrom 1＋sendto 1。
  CPU を燃やさない純粋なシステムコール削減なので、混雑した本番でも逆効果になりにくい。
- 不要な応答ヘッダーを削除（git 応答の cache-control/expires/pragma、REST の x-github-media-type。どのチェックも見ていない）。
- 機能 120/120、grade-local（Docker/runc）984、リプレイはバイト一致を確認済み。
- gVisor 内（`tests/perf/gv.sh`、2 CPU に固定）で旧版と交互に計測し、4 ワークロードとも改善傾向（結果は 4 章末尾）。
- **このクラウドセッションでもコードを書いたので、提出時の bundle にこのセッションの記録も含めること**（7 章参照）。

### リポジトリの HEAD（以前の記述）

- 既定は **#3 と同じ設計**（スレッド/接続、ビジーポーリング OFF）＋本番では効果未確認の無害な変更（事前起動ワーカー、読み取り共有ロック）。
- 実験用に残しているもの（既定 OFF、環境変数で有効化）:
  - `FORGE_SERVER=epoll` … epoll イベントループ（`rust/src/event.rs`）
  - `FORGE_SPIN=<回数>` / `FORGE_SPINNERS=<上限>` … スレッド版のビジーポーリング
  - `FORGE_EPOLL_SPIN=<回数>` … epoll 版のビジーポーリング
- **本番実績のある最良は `1a85f02`**。HEAD を提出する前に、同等以上であることを確認すること。

---

## 2. アーキテクチャ

```text
main.ts          env ライフサイクル（register → done 待ち → exit 0）。例外は握りつぶして生存
native.ts        bin/forge-linux-amd64 を起動 → "FORGE_PORT n" を待つ → /healthz 確認 → そのポートを登録
                 失敗時は Node 版（app.ts）にフォールバック。バイナリが落ちたら同じポートで再起動、
                 それも無理なら Node 版が同ポートを引き継ぐ
bin/forge-linux-amd64   Rust 版の静的 musl バイナリ（git 上で mode 100755 必須）
rust/src/        Rust 版（標準ライブラリのみ、外部クレートなし、約 4,800 行）
  main.rs        認証・ルーティング、RwLock<Forge>、Issue GET の共有ロック高速路
  http.rs        スレッド/接続サーバー（事前起動 64、上限 180、任意でビジーポーリング）
  event.rs       epoll イベントループ（実験用）
  json.rs util.rs(SHA-1/base64/日時) zlib.rs(inflate 完全実装、deflate は stored)
  objects.rs     オブジェクトストア、tree/commit、平坦化・3-way マージ
  pack.rs        PACK 読み書き（OFS/REF デルタ）、pkt-line、side-band
  smart.rs       info/refs、upload-pack v2（ls-refs/fetch/shallow/deepen）＋v0、receive-pack
  store.rs       Forge（repo/ref/contents コミット/issue/label/comment/PR/squash merge）、決定的な時計
  present.rs rest.rs gql_parse.rs graphql.rs mcp.rs
app.ts, src/     Node 版（同じ挙動。フォールバック用）
tests/serve.ts   Node 版をポート指定で起動（`node tests/serve.ts 3000 0.0.0.0`）
tests/perf/gvisor-perf.sh   gVisor 内で forge＋スイートを動かす計測スクリプト
```

決定性: 時計は `MAKEFUN_TRIAL_NOW`＋イベントごとに 1 秒、乱数なし。リプレイは毎回バイト一致を確認済み。
認証: `Authorization: Bearer $MAKEFUN_SHOP_TOKEN`。トークンのログインはスイートのシード由来の `adaiversen`（`FORGE_LOGIN` で変更可）。

---

## 3. ビルドとテスト

### 必要なもの

- **公式キット**（`github-core-clone-01-kit.tar.gz`）。リポジトリには入っていない（make.fun 側の配布物なので入れないこと）。ユーザーから受け取るか、make.fun の案内（https://make.fun/skill.md）に従って取得する。
- Node 22、Docker（Rust のビルド用）。

### Rust バイナリのビルド（静的 glibc、2026-11-20 クラウドセッションで musl から変更）

```sh
docker run --rm -v "$PWD/rust:/src" -v "$PWD/bin:/out" -w /src \
  -e RUSTFLAGS="-C target-feature=+crt-static" rust:1 \
  sh -c 'cargo build --release --target x86_64-unknown-linux-gnu && cp target/x86_64-unknown-linux-gnu/release/forge /out/forge-linux-amd64'
git update-index --chmod=+x bin/forge-linux-amd64   # Windows で作業した場合は必須
```

**musl をやめた理由**: musl の malloc はグローバルロック 1 本＋こまめな mmap/munmap。strace で数えると
clone ワークロード 1 回で futex 約 40,000 回（50 並列のスレッドが malloc ロックで競合）、
write/push では 1 操作あたり mmap/munmap 約 5 回。gVisor ではどちらも高価なシステムコール。
静的 glibc（スレッドごとの arena）＋ `main.rs` の `tune_malloc()`（mallopt でトリム無効・mmap 閾値 64 MB・
TOP_PAD 16 MB）にすると、定常状態のシステムコールは 4 ワークロードとも **recvfrom 1 回＋sendto 1 回/リクエストだけ**
（clone の futex は約 180 回）。

### 機能テスト（120 チェック）

```sh
MAKEFUN_SHOP_TOKEN=tk ./bin/forge-linux-amd64 3000 &      # "FORGE_PORT 3000" が出る
node --experimental-strip-types <kit>/sdk/run.ts --url http://127.0.0.1:3000 --token tk          # 120/120 を確認
node --experimental-strip-types <kit>/sdk/run.ts --url http://127.0.0.1:3000 --token tk --perf   # 性能も
```

### 公式採点（本番と同じ流れ）

```sh
node <kit>/cli/makefun.mjs grade-local --kit <kit> --bot <repo> --driver docker   # HEAD を採点、リプレイ一致も確認
```

- Windows では Docker ドライバがホストパス（`C:\...`）を拒否して void になる。Linux（または Linux コンテナ内）から実行すること。
- 直近の結果: HEAD 相当で 994〜995（ローカル Docker。本番より大幅に速い）。

### gVisor での性能計測（本番に一番近い）

`tests/perf/gvisor-perf.sh` の冒頭コメントを参照。root（特権コンテナ）＋ runsc が必要。

| gVisor 内の中央値（ms） | push | clone | read | write | perf_p50 |
|---|---|---|---|---|---|
| Rust #3（本番 948） | 298 | 116 | 1019 | 137 | 263 ms |
| Rust HEAD 既定 | 301 | 107 | 1027 | 129 | 255 ms |
| Rust＋ビジーポーリング（本番 924） | 244 | 99 | 739 | 103 | 207 ms |
| Rust＋epoll（スピンあり） | 269 | 99 | 741 | 115 | 218 ms |
| Node | 367 | 114 | 1220 | 147 | 294 ms |

**較正の注意**: 本番はこの環境の 1.6〜2.4 倍の時間（本番自体が時間帯で約 50% ぶれるため幅がある）。ビジーポーリング版は本番で 924 だったが、同じ時間帯に #3 のコードも 918 だったので、**ビジーポーリングが本番で悪いとは断定できない**。ただし本番は CPU が混雑した共有ノードなので、CPU を燃やす最適化のリスクはこの環境では評価できない。

---

## 4. 分かっていること（計測の結論）

1. **forge の処理時間はほぼゼロ**。gVisor 内で `/healthz` と Issue GET の 2,000 連続リクエストがほぼ同時間（約 460 µs/往復）。forge の JS/Rust 処理は 15〜20 µs/リクエスト。
2. **1 リクエストのシステムコールは recvfrom 1 回＋sendto 1 回**（gVisor の strace で確認）。これ以上は減らせない。
3. 往復時間の内訳は「gVisor のネットワークスタック＋計測側（Node の undici）＋スレッド起床」。ビジーポーリングで起床を消すとローカルでは往復が約 1/3 短縮された。本番での効果は、ぶれが大きく未確定。
4. 本番の内訳推定（#3 の 948 の回を基準に、gVisor 値 × 1.58）: push 471 / clone 183 / read 1610 / write 216 ms → 点数 47 / 32 / 35 / 34 = 148。本番の perf 148 と一致する。混雑時（#5）は全体がさらに約 1.5 倍。
5. **本番のぶれ（±25% 程度）が、コード改善の効果（数%〜20%）より大きい**。改善の評価はローカル gVisor で行い、本番提出は「最終版の確認」と割り切るのが現実的。

---

## 5. 1000 点に必要なこと（現実的な評価）

1000 点 = perf 200 = 4 ワークロードすべてが**本番で T_ref の 1/4 以下**:

| ワークロード | 必要（本番） | 推定現状（本番） | 必要な短縮 |
|---|---|---|---|
| push | ≤ 410 ms | 約 471 | 1.15 倍 |
| clone | ≤ 67 ms | 約 183 | 2.7 倍 |
| read | ≤ 690 ms（345 µs/往復） | 約 1610 | 2.3 倍 |
| write | ≤ 87 ms（435 µs/件） | 約 216 | 2.5 倍 |

- read は 2,000 往復の逐次処理。本番の往復の下限は、forge の処理をゼロにしても約 700〜1,000 µs と見積もられる（gVisor 内の床 460 µs × 1.6、うちサーバー起床分を除いても 620 µs × 1.6）。**read を 690 ms 以下にするのは、forge 側の工夫だけではほぼ不可能**。
- したがって **1000 点は現実的ではなく、狙える上限は 975〜980 程度**。1 位（970）を超えることを当面の目標にするのが妥当。
- 970 との差は perf で約 22 点。clone・read・write をそれぞれ 1.2〜1.4 倍縮めれば届く計算。

---

## 6. 改善案（優先度順、CPU を燃やさないもの）

1. **応答サイズの削減**（read / write に効く可能性）: 本番は計測側コンテナも CPU が混雑しているので、クライアントの JSON パースや受信コストが効いている可能性がある。Issue の JSON は約 920 バイトで、そのうち約 700 バイトが URL などの付帯情報。チェックに必要なフィールド（`checks-rest.ts` / `checks-mcp.ts` の shape）を残して削る。ローカルでは差が出ないので、本番で 1 回計測する価値あり。
2. **epoll（スピンなし）**: スレッド数と起床回数が減る。clone（50 並列）で効く可能性。CPU 消費は増えない。`FORGE_SERVER=epoll` の既定化をビルドで切り替えて本番で試す。
3. **控えめなスピン**: たとえば 50〜100 回程度だけ `MSG_DONTWAIT` で確認してからブロックする。2,000 回は本番で逆効果だった。効果が出るかは本番でしか分からない。
4. **git の広告・応答を事前生成して共有ロックで返す**（push と clone の info/refs）。
5. **TCP オプション**（`TCP_QUICKACK` など）: POST でヘッダーと本文が別セグメントで届く場合の遅延対策。gVisor が対応しているかは要確認。
6. **ぶれとの付き合い方**: 同一コードで 948 と 918 が出た。本選は締切後に 3 インスタンスで採点され、その平均で決まる（その時点の混雑次第で、全員が同じように影響を受けると考えられる）。dev 段階の 1 回の点数に一喜一憂しないこと。
7. **ビジーポーリングの再評価**: ローカル gVisor では 1.26 倍速かった。本番の 924 は混雑時間帯の値なので、空いている時間帯に再計測する価値はある。ただし CPU を燃やすので、混雑ノードで不利になるリスクも残る。上限を小さく（例: 200 回）すれば、リスクと効果の中間を取れる。

**2026-11-20 クラウドセッションの gVisor 交互計測**（`tests/perf/gv.sh`、2 CPU 固定、各 4 回の中央値 ms。この VM はぶれが大きい）:

| | push | clone | read | write |
|---|---|---|---|---|
| 旧（musl、`9d8e656`） | 448（375–503） | 137（114–184） | 1188（956–1192） | 196（169–232） |
| 新（glibc＋malloc 調整） | **350（339–353）** | 148（134–158） | 1200（874–1267） | 181（114–198） |

push は明確に改善（ばらつきも小さい）、write はやや改善、clone と read はぶれの範囲内で差なし。
`grade-local` を gVisor（Docker の既定ランタイムを runsc）で動かす試みは、コンテナ間ネットワークが通らず止まった。

**ビジーポーリングの結論（2026-11-20 クラウドセッション）: 使わないこと。** CPU が足りない条件で計測すると大幅に悪化する
（gVisor 内で forge と計測側が CPU を共有、中央値 ms、各 3 回）:

| 条件 | push | clone | read | write |
|---|---|---|---|---|
| 2 CPU・スピンなし | 340 | 134 | 934 | 157 |
| 2 CPU・FORGE_SPIN=300 | 636 | 156 | 1935 | 270 |
| 2 CPU・FORGE_SPIN=2000 | 1452 | 260 | **5042** | 644 |
| 1 CPU・スピンなし | 464 | 162 | 1523 | 189 |
| 1 CPU・FORGE_SPIN=2000 | 522 | 204 | 1810 | 207 |

ハンドオフ時点の「ローカル gVisor で 1.26 倍速い」は CPU に余裕がある条件での値。本番は混雑ノードなので、
#4（スピンあり）の 924 は実際に逆効果だった可能性が高い。

**epoll（スピンなし）も不採用**: 2 CPU では read 721 vs 1267（threads）と良く見えたが、1 CPU では read 1711 vs 1241 と逆転。
ぶれの範囲を超える一貫した差がないため、本番実績のあるスレッド版を既定のままにした。

**Go との比較（上位 3 つは Rust/Go）**: 固定 JSON を返すだけの Go `net/http` サーバーと、forge を同じ gVisor 内で交互に計測
（`tests/perf/gab.sh`、2 CPU）。1 リクエストあたり forge は GET 610〜686 µs / POST 875〜915 µs、
Go は GET 877〜976 µs / POST 1226〜1273 µs。**forge のほうが 30〜40% 速い**（Go は netpoller と M の受け渡しで
システムコールが多い）。サーバーの作りで上位に負けている形跡はない。

**応答サイズは無関係**: Go の固定サーバーで Issue JSON 約 700 バイトと最小 JSON を比べても差はぶれの範囲内。
フィールドを削っても速くならない。

**git プロトコル v0 で clone の往復を 3→2 回に減らす案は不可**: ベンチの clone は `Git-Protocol: version=2` を送り、
チェック `advertise-v2` が v2 応答を必須にしている。

試すときの原則: ローカル gVisor（`tests/perf/gvisor-perf.sh`）で確実に速いものだけを採用する。本番の 1 回の点数では判断しない。**最後の提出は、ローカル gVisor で最速かつ 120/120 の版にする**。

---

## 7. 提出手順（重要な落とし穴つき）

提出は**ユーザーの Windows PC のローカル**から行ってきた（make.fun の認証情報、鍵ペア `~/.makefun/wallet.json`、セッション記録がそこにある）。

```sh
# 1) トランスクリプトの bundle（kit ディレクトリで）
node cli/makefun.mjs bundle --repo <repo> --out transcripts.tar.gz \
  --prompt-file prompt.txt --prompt-origin other \
  --prompt-via "typed by the person in the Claude desktop app's Code tab; the app prepends its own system-reminder text block to that first message, so the typed words are the message's second text block" \
  --harness claude-code --harness-version 2.1.326 --provider anthropic --model claude-opus-5-5 --effort medium \
  [--session <クラウドセッションの .jsonl>]
# 2) 提出前チェック（16 項目すべて pass が条件）
node cli/makefun.mjs check --kit . --repo <repo> --transcripts transcripts.tar.gz
# 3) 提出（GitHub リポジトリ方式。報酬はエスクローなしなので署名不要）
node cli/makefun.mjs submit --competition github-core-clone-01 --repo-url https://github.com/motchan2000/node-git-forge \
  --commit <sha> --title "<公開タイトル>" --bundle transcripts.tar.gz
```

- `prompt.txt` の中身は、ユーザーの最初のメッセージそのもの（ファイル添付 2 つ）:
  `@"C:\Users\Admin\Downloads\github_forge_design_v2.md" @"C:\Users\Admin\Downloads\github-core-clone-01-kit.tar.gz"`（末尾に改行）。
- **トランスクリプト**: コードを書いたセッションはすべて提出が必要。元のローカルセッション（`9b756aad-4d73-4256-8226-b50a84aa4e38`）は削除しないこと。**クラウドセッションでコードを書いた場合、その記録も bundle に含める必要がある**。`bundle` には Claude Code のクラウド用フラグがないため、teleport などでセッション記録（.jsonl）をローカルに持ってきて `--session` で渡す。早めに `bundle --dry-run` で拾えるか確認すること。
- make.fun が混雑すると HTTP 503 で提出が失敗することがある。失敗したら `node cli/makefun.mjs status` でエントリー一覧を確認してから再提出する（二重提出を避けるため）。
- 提出後の採点は 10〜15 分。スコアはリーダーボード API で確認する（`status` にはスコアが出ない）。
- README のバッジ 3 つと GitHub の About 設定（website = バウンティページ、topic `makefun`）は提出条件。消さないこと。
- LICENSE の先頭の `SPDX-License-Identifier: MIT` も必須。

---

## 8. 未完了・既知の課題

- 現在のリーダーボード上の点数は #5 の 918（最後の提出が採用されるため）。締切までに最終版を必ず提出すること。
- `FORGE_SERVER=epoll` は機能テスト（120/120）を通過しているが、本番では未計測。
- Rust 版の zlib deflate は stored ブロックのみ（圧縮なし）。clone の PACK は小さいので影響は小さいと見ている。
- Node 版（フォールバック）はビジーポーリングを持たない。Rust 版が起動できる限り使われない。
