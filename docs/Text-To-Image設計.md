# Text-To-Image フィルタ設計（v1.5・Phase 1）

画像生成 AI を TexoS ネイティブのフィルタ（`builtin:"t2i"`）として組み込む。AI チャットフィルタ
（`builtin:"ai"`）と同系統＝「パイプラインのデータ変換子」。外部 Python プロジェクトを都度
シェル起動する旧 BlockProg 案は破棄し、**Ollama 相当の常駐デーモン**を自前（diffusers）で建てる。

## 全体像

```
[テキストカード（プロンプト）] --flow--> [t2i フィルタ] --> [PNG 実身（プロジェクト直下）]
```

- 入力 = テキスト（接続カードのプロンプト）。出力 = PNG を実身としてプロジェクトに生成・表示。
- 実行は AI と同じく既存フロー機構に乗る（`run_flow` → `eval_filter` → `execute_node`）。

## アーキテクチャ（Ollama 流の常駐デーモン）

- **`python/t2i_server.py`** … diffusers の小さな HTTP デーモン（標準 `http.server` のみ・追加依存なし）。
  モデルを VRAM に常駐させ、反復生成のロード待ち（初回 ~15s）を避ける＝Ollama の keep_alive と同趣旨。
  - `GET /health` `GET /models` `POST /load` `POST /pull`(NDJSON 進捗) `POST /generate`(→PNG)。
  - モデルは共有 app-data `%APPDATA%\com.yoshi.texos\t2i\models\<name>` に保存（HF キャッシュも同配下）。
  - `allow_patterns` で diffusers の fp16 variant safetensors だけ取得（SD1.5 で ~33GB→~2GB に削減）。
- **TexoS（Rust）がデーモンを起動・管理**。`t2i_server.py` はビルドに `include_str!` で埋め込み、
  起動時に app-data へ書き出す（リリースでも resources 設定が要らない）。子プロセスは
  `AppState.t2i_server` に保持して停止できる。

## バックエンド（Rust）

- `flow.rs`
  - `FilterSpec` に t2i 用フィールド追加（`negative` `steps` `guidance` `height` `seed`。`model` は
    AI と共用＝SD モデルの friendly name、`width` は img_resize と共用）。serde 後方互換。
  - `NodeKind::T2i`。`node_kind()` で `builtin=="t2i"` を割り当て。
  - `run_t2i()` … 入力テキストをプロンプトとしてデーモン `/generate` に POST し PNG を得る。
    AI と同じく reqwest::blocking を tokio 文脈外スレッドで実行（内部ランタイム drop パニック回避）。
  - `T2I_PORT = 8765`。
- `lib.rs`
  - `execute_node` の `NodeKind::T2i` 分岐（`inputs[0]` をプロンプトに）。
  - デーモン管理コマンド: `t2i_status` / `t2i_start_server` / `t2i_stop_server` /
    `t2i_download_model`(進捗 `t2i-model` イベント) / `t2i_set_model`。
  - `detect_python()` … システム Python（`python`/`py`/`python3`）を探し torch/diffusers の有無を確認。
  - `run_flow` の終端が t2i なら `t2i_ensure_server` で先にデーモンを起こす（▶実行が「押せば動く」）。

## フロント（React）

- `model.ts` … `filterOutputMime` に `t2i`→`image/png`。
- `ui/T2iSetupDialog.tsx` … Whisper/Ollama 流のセットアップ（Python/依存確認・モデル DL・サーバー起動）。
- `ui/T2iSettingsDialog.tsx` … AiSettings 流のパラメータ設定（モデル/負/ステップ/ガイダンス/サイズ/シード）。
- `App.tsx` … フィルタメニュー「画像生成: Text-To-Image …」、`handleAddFilter`/`handleEditFilter` の
  t2i 分岐（依存＋モデルが揃えば設定ダイアログ、未整備ならセットアップへ）。

## Python 環境

「システム Python 優先」。この開発 PC は torch 2.5.1+cu121 / diffusers 0.33.1 等が導入済みで追加導入不要。
`t2i_status.deps_ok` で判定。将来 venv 選択を追加できる（status に Python 候補を増やす）。

## モデル

Ollama 流の名前付き pull。Phase 1 は SD1.5 系（軽量・高速。RTX 3090 で ~2s/枚）。
`t2i_server.py` の `MODELS` と `lib.rs` の `T2I_KNOWN_MODELS` を対応させること。
既定 = `stable-diffusion-v1-5/stable-diffusion-v1-5`。`dreamshaper`(Lykon/dreamshaper-8) も候補。

## 検証（2026-06-30）

- デーモン単体: `/generate` が HTTP 200・有効 PNG(512x512)。fp16 lean モデル(~2GB)で初回 ~8.6s
  （モデルロード込み・以降は数秒）。SD1.5 既定モデルのDL〜生成まで一気通貫を実機確認。
- `cargo check` / `cargo test --lib`(29 passed) / `tsc --noEmit` すべて通過。
- 実機（GUI でカードを繋いで ▶ 実行）はユーザー確認待ち。

## Phase 2（2026-06-30 実装・実機検証済み）: img2img ＋ FLUX ＋ バックエンド抽象化

### バックエンド抽象化（拡張点）
`t2i_server.py` を **モデル種別ごとの `Backend`** に再構成（`SDBackend` / `FluxBackend`、`BACKENDS`
レジストリ）。`MODELS` の `"kind"` で実装にディスパッチ＝Engine は種別非依存。新種別（SDXL /
ControlNet 等）は `Backend` を継承して `BACKENDS` + `PATTERNS_BY_KIND` に1エントリ足すだけ。
取得パターンも種別ごと（SD=fp16 safetensors ~2GB / FLUX=transformer+T5+VAE ~26〜34GB）。

### img2img（テキスト＋画像 → 画像）
- t2i カードは **集約ノード**化（`accept_mime:"*/*"`）。入力を種別で振り分け＝**テキスト→プロンプト /
  画像→元画像**。`run_t2i(spec, prompt, init_image)`、`execute_node` で振り分け、`eval_filter` で T2i を
  集約パスへ。設定に **strength（変化度 0〜1）** を追加。
- img2img パイプは `AutoPipelineForImage2Image.from_pipe` でコンポーネント（と CPU オフロード）を
  引き継ぐ＝VRAM 二重消費なし・SD/FLUX 共通。
- 検証: SD txt2img/img2img・FLUX txt2img/img2img（flux-lite 1024² ~35s）すべて HTTP 200・有効PNG。

### FLUX.1
- `FluxBackend`: `FluxPipeline`・bf16・24GB GPU は `enable_model_cpu_offload()`（やや遅いが確実）。
  負プロンプト非対応・ガイダンス蒸留（schnell/lite は guidance 0・4 step）。
- モデル: **`flux-lite`（Freepik/flux.1-lite-8B・オープン・~26GB）で検証**。公式 `flux-schnell` /
  `flux-dev` は **HF がゲート化**（要ライセンス承諾＋`HF_TOKEN`）＝レジストリに載せて auth 付きで取得可。
- 既知の留意点: FLUX は初回ロード ~26GB（オフロード）で重い。`is_available` は model_index.json だけ見るので
  **DL 途中でも available に出る**（全シャード揃う前にロードすると失敗）— 改善候補。

## 既知の留意点 / Phase 3 以降

- **img2img（部分修正の元画像入力）**: デーモンは `init_image`(base64)+`strength` を既に受けられる。
  Rust/フロントから画像入力（2本目の接続＝image）を渡す配線が Phase 2。要件「入力＝テキスト＋画像」。
- **対話パネル**: ChatPanel 風の T2i パネル（プロンプト入力＋プレビュー＋画像添付）は Phase 2。
- **デーモンの停止**: 現状 TexoS 終了時に自動 kill していない（`t2i_stop_server` で明示停止）。
  起動時自動起動（モデルがあれば health だけ立てる）も将来の改善候補。
- **バージョン**: 版数 bump と main へのコミットはユーザーのフロー（未実施）。
