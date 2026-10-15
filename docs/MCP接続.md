# MCP 接続 — AI エージェントから起動中の TexoS を操作する（v1.11 MVP）

> 初版 2026-10-15。実装＝`src-tauri/src/mcp_bridge.rs`（TexoS 側の口）＋
> `scripts/texos_mcp.py`（MCP サーバー・stdio）。通しテスト＝`scripts/texos_mcp_selftest.py`。

これまでエージェントは「TexoS を終了して Python ビルダーで DB を組む」（規約 §3-8）か
「Computer Use で画面を操作する」しかなかった。MCP 接続は3つ目の道で、
**いま開いている画面と同じ状態**を、UI と同じ処理で取得・編集・実行・確認できる。

---

## 1. 仕組み

```
Claude Code 等 ──stdio(JSON-RPC/MCP)──▶ scripts/texos_mcp.py
                                          │ 1行1 JSON ＋トークン
                                          ▼
                    \\.\pipe\texos-mcp-<ユーザー名>（名前付きパイプ）
                                          │
                                          ▼
              TexoS 本体 mcp_bridge.rs ── notes/objects/filters の *_in(&Project)
                                       └─ run_flow_job（UI の ▶ と同じ実行系）
```

- **処理は複製していない。** 付箋・カード作成、矢印、移動、一覧、本文取得、実行は、
  UI のコマンドから本体を `*_in(&Project)` として切り出し、UI と MCP が同じ関数を呼ぶ
  （`create_note_in` `update_note_in` `move_note_in` `move_object_in` `create_filter_in`
  `update_filter_in` `create_op_card_in` `create_relation_in` `set_relation_attrs_in`
  `delete_relation_in` `list_*_in` `read_view_text_in` / 実行は `run_flow_for` を拡張した `run_flow_job`）。
- **SQLite へ外から書かない。** 書くのは起動中の TexoS 自身の接続だけ。
- **画面へ反映される。** 変更のたびに `mcp:changed` イベントを出し、フロントは同じ
  プロジェクトを開いていれば UNDO に積んで `refresh()` する。実行完了は既存の
  `job:done` でも読み直される。

### IPC の選択理由（名前付きパイプ＋stdio）

- **TCP を開かない。** ローカル HTTP は Web ページからの不正要求（CSRF・DNS リバインディング）対策が
  別途要るが、名前付きパイプはブラウザから届かず、`PIPE_REJECT_REMOTE_CLIENTS` で
  SMB 越しの接続も拒否する。LAN にもインターネットにも出ない。
- **なりすまし防止。** 最初のパイプは `FILE_FLAG_FIRST_PIPE_INSTANCE` で作る
  （他プロセスが先に同名の口を作っていたら TexoS 側が失敗する）。
- **認証。** 有効化のたびに作り直すトークンを毎要求に添える。トークンは
  `%APPDATA%\com.yoshi.texos\mcp.json`（本人だけが読める場所）に口の名前と一緒に置かれ、
  無効化で消える。
- **MCP サーバーは stdio・標準ライブラリだけ**（pip 不要）。標準出力は MCP 専用、ログは標準エラー
  （`--verbose` か `TEXOS_MCP_VERBOSE=1` のときだけ出す）。
- macOS / Linux では同じ形の Unix ドメインソケット（`app_data/mcp.sock`・0600）。**未検証**。

---

## 2. 起動・有効化・無効化・終了

- **有効化**: TexoS のキャンバス右クリック →「表示・設定 ...」→
  「AIエージェント接続（MCP）を受け付ける」にチェック。既定は**オフ**。
  設定は `config.json` の `mcp_enabled` に保存され、次回起動時も引き継ぐ。
- **無効化**: 同じ項目のチェックを外す。その場でパイプを閉じ、`mcp.json` を消す。
  既存の接続も次の要求から `mcp_disabled` で断る。実行中のフローには触らない。
- **MCP サーバーの起動・終了**: MCP クライアント（Claude Code）が自動で起動・終了する。
  TexoS が起動していなくても MCP サーバーは立ち上がり、ツールは `texos_unavailable` を返す
  （TexoS を起動して有効化すれば、次の呼び出しから繋がる）。
- TexoS を再起動・MCP を入れ直すとトークンが変わる。MCP サーバーは `unauthorized` を受けたら
  `mcp.json` を読み直して1回だけ再送するので、Claude Code 側の再起動は要らない。

### Claude Code の設定例

ユーザー全体に登録する（どのフォルダの Claude Code からも使える）:

```bash
claude mcp add texos --scope user -e PYTHONUTF8=1 -- python C:\Users\yoshi\dev\texos\scripts\texos_mcp.py
```

プロジェクト単位にするなら、そのフォルダの `.mcp.json`:

```json
{
  "mcpServers": {
    "texos": {
      "command": "python",
      "args": ["C:\\Users\\yoshi\\dev\\texos\\scripts\\texos_mcp.py"],
      "env": { "PYTHONUTF8": "1" }
    }
  }
}
```

動作確認は Claude Code で `/mcp` → texos が connected、ツール `status` を呼ぶ。

---

## 3. ツール一覧

共通の約束:

- **変更・実行のツールは `project` 必須**（`get_project` が返す `project.path`）。
  TexoS で開いているプロジェクトと違えば `project_mismatch`（`details.current` に現在の値）。
  照合と処理は TexoS 内の**同じロックの中**で行うので、呼び出しの途中でユーザーが
  プロジェクトを切り替えても、別のプロジェクトへ書くことはない。実行は終端カードを読む
  短ロックの中で照合し、以後はジョブ専用の DB 接続で起動元プロジェクトへ書く（既存の契約）。
- 読み取りのツールは `project` 省略可（現在のプロジェクト）。応答には必ず `project` が入る。
- 応答は JSON（`structuredContent` と、同じ内容の text）。失敗は `isError:true` と
  `{"error":{"code","message","details?"}}`。
- **`request_id`（冪等キー）**: `create_note` `create_card` `connect` `import_file` `run_card` で使える。
  同じキーの再送は作り直さず、1回目の結果を `replayed:true` で返す（TexoS の再起動で消える・直近1000件）。
  `connect` はキーが無くても、同じ矢印が既にあればそれを返す（`already_existed`）。

### 状態取得（読み取り専用）

- `status` — TexoS のバージョン・MCP 状態・現在のプロジェクト。
- `get_project {project?}` — `project{path,name}`・カード/付箋/矢印の件数・このプロジェクトの実行中ジョブ。
- `list_cards {project?, kinds?, limit?=100(≤500), offset?}` — `cards[]`:
  `id, kind, type, label, x, y, mcp_editable, last_run?, path?, mime?, text_preview?(200字)`。
  `type` は `note` / `ai` / `ai_chat` / `builtin:<名前>` / `program` / `shell` / `op:<in|out|trigger…>` / `file` 等。
  `total` と `next_offset` でページング。**設定の全体と本文は含まない。**
- `get_card {project?, card_id}` — 上に加えて `settings`（filter_ref の JSON。トークン・キー・パスワードらしい
  キーは `***`、8000字を超える文字列は切って `settings_truncated_fields` に記録）、`memo`、
  `inputs[]` / `outputs[]`（矢印 `id, from_id, to_id, kind, attrs`）。
- `list_relations {project?, card_id?, limit?, offset?}` — 矢印の一覧。
- `read_card_text {project?, card_id, max_chars?=20000(≤200000), offset?}` — 付箋の本文か、
  テキスト系ファイルカードの中身。`total_chars / truncated / next_offset` で続きを読む。
  バイナリは `not_text`、32MB 超は `too_large`。
- `list_ai_models {provider?=ollama|lmstudio|openai, endpoint?}` — 実機のモデル名一覧。

### 編集

- `create_note {project, text, x, y, request_id?}` → `card_id`。UNDO 可。
- `update_note {project, card_id, text}` — UNDO 対象外（UI の本文編集と同じ）。
- `create_card {project, type, x, y, label?, name?, settings?, request_id?}` → `card_id, type, settings`。UNDO 可。
  - `type:"ai"` — AI 変換カード（`builtin:ai`・`mode:transform`・`accept_mime:text/*`・`output_ext:md`）。
    `settings.model` 必須。使えるキー: `model provider endpoint system temperature prefix seq think num_ctx gpu send_reasoning note`。
  - `type:"builtin", name:` — `text_concat text_diff text_paste text_grep text_replace text_head text_tail
    text_sort text_count text_cut text_convert md_codeblock tap`。使えるキー:
    `pattern replacement lines order compare unique delimiter fields encoding separator lang prefix seq output_ext accept_mime note`。
  - `type:"out"`（`settings.prefix` のみ）/ `type:"in"`。
  - 外部プログラム・シェル・ランチャー・画像生成・Web 系・トリガ/タイマ等は**作れない**（`unsupported_card_type`）。
- `update_card {project, card_id, settings}` — 上の種別だけ。キーをマージ、`null` で削除。
  許可外のキーは `unsupported_setting`（`details.allowed` に一覧）。種別そのものは変えられない。UNDO 対象外。
- `move_card {project, card_id, x, y}` — カード・付箋とも。UNDO 可（元の位置へ）。

### 配線

- `connect {project, from_id, to_id, role?, when?, carry?, request_id?}` → `relation_id, kind, already_existed`。
  kind は UI と同じ規則で自動（処理カードが絡めば `flow`、トリガ等なら `req`、それ以外 `link`）。UNDO 可。
  `role:"system"` は AI カード宛、`"answer"` は付箋→問い合わせカード、`"previous"` は反復カード発だけ
  （違えば `invalid_params`）。
- `update_relation {project, relation_id, role?, when?, carry?, port?}` — 空文字で外す。UNDO 対象外。
- `disconnect {project, relation_id}` — ソフト削除。UNDO 可（復活）。

### 実行・実行管理・結果

- `run_card {project, card_id, request_id?}` → すぐに `job_id`。終端は処理カードか OUT カード
  （付箋・ファイル・対話モードの AI は `not_runnable`）。同じ終端が実行中なら `busy`。
  **上流に外部プログラム・シェル・Web 取得などがあれば、それも実行される（副作用）。**
- `get_job {job_id, include_nodes?=true, wait_seconds?(≤60)}` — `status`:
  - `running` / `cancelling`（`progress.note/percent`・`elapsed_ms`・途中までの `nodes`）
  - `succeeded` — エラーなし
  - `completed_with_errors` — 終端は完走したが、どこかの段が失敗（**出力が欠けている可能性**）
  - `nodata` — 材料が無く何も作らなかった
  - `failed` — 実行そのものが失敗（`error`）
  - `cancelled` — 中断された
  - 完了時: `output_cards[]{card_id,label,path,mime,created}`（`created:false` は固定名で上書き更新）、
    `output_files`, `errors[]`, `skipped[]`, `nodes[]{card_id,label,kind,status(ok/error/nodata/reused),ms,errors,outputs}`。
  - MCP 以外（UI・トリガ）から起動した実行中ジョブも見える（完了後の結果は MCP 起動分だけ保持・直近200件）。
- `cancel_job {job_id}` — UI の中断ボタンと同じ `break_flow`。
- `list_jobs` — 実行中のジョブ全部＋MCP から起動した直近20件。
- `import_file {project, path(絶対), x, y, label?, request_id?}` — 既存ファイルをカードに（UI のドロップと同じ
  `import_file_into`。プロジェクト内＝実身・外＝窓）。フォルダは不可。UNDO 可。

### エラーコード

`invalid_params` `unauthorized` `mcp_disabled` `no_project` `project_mismatch` `not_found`
`unsupported_card_type` `unsupported_setting` `invalid_spec` `busy` `not_runnable` `not_text`
`not_readable` `too_large` `ai_unreachable` `import_failed` `run_failed` `unknown_job`
`unknown_method` `request_too_large` `invalid_json` `internal`、MCP サーバー側で `texos_unavailable`。

---

## 4. エージェント向け 短い操作ガイド

1. `get_project` → `project.path` を控える。以後の変更・実行には毎回渡す。
2. 既存の面を読むなら `list_cards`（必要なら `kinds`）→ 気になるカードだけ `get_card`。
3. AI を使うなら `list_ai_models` で実機のモデルを確かめて `settings.model` に入れる（決め打ちしない）。
4. 作る: 入力は付箋（`create_note`）、処理は `create_card`、配線は `connect`。
   - **AI 変換カードは通常入力ごとに別々に走る。** 複数資料を1回で読ませるなら `text_concat` を挟む
     （束への入力順＝`connect` した順）。
   - 役割は付箋を `role:"system"` で AI カードへ（入力に数えない＝ファンアウトしない）。
   - 終端の出力名は終端カードの `prefix` が決める。
5. `run_card` → `get_job`（`wait_seconds` で待てる）。
6. **完了＝成功ではない。** `status` が `succeeded` か、`errors` と `nodes[].status` を確認し、
   出力は `read_card_text` で中身を読んで期待どおりか確かめる（規約 §3-14）。
7. 直すときは `update_note` / `update_card` してもう一度 `run_card`。同じ `prefix` なら同じカードが更新される。
8. 同じ操作を再送する可能性があるなら `request_id` を付ける。

設計の詳しい約束（落とし穴）は `docs/AIエージェント向け_フロー構築規約.md`。

---

## 5. 検証結果（2026-10-15）

### 自動テスト

- `cargo test --lib` — 196 件成功（うち MCP ブリッジの単体テスト5件: ルート照合・秘密の伏せ字・
  設定キーの許可/削除・種別判定・AI 設定の検証）。既存テストも全件成功＝切り出しで UI 側の挙動は変わっていない。
- `npx tsc --noEmit` — 成功。
- MCP サーバー単体（TexoS 無し）: `initialize` / `tools/list`（20ツール）/ 未起動時の `texos_unavailable` /
  未知メソッドの -32601 を確認。

### 実機（デバッグビルド・専用プロジェクト `F:\yoshi\Documents\ChatGPT\TexoS_MCP検証`）

`python scripts/texos_mcp_selftest.py --project <検証用>` を Claude Code と同じ経路（stdio）で実行し
**28/28 成功**。モデルは `list_ai_models` の7件から自動選択（gpt-oss-safeguard:20b）。

1. 接続・現在のプロジェクト取得 — 成功
2. 付箋・AI 変換カード作成、flow と role=system の接続、重複矢印を作らない、移動 — 成功
3. 再起動・開き直しなしで画面に出る — 目視（スクリーンショット）で確認。失敗・中断した AI カードは赤枠で表示
4. モデルを実機から選択 — 成功
5. 実行 → ジョブ ID → `succeeded` — 成功
6. 出力本文「APPLE」を `read_card_text` で取得 — 成功
7. 付箋を「ねこ」に更新して再実行 → 同じ出力カードが更新され「CAT」— 成功
8. 異常系 — プロジェクト不一致（編集・実行とも拒否）、project 無し、不正 ID、不正な設定キー、
   model 無し、対応外カード、付箋の実行、不明ジョブ、存在しないモデル（`completed_with_errors` と理由）、
   同じ終端の多重起動（`busy`）、キャンセル（`cancelled`・出力カードなし）、`request_id` 再送（作成・実行とも重複なし）— 成功
9. UI から MCP を無効化 → `mcp.json` 削除・パイプも閉じる・`texos_unavailable`。その状態で UI のカード操作は通常どおり — 確認
- UNDO: エージェントの作成・配線が Ctrl+Z で戻ることを確認（下の注意も参照）。

### 未確認・注意

- **AI 変換カードの中断**（解決済み）: `cancel_job` は UI の中断ボタンと同じ処理。当初 AI 変換
  （`flow.rs: run_ai_rec`）は一括 HTTP 呼び出しで中断フラグを見ず、LLM の応答が返るまで止まらなかった
  （実測：中断要求から 53 秒で `cancelled`）。受信をストリームにして行ごとに中断を見るよう修正
  （対話と共通の `stream_ai_chat`）。ライブ Ollama で中断要求から 0.1 秒未満で戻ることを確認
  （`cargo test --lib -- --ignored ai_live_transform_cancel`）。残る隙間は「最初の1トークンが出るまで」
  （長い入力の読み込み中）で、その間は最初の行が届いた時点で止まる。
  外部プログラムは既存の監視ループがツリーごと止める（今回 MCP からは外部プログラムカードを作れないため実機未確認）。
- **AI エンドポイントは同時1つ・超過は即拒否**（既存の資源セマフォ）。AI を使う実行が走っている間に
  別の AI 実行を始めると、その段は「使用中です」で `completed_with_errors` になる。実測で確認。
- **UNDO のまとめ**: 既存の UNDO は「150ms 以内の記録を1操作」とみなすため、エージェントが立て続けに
  行った作成＋配線は Ctrl+Z 1回でまとめて戻る。
- 本文・設定・矢印属性の変更は UNDO 対象外（UI でも対象外）。
- 付箋を UI で編集中に MCP が同じ付箋を書き換えた場合の競合は未対策（後勝ち）。
- macOS / Linux（Unix ソケット）は未検証。リリースビルド・インストーラ経由の起動は未検証（デバッグビルドで確認）。
- Claude Code の `/mcp` からの接続は、同じ stdio 経路をテストスクリプトで代行した。Claude Code に登録しての対話操作は未実施。

---

## 6. MVP で対応しないもの・次の拡張候補

- 対応しない: カードの完全削除（ソフト削除も未提供）、バイナリ本文の取得、プロジェクトの作成・切り替え、
  外部プログラム／シェル／画像生成等の作成・設定変更、任意 SQL・任意コマンド、ラベル変更、図形。
- 次の候補:
  - AI 変換の中断を即時にする（ストリーミング化して中断フラグを見る。対話モードは既に対応済み）。
  - カードのソフト削除（UI の削除＝復活の呪文と同じ）とラベル変更。
  - モジュール JSON の読み込み（規約 §7 の本筋）を MCP から呼べるようにする。
  - 進捗の購読（MCP の notifications/progress で `job:progress` を流す）。
  - 「別プロジェクト＋親に入口」（規約 §5-2）を MCP から行う `create_subproject`。

---

## 7. 調査で分かった既知の UI 問題（未修正・記録のみ）

- **付箋→処理カードの矢印が赤く出る**: `App.tsx` の `invalidFlowIds` が矢印の出所を `objects`
  （`list_objects`）だけで引いている。v1.4.17 で付箋は `list_notes` 側へ分かれたため、付箋発の矢印は
  常に「フロー入力にできないノード」扱いで赤になる（コメントも v1.4.17 以前の前提のまま）。
  実行系（`flow_run.rs`）は付箋を text/plain の定数として正しく流すので、実行は成功する。
  直すなら、出所が付箋なら `mimeMatches(accept, "text/plain")` で判定する。
- ズーム後の座標ずれ・モデル読込後にダイアログが伸びる件は、今回は原因を調べていない。
