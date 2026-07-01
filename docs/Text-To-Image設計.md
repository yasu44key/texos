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

## Phase 3（2026-07-01 実装）: モディファイア層（SD/SDXL 中心の拡張点）

**SERVER_VERSION "7"→"8"（モディファイア層）→"9"（不一致 LoRA ガード）。v1.4.16 に同梱。**
よく使われる SD/SDXL 拡張を、モデル種別(kind)とは**直交する修飾**として
`t2i_server.py` に追加。いずれも常駐 pipe を書き換えるので **毎生成、要求状態へリコンサイル**（差しっぱなし
＝次生成へのリークを防ぐ）。ロード済みアセットは名前でキャッシュ＝繰り返しは再読込なし。新しい修飾は
`Backend` にメソッド1本＋能力フラグ（`supports_*`）を足すだけ＝拡張点。

- **対応**（SD/SDXL）: **LoRA**（複数・強度／PEFT アダプタ API で有効化・切替）／**Textual Inversion 埋め込み**
  （`home/embeddings` 全自動ロード＝名前をプロンプトに書けば効く／A1111 流）／**VAE 差し替え**（`home/vae`・
  単一ファイル or diffusers フォルダ）／**サンプラー選択**（`SAMPLERS` レジストリ・既定 DPM++）／**Clip skip**
  （A1111 流。diffusers の値は A1111-1）。FLUX は LoRA/VAE のみ（`supports_*` で無効化）。
- **アセット置き場**: `home/loras` `home/embeddings` `home/vae`（`models/` とは分離＝モデル一覧を汚さない）。
  `GET /assets` で一覧（loras/embeddings/vae/samplers）。名前は「拡張子なしファイル名」で指定可。
- **配線**: `/generate` に `lora`（"name:weight, .." 自由記述・デーモンは複数受理可）/`vae`/`sampler`/`clip_skip`。
  Rust `FilterSpec`＋`run_t2i_blocking` で素通し。
- **UI＝フィルタ流儀「手持ちから選ぶ」（セレクト）**: 「生成＝画像変換の手段」というコンセプトに沿い、モディファイアも
  フリーテキストでなく**実在アセットからの select**。Rust `list_t2i_assets`（`home/loras・embeddings・vae` を直接走査＝
  **daemon 非依存**・モデル一覧と同じ流儀）を `t2i_status` の `loras/embeddings/vae` に載せ、`App.tsx`→`T2iSettingsDialog`
  へ。**LoRA=選択＋強度**（何を・どれだけ／複数は将来）、**VAE=選択**、**サンプラー=選択**、Clip skip=数値。埋め込みは
  自動ロードなので UI 欄なし（利用可能名をヒント表示）。保存済みでアセットが消えても現在値は選択肢に残す。
- **依存＆不一致ガード**: LoRA は **`peft`** が要る（この開発PCは導入済み: peft 0.19.1 / diffusers 0.33.1）。
  **落とし穴＝LoRA とベースモデルの系(SD1.5/SDXL/FLUX/DiT)が不一致だと、`load_lora_weights` は例外を投げず通るが
  アダプタが0層で未登録→直後の `set_adapters` が「present adapters: set()」で 500**。対策として `apply_loras` が
  `get_list_adapters()` で実登録を確認し、未登録なら `delete_adapters` して**明示 ValueError**（「現在のモデル(kind)に
  適合しません＝SD/SDXL 用でない可能性」）＋キャッシュに誤登録しない（実機で krea2/DiT 系 LoRA を SD/SDXL に入れて
  遭遇→"9" で対策）。
- **次リビジョン**: **LoRA 複数掛け UI**（比較的簡単・下の「次リビジョン候補」参照）→ ControlNet（別バックエンド・
  複雑ゆえ次へ＝ユーザー合意）→ compel（`(word:1.3)`）→ Hires fix・アップスケール。
- 検証: `python -m py_compile` / `cargo check` / `cargo test --lib`(30 passed) / `tsc --noEmit` 通過。純粋ヘルパ
  （LoRA 指定パース・アダプタ名・アセット解決/一覧）は実データで単体確認済み。**実機で SD/SDXL 用 LoRA を選択→生成成功を
  確認**（緑髪キャラで作風反映を目視）。不一致 LoRA は上記ガードで明示エラー。

---

## 現状（v1.4.16・2026-07-01・**モディファイア層まで実装済み**）＝続きの入口

> **build & push はユーザーのフロー**。本節を書いた時点では **未ビルド/未 push ＝この作業ツリーが最新**。
> ビルド/push 後に「v1.4.16・新コミットハッシュ・`I:\texos_1.4.16_x64-setup.exe`」へ更新すること。
> （直前の確定リリースは v1.4.14 = de1391c・`I:\texos_1.4.14_x64-setup.exe`）

**t2i フィルタは一通り完成（AI対話→grep→生成・img2img・手持ちモデル・中断）＋モディファイア層（LoRA/VAE/
サンプラー/Clip skip/埋め込み）まで実機動作。以下は別スレッドで続きを始めるための引き継ぎ。**

### 実装済み（すべて実機検証済み）
- `builtin:"t2i"`＝Ollama 流の常駐 diffusers デーモン。TexoS が起動・管理。テキスト→PNG 実身。
- **バックエンド抽象化**（`python/t2i_server.py` の `Backend`／`SDBackend`・`SDXLBackend`・`FluxBackend`・
  `BACKENDS` レジストリ）。`MODELS[kind]` でディスパッチ＝**種別非依存の拡張点**（ControlNet 等はここに1件足す）。
- **txt2img / img2img**（t2i カードは集約ノード＝テキスト入力→プロンプト・画像入力→元画像。`strength`）。
- **モデル**: `sd1.5`/`dreamshaper`/`flux-lite`（オープン）＋公式 `flux-schnell`/`flux-dev`（HF 認証）。
- **手持ちモデル**: ①diffusers フォルダ手動配置 ②**単一ファイル(.safetensors/.ckpt)**＝safetensors ヘッダの
  キーから SD/SDXL/FLUX を自動判別（`_detect_kind_file`）。SDXL は既定 1024。**FLUX 単一ファイルは非対応**（T5 別）。
- **セットアップ**（`T2iSetupDialog`）: Python 検出・モデルDL（進捗/**レジューム**/**完全性チェック**）・
  **HF トークン（ファイル方式）**・手動配置導線（📁配置先を開く/🔄更新）・サーバー停止・**設定に常駐リンク**。
- **中断＆制限時間**: 制限時間欄（既定600・0=無制限）／`/cancel`＝生成を次ステップで止め **GPU 即解放**
  （run_t2i タイムアウト時＋フロー中断ボタンから）／**自己修復**＝版照合で古いデーモンを入替。
- **モディファイア層（v1.4.16・SD/SDXL 中心）**: LoRA（選択＋強度）／VAE（選択）／サンプラー（選択）／Clip skip／
  Textual Inversion（`home/embeddings` 自動ロード＝名前をプロンプトに書くだけ）。アセットは `home/{loras,embeddings,vae}`
  に置く（`models/` と分離）、Rust `list_t2i_assets`→`t2i_status`→`T2iSettingsDialog` のセレクトへ。**不一致 LoRA ガード**あり。
  詳細は上の「Phase 3（実装）」節。FLUX は LoRA/VAE のみ。
- デーモンは `include_str!` で埋め込み（**SERVER_VERSION "9"**）。

### ファイルマップ
- `python/t2i_server.py` — デーモン本体（Engine/Backend/HTTP）。**Rust に include_str! で埋め込み**。
- `src-tauri/src/flow.rs` — `FilterSpec` の t2i フィールド／`NodeKind::T2i`／`run_t2i`／`t2i_post_cancel`／`T2I_PORT=8765`。
- `src-tauri/src/lib.rs` — `execute_node` の T2i／デーモン管理コマンド群（status/start/stop/download_model/
  set_model/set_hf_token/reveal_model_dir/delete_model）／完全性チェック（`t2i_dir_complete` 等）／`T2I_SERVER_VERSION`。
- `src/model.ts`（t2i→image/png）／`src/ui/T2iSetupDialog.tsx`・`T2iSettingsDialog.tsx`／`src/App.tsx`・`menuBuilder.ts`（配線）。

### 運用の要点（他PC / 他スレッド）
- **画像生成エンジンは本体に同梱していない**。各PCに system Python＋`torch diffusers transformers accelerate
  safetensors`（NVIDIA なら CUDA torch）が要る。無ければセットアップの「Python＋生成ライブラリ」が✗。
- モデルは `%APPDATA%\com.yoshi.texos\t2i\models\`。HF キャッシュ・トークン・スクリプトも `…\t2i\` 配下。
- **HF トークン**は `config.json` の `t2i_hf_token`（TexoS が `…\t2i\hf_token` へ同期）→ デーモンが pull ごとに読む＝再起動不要。
- **デーモンを変えたら** python `SERVER_VERSION` と Rust `T2I_SERVER_VERSION` を**両方**上げる＋**再ビルド**
  （埋め込みはビルド時）。版照合で稼働中の古いデーモンが自動で入れ替わる。
- 検証時は**同一ポート(8765)に手動デーモンを乱立させない**（混在で切り分けが困難になった実績あり）。

### 次リビジョン候補（未実装・`docs/気づき.txt` 由来。※旧「Phase 3 候補」を改称＝Phase 3 は実装済のため）

**★t2i モディファイアの次の一手＝LoRA 複数掛け（比較的簡単・UI 据え置き方針でも入る）**:
デーモン/配線は**既に複数受理済み**（`_parse_lora_specs` が `"a:0.8, b:0.6"`・リスト・dict を `[(name,weight)]` に正規化し、
`apply_loras` が PEFT `set_adapters` で複数アダプタを同時有効化＝リコンサイルも対応）。**残るは UI のみ**＝
`T2iSettingsDialog` の LoRA を「行を追加できる複数選択＋各行の強度」にして、保存時に `"name:weight, .."` へ連結するだけ
（`lora` フィールドは文字列のまま）。**前提＝全 LoRA がベースモデルの系(SD/SDXL)と一致**（不一致は "9" ガードが弾く）。
ControlNet より軽い＝先に入れて良い。

その他（t2i / UX 一般）:
1. **t2i インライン編集パネル**（ChatPanel 風＝ポジ欄/ネガ欄（やや小）/実行ボタン。追加プロンプトを編集しつつ生成）。★本命候補。
2. **ワークフローの部分を別プロジェクトへコピー/移動**（つながり選択→右クリック→**仮身化コピー**(CAS依存)／
   **実身化移動**(元から分離)）。`docs/共有とモジュール設計.md` のデータ2レーンと直結。
3. AI チャット欄の Ctrl+C コピーが不安定（できる時とできない時がある）。
4. 終了時デーモン自動 kill／起動時自動起動（VRAM 常駐の後始末）。
5. DL 進捗の粒度（現状は file-count バーで粗い）／ControlNet 等のバックエンド追加。

### 参考
- 詳細な実装知見・ハマり所の履歴は**メモリ `[[project_texos_t2i]]`** に集約（本ドキュメントの補完）。
- AI フィルタ設計は `[[project_texos_ai_card]]`、共有/モジュールは `docs/共有とモジュール設計.md`。
