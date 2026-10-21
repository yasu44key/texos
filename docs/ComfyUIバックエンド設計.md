# ComfyUI バックエンド設計（t2i フィルタの外部サーバ化・設計メモ）

作成: 2026-10-21。**Phase 1 を同日実装・dev で実機確認済み（未コミット）＝末尾「実装状況」参照**。前提文書: `docs/Text-To-Image設計.md`（自前 diffusers デーモン）、
`docs/申し送り_AIカード_OpenAI互換サーバの思考指定.md`（外部サーバへの「汎用」接続の作法）。

## 0. 何をしたいか

Text-To-Image フィルタ（`builtin:"t2i"`）に、自前デーモン（`python/t2i_server.py`・8765・Rust が起動管理）とは
別のバックエンドとして **ComfyUI** を足す。カードは ComfyUI の **API 形式ワークフロー JSON** と
「どこに何を差し込むか」を持ち、TexoS は値を差し込んで `/prompt` に投げ、出来た PNG を実身にする。

```
[テキスト（プロンプト）] ─flow─┐
[画像（i2i / 編集の元）]  ─flow─┼→ [t2i カード backend=comfyui] ──→ [PNG 実身]
                                │       │ ① /upload/image（画像入力）
                                │       │ ② ワークフローに値を差し込み POST /prompt
                                │       │ ③ /history（または /ws）で完了待ち
                                │       └ ④ /view で PNG を取得
```

狙い:
- 新しいモデル（Qwen-Image 2.1 など）への対応を **自前デーモンの Backend 追加ではなく ComfyUI に任せる**。
  TexoS の方針「OSS に乗る・全部自前で持たない」（AI カードが Ollama / LM Studio / 汎用 OpenAI 互換に乗るのと同じ）。
- サーバは **外にある**（TexoS は起動・停止しない）。同じ PC でも、LAN の別 GPU 機でもよい。

### 実績（この設計の根拠）
- ローカル ComfyUI v0.41.0（`E:\ComfyUI_windows_portable_nvidia\ComfyUI_windows_portable_New`、`127.0.0.1:8188`）。
- Qwen-Image 2.1 の t2i テンプレート（`image_qwen_image_2_1_t2i.json`）と画像編集テンプレート
  （`image_qwen_image_2_1_image_edit.json`・画像入力 `image_1`〜`image_10`・**サブグラフ構成**）を、ブラウザ経由で
  `/prompt` に投げて値を書き換え、t2i と画像編集（ゴスロリ画像→フィギュア化）を生成できた。
- RTX 3090 で 1024px・25 step・**約 60〜80 秒/枚**（SD1.5 の自前デーモン ~2 秒/枚とは桁が違う＝制限時間・進捗表示が要る）。

---

## 1. カードが持つもの

### 1-1. 形 — t2i カードに `backend` を足す（新 builtin は作らない）
`builtin:"t2i"` のまま `FilterSpec` に `backend` を追加する。未指定＝`"diffusers"`（既存カードの挙動を変えない＝
serde 後方互換。AI カードの `provider` 未指定＝ollama と同じ流儀）。

| フィールド | diffusers（既存） | comfyui（新） |
|---|---|---|
| `backend` | 未指定 / `"diffusers"` | `"comfyui"` |
| `endpoint` | 使わない（8765 固定） | 接続先 URL（空＝全体設定の既定。§3） |
| `comfy_workflow` | — | API 形式ワークフロー JSON（§1-2） |
| `comfy_bindings` | — | 差し込み口の定義（§1-3） |
| `comfy_output` | — | 画像を取り出す出力ノード ID（§1-3） |
| `prompt` / `negative` / `steps` / `guidance` / `width` / `height` / `seed` | デーモンへ | **差し込み口があるものだけ**差し込む（無い口は設定画面で出さない） |
| `strength` | img2img の変化度 | 口があれば（KSampler の `denoise` 等）差し込む |
| `lora` / `vae` / `sampler` / `clip_skip` | デーモンへ | **使わない**（ワークフロー側の責務）。設定画面で隠す |
| `timeout_secs` | 共通契約（未指定＝無制限） | 同じ。超過時は §2-5 の中断 |

`NodeKind` は `T2i` のまま。`run_t2i()` の入口で `backend` を見て `run_t2i_comfy()` に分岐する。
`flow_run.rs` の入力振り分け（テキスト→プロンプト／画像→元画像）・パネルの追加プロンプト合成
（`own_prompt()` を ", " で連結）・中断時の出力破棄はそのまま共有する。違うのは「画像は 1 枚に限らない」点だけ（§1-4）。

### 1-2. ワークフロー JSON（API 形式）
ComfyUI には 2 つの JSON 形式がある。

| 形式 | 中身 | `/prompt` に投げられるか |
|---|---|---|
| UI 形式（テンプレート・保存ファイル） | `nodes[]` `links[]` `widgets_values`（位置で並ぶ）・サブグラフ定義・Reroute/Primitive/Note 等の UI 専用ノード | **投げられない** |
| API 形式 | `{ "<node_id>": { "class_type": "...", "inputs": { "<名前>": 値 または ["<node_id>", 出力番号] }, "_meta": {"title": "..."} } }` | 投げられる |

**変換方針（推奨＝TexoS は UI 形式を解釈しない）**:
1. **ComfyUI 側で API 形式に書き出してもらう**（推奨）。ComfyUI の「ワークフロー → エクスポート（API）」。
   サブグラフはこの時点で平坦化され、内部ノードは `"<サブグラフのノードID>:<内部ID>"` のような複合 ID になる
   （Qwen-Image 2.1 編集テンプレートが該当）。Primitive・Reroute は値・接続に解決済みで消える。
2. **ブラウザで実際に投げた `/prompt` の本文から採る**（今回の実績の方法）。開発者ツールのネットワークで
   `POST /prompt` の `prompt` フィールドがそのまま API 形式。1 と等価。
3. TexoS が UI 形式→API 形式を自前変換する（**採らない**）。`widgets_values` は位置配列なので `/object_info` の入力順と
   突き合わせる必要があり、`control_after_generate` のような UI 専用の値が混ざる・サブグラフの平坦化・
   Primitive/Reroute/グループノード・ノード無効化（bypass/mute）の解決まで要る。ComfyUI のフロントエンドの版に追従し続ける羽目になる。

TexoS 側の取り込み画面は「API 形式 JSON を貼る／ファイルを選ぶ」。UI 形式（`nodes` キーがある）を貼られたら
「ComfyUI で『エクスポート（API）』したものを貼ってください」と案内して拒否する（黙って失敗させない）。

### 1-3. 差し込み口（バインディング）
ワークフローのどのノードのどの入力に、TexoS のどの値を入れるか。

```jsonc
"comfy_bindings": [
  { "slot": "prompt",   "node": "75:6",  "input": "text" },
  { "slot": "negative", "node": "75:7",  "input": "text" },
  { "slot": "seed",     "node": "75:3",  "input": "seed" },
  { "slot": "steps",    "node": "75:3",  "input": "steps" },
  { "slot": "width",    "node": "75:58", "input": "width" },
  { "slot": "height",   "node": "75:58", "input": "height" },
  { "slot": "image",    "node": "78",    "input": "image", "index": 1 },   // image_1
  { "slot": "image",    "node": "106",   "input": "image", "index": 2 }    // image_2 …
],
"comfy_output": "60"   // SaveImage / PreviewImage のノード ID
```
（ID は例。実際の値はテンプレートの書き出し結果で決まる。）

- **slot の語彙**は既存 FilterSpec の名前に合わせる: `prompt` `negative` `seed` `steps` `guidance` `width` `height`
  `strength` `image`。カードの値はこの slot を通して差し込まれる＝設定画面は今の T2iSettingsDialog の欄を
  「口がある欄だけ出す」形で流用できる。
- `prompt` に入るのは **接続テキスト＋パネルの追加プロンプト**（既存の合成結果）。
- `seed` 未指定（＝毎回ランダム）は **TexoS が乱数を作って差し込む**。ワークフローに入っている固定値のまま投げると
  ComfyUI が「同じ入力」としてキャッシュを返し、新しい画像が出ない（§2-3）。使った seed は実行ログに残す（再現用）。
- 1 つの slot を複数ノードに入れてよい（例: width をラテントと画像スケーラ両方へ）。
- **自動推定**: 取り込み時に class_type から候補を出す（`CLIPTextEncode`／Qwen の TextEncode 系の `text`/`prompt` →
  prompt 候補、`KSampler` 系の `seed`/`noise_seed`・`steps`・`cfg`・`denoise`、`EmptyLatentImage`／`EmptySD3LatentImage` 等の
  `width`/`height`、`LoadImage` の `image`、出力は `SaveImage`/`PreviewImage`）。プロンプトが正・負どちらかは
  KSampler の `positive`/`negative` へ辿って決める。推定はあくまで初期値で、ユーザーが確認・修正する。
- **ノードの指し方**: 第一案はノード ID。ただし ID は書き出し直すと変わりうるので、`_meta.title`（ComfyUI 上で
  ノードに付けた名前）でも引けるようにする（`"node": "title:正プロンプト"`）。ワークフローを差し替えたときの
  付け替えが楽になる。どちらを主にするかは未決（§7）。

### 1-4. 入力画像の割り当て（i2i・画像編集）
- カードへの画像入力は **矢印を作った順**（規約 §3-3 の集約入力順）に `index` 1, 2, … の `image` 口へ割り当てる。
  Qwen-Image 2.1 編集なら `image_1`〜`image_10` 相当の `LoadImage` 群。
- 口より画像が多い → 余りは無視して警告。少ない → 使わない `LoadImage` が残る。`LoadImage` をファイル指定なしの
  まま投げると ComfyUI の検証エラーになるので、**使わない `LoadImage` 枝は外す**（API 形式ではそのノードを削除し、
  それを参照している入力キーも消す）。
- **実機確認済み（2026-10-21・v0.41.0・RTX 3090）**: Qwen-Image 2.1 編集テンプレートのメインサブグラフ
  （入力 `images.image_1`〜`images.image_10`）で、`image_1` に LoadImage(node 470) を 1 枚だけ繋ぎ、`image_2` の
  LoadImage(node 475) をノードごと削除した状態（`image_2`〜`image_10` が未接続）で `/prompt` に投入 → エラーなく
  success、約 78 秒で 1024x1024 PNG が出た。**未接続の画像口は optional 扱い**＝画像枚数別のワークフローは不要で、
  TexoS は余った `LoadImage` 枝を外すだけでよい。
  - 注意: 確認は UI 上で枝を外して投げたもの。API 形式の JSON で「未接続入力のキーそのものを持たない」状態でも
    通るかは Phase 1 で再確認する。
- 自前デーモンの img2img（`strength` で元画像を崩す）と、Qwen 編集（指示文で画像を変える）は意味が違う。
  t2i カードの「画像が来たら元画像」という入力規則は同じだが、`strength` 欄は口が無ければ出さない。

---

## 2. 実行シーケンス

reqwest::blocking を **tokio 文脈外の素の OS スレッド**で回す（`run_t2i` と同じ理由＝内部ランタイム drop パニック回避）。
HTTP クライアントは `gen_client(timeout_secs)`（生成系の制限時間の共通契約。blocking の既定 30 秒に注意）。
ただし 1 回の長い POST ではなく短い要求の繰り返しになるので、**全体の制限時間は TexoS 側の経過時間で判定**し、
個々の要求は短いタイムアウト（数秒〜アップロードは長め）にする。

### 2-1. 画像のアップロード（i2i・編集のときだけ）
```
POST {base}/upload/image   multipart: image=<PNG>, type=input, subfolder=texos, overwrite=true
→ {"name": "<保存名>", "subfolder": "texos", "type": "input"}
```
- ファイル名は **実身の CAS ハッシュ**（`<hash>.png`）にする。同じ画像の再実行は上書き＝重複が溜まらない。
  ComfyUI の `input/texos/` に TexoS 由来の画像がまとまる（掃除しやすい）。
- 差し込む値は `"texos/<hash>.png"`（`LoadImage.image` はサブフォルダ付きの名前を受ける）。
- 遠隔サーバではパス共有ができないので、ローカルでも常にアップロードする（経路を一本にする）。

### 2-2. 投入
```
POST {base}/prompt   {"prompt": <差し込み済み API JSON>, "client_id": "<TexoS の固定 UUID>"}
→ 200 {"prompt_id": "...", "number": n, "node_errors": {}}
→ 400 {"error": {...}, "node_errors": {"<node_id>": {"class_type": "...", "errors": [{"message","details",...}]}}}
```
- 400 は **ComfyUI 自身の検証結果**（モデルファイルが無い・入力値が範囲外・未知のノード）。`node_errors` を
  「ノード 75:6（CLIPTextEncode）: …」の形に整えてカードのエラーに出す。
- 差し込み前の JSON はカードの持ち物なので書き換えない（毎回コピーに差し込む）。

### 2-3. 完了待ち
**Phase 1 = `/history` ポーリング（推奨）**:
```
GET {base}/history/{prompt_id}   （1 秒間隔）
→ {} …… まだ（キュー待ち or 実行中）
→ {"<prompt_id>": {"status": {"status_str": "success"|"error", "completed": bool, "messages": [...]},
                   "outputs": {"<node_id>": {"images": [{"filename","subfolder","type"}]}}}}
```
- 待ち中は `GET /queue` で「キュー何番目／実行中」を見て `emit_job_progress` に出す（ComfyUI は他の利用者・
  ブラウザからも積まれうる＝順番待ちが起きる）。
- `status_str == "error"` は `messages` の `execution_error`（`node_id` `exception_message`）を整えてエラーにする。
- **キャッシュの落とし穴**: ComfyUI は前回と同じ入力のノードを再実行しない。全ノードがキャッシュ命中すると
  一瞬で終わる。出力ノードの結果が history に載るかは版によるので、seed を毎回変える（§1-3）のを基本にし、
  「outputs が空で success」は「キャッシュ命中＝前回と同じ」として扱う（前回結果の再取得か、エラー案内か＝§7）。

**Phase 2 = `/ws?clientId=<client_id>`（任意）**:
`progress {value, max, node}`（ステップ進捗）・`executing {node}`・`execution_success`・`execution_error`・
`execution_interrupted` が来る。60〜80 秒かかる生成で「何ステップ目か」が見えるのは価値が大きいが、
WebSocket クライアント（tungstenite 等）の依存追加が要る。完了判定は history を正とし、ws は進捗表示だけに使う
（ws が切れても生成は失敗にしない）。

### 2-4. 画像の取得
```
GET {base}/view?filename=<f>&subfolder=<s>&type=<output|temp>   → PNG バイト列
```
- `comfy_output` のノードの `images[0]` を採る。指定が無ければ outputs の最初の画像。
- 複数枚（バッチ・複数出力ノード）は Phase 1 では先頭 1 枚＋「他に n 枚あります」の警告。複数カード化は §7。
- 取得後、`POST /history {"delete": [prompt_id]}` で履歴を片付ける（任意）。
- **出力ファイルの溜まり**: `SaveImage` は ComfyUI の `output/` に毎回保存する。TexoS は実身を自分で持つので、
  ComfyUI 側にも残すかは選択（§7）。取り込み時に `SaveImage`→`PreviewImage`（`temp`＝ComfyUI 再起動で消える）へ
  差し替える案がある。既定は「ワークフローのまま」（ユーザーが ComfyUI で後から見返せる）。

### 2-5. 中断・制限時間
- フローの中断ボタン／制限時間超過 → `POST {base}/interrupt`。**/interrupt は「今実行中のもの」を止める＝
  他人の生成を止めうる**。新しめの ComfyUI は `{"prompt_id": ...}` 付きで自分のものだけ止められる（v0.41.0 で
  使えるか要確認＝§7）。使えないときは「実行中が自分の prompt_id のときだけ /interrupt」（`/queue` で確認）。
- まだキュー待ちなら `POST /queue {"delete": [prompt_id]}` で取り下げる。
- 中断後に完走が返ってきた場合の破棄は既存契約どおり（`flow_run.rs` の cancel 判定がそのまま効く）。
- `t2i_active` の点灯と `break_flow` の連動も既存どおり。`flow::t2i_post_cancel()` を backend で分岐させる
  （ジョブ単位で backend と prompt_id を覚えておく必要がある＝Job に持たせる）。

---

## 3. 接続先と死活確認

### 3-1. 接続先 URL
- 全体設定（`config.json`）に `comfyui_url`（既定 `http://127.0.0.1:8188`）。カードの `endpoint` が空ならこれ。
  AI カードの `effective_ai_endpoint()`（明示 endpoint 優先・空は provider 既定）と同じ形の
  `effective_t2i_endpoint()` を作り、資源セマフォのキーにも使う（§4-1）。
- 末尾 `/` の正規化は既存の `normalize_endpoint` を通す。
- **TexoS は ComfyUI を起動しない**（Phase 1）。Ollama・LM Studio・Strata と同じ「外のサーバ」扱い。
  未起動なら「ComfyUI（<URL>）に接続できません。ComfyUI を起動してください」。将来、起動コマンド
  （`run_nvidia_gpu.bat` 等）を設定に持たせて ▶ 実行で起こす案は §7。
- 認証: ComfyUI 本体には認証が無い。LAN 公開は「信頼できるネットワークだけ」と案内する。リバースプロキシ越しの
  Bearer トークン等を許すか（ヘッダ設定欄）は §7。

### 3-2. 死活確認
- `GET {base}/system_stats` → `system.comfyui_version`・`devices[].vram_total / vram_free`。
  - 到達確認と**版の記録**（§5 のキャッシュキー）と **VRAM 残量の表示**を 1 回で取れる。
- 確認するタイミング:
  - 設定ダイアログを開いたとき・「接続テスト」ボタン（AI 設定ダイアログのモデル一覧取得と同じ位置づけ）。
  - `run_flow` の終端が comfyui の t2i なら先に 1 回（自前デーモンの `t2i_ensure_server` の代わり。
    起こすのではなく「居るか」を見て、居なければ実行前に分かりやすく止める）。
  - 実行中は確認しない（`/prompt` の接続失敗をそのまま案内に変える。接続タイムアウトは `GEN_CONNECT_TIMEOUT_SECS`）。

---

## 4. VRAM の取り合い

同じ GPU に、自前 t2i デーモン（SD/SDXL/FLUX を常駐）・t23d デーモン（texture 込み 16GB 常駐）・Ollama（AI カード）・
ComfyUI（Qwen-Image 2.1 は 24GB をほぼ使う）が乗りうる。RTX 3090（24GB）では 2 つ同時常駐は基本的に無理。

### 4-1. 既存のセマフォとの関係
- `flow_run.rs` の資源取得: ai セマフォ（エンドポイント単位・許可数 1）＋ **gpu セマフォ（t2i/t23d 共通・排他 1）**。
- comfyui の t2i:
  - ai セマフォのキー＝ComfyUI の URL（同じ ComfyUI への同時投入は TexoS 内では 1 本ずつ。ComfyUI 自体が
    キューなので並べてもよいが、中断・進捗の対応が単純になる）。
  - gpu セマフォは **ComfyUI がローカル（127.0.0.1 / localhost / ::1）のときだけ**取る。LAN の別機なら取らない
    （この PC の GPU を使わない＝自前デーモンと並行してよい）。
- セマフォは「生成中だけ」の排他で、**常駐（生成していないのに VRAM を握っている）は防げない**。それが 4-2。

### 4-2. 常駐の後始末（排他の実体）
生成の直前に「相手」を空けさせる。

| これから使う | 空けさせる相手 | 手段 |
|---|---|---|
| ComfyUI（ローカル） | 自前 t2i デーモン | **デーモンに `/unload` を新設**（現状 `t2i_server.py` は `Engine.load` 内で旧 backend を unload するだけで、外から解放する口が無い）。無い版ならデーモン停止（`t2i_stop_server`）でも可 |
| ComfyUI（ローカル） | t23d デーモン | 同上（t23d 側にも解放口が要るか確認） |
| 自前 t2i / t23d | ComfyUI（ローカル） | `POST {base}/free {"unload_models": true, "free_memory": true}` |
| Ollama（AI カード） | ComfyUI / 自前 | 範囲外（Ollama は溢れた層を CPU に逃がすので致命的ではない）。将来の課題として記録 |

- 方針の選択肢（全体設定 `gpu_share`）:
  - **排他（推奨・既定）**: 上の表どおり、使う直前に相手を空けさせる。毎回モデルの載せ直し（Qwen で十数秒〜）が
    起きるが、OOM で落ちるよりよい。
  - 共存: 何もしない（48GB 機や、片方が CPU オフロード前提のとき）。
- `/free` は ComfyUI の**全モデル**を下ろす（他の利用者の分も）。共有サーバでは迷惑になるので、ローカル時だけ・
  排他設定時だけ送る。
- 失敗（OOM）の案内: ComfyUI の `execution_error` に `OutOfMemory` 系が含まれていたら「GPU メモリ不足。
  他の生成サーバ（TexoS の画像生成・3D 生成）を停止するか、全体設定の GPU 共有を『排他』に」と出す。

---

## 5. ノード名・入力名の変更への備え（`/object_info`）

ComfyUI は更新でノードの入力名が変わる・カスタムノードが消える・モデルファイル名が変わる。黙って変な画像が
出るより、実行前に分かる方がよい。

- `GET {base}/object_info` → 全ノードの `input.required / optional`（型・combo の選択肢）・`output`。数 MB あるので
  **接続先 URL＋`comfyui_version` をキーにキャッシュ**（app-data。版が変わったら取り直す。カスタムノード追加は
  版が変わらないので「更新」ボタンも置く）。単体は `GET /object_info/<class_type>`。
- 検証項目（取り込み時と「接続テスト」時。実行時は軽い分だけ）:
  1. ワークフローの全 `class_type` が object_info にある → 無ければ「カスタムノード <名前> が見つかりません」。
  2. 各バインディングの `node` がワークフローにあり、`input` がその class の入力名にある。
  3. 差し込む値の型が合う（INT/FLOAT/STRING、combo なら選択肢内）。width/height の min/max/step も object_info にある。
  4. **モデル系 combo**（`unet_name` `clip_name` `vae_name` `ckpt_name` `lora_name` 等）の現在値が選択肢にある
     → 無ければ「モデル <ファイル名> が ComfyUI にありません」。ComfyUI の 400 でも分かるが、取り込み時に分かる方がよい。
- combo の選択肢は設定画面の **select の元**にも使える（モデル差し替え口を `slot: "model"` で出せば、
  自前デーモンの「手持ちから選ぶ」と同じ UI 流儀になる）。Phase 2 の候補。
- それでも漏れたものは `/prompt` の `node_errors` が最終防衛線（§2-2）。

---

## 6. 自前デーモンを残すか

| 案 | 中身 | 長所 | 短所 |
|---|---|---|---|
| A. ComfyUI に一本化 | 自前デーモンを撤去 | 保守対象が減る。新モデル対応は ComfyUI 任せ | ComfyUI の導入が必須になる（ポータブル版でも数 GB＋モデル）。既存カード（SD/LoRA/VAE/サンプラー設定）の移行が要る。セットアップ画面の「押せば動く」が消える |
| **B. 併存・カードごとに選ぶ（推奨）** | `backend` で選択。自動切替はしない | 既存カードがそのまま動く。軽い SD は数秒の自前、Qwen 等の重い新モデルは ComfyUI と使い分けられる | 2 系統の保守。VRAM 排他（§4）が要る |
| C. 併存＋自動フォールバック | ComfyUI に繋がらなければ自前で生成 | 止まらない | **別モデルの画像が黙って出る**（Qwen のつもりが SD1.5）＝再現性が壊れる。プロンプト・解像度の前提も違う |

**推奨: B**。
- 自動フォールバック（C）はしない。ComfyUI に繋がらなければ明示エラー＋案内（AI カードが Ollama 不通で
  勝手に別サーバへ行かないのと同じ）。
- 自前デーモンは **据え置き（機能凍結寄り）**: 新しいモデル種別（Qwen-Image・新しい DiT 系）は自前の
  `Backend` に足さず ComfyUI で受ける。LoRA 複数掛け UI のような小さい改良は続けてよい。
- 将来、ComfyUI で SD1.5 も十分軽く動き、セットアップの手間も解決できたら A へ寄せる判断をする（その時点で再検討）。

---

## 7. 未決事項

1. **ワークフローの置き場**: (a) カードの spec にインライン（`comfy_workflow`。コピーで一緒に動く・Phase 1 推奨）／
   (b) プロジェクトの実身（CAS・JSON 実身を付箋のように繋ぐ＝役割ポート的な渡し方）／
   (c) app-data の共有テンプレート置き場から名前で引く（モデルと同じ流儀・複数カードで共有）。
   `docs/共有とモジュール設計.md` のデータ2レーンとの整合も含めて決める。
2. **ノードの指し方**: ID 主か `_meta.title` 主か。ワークフロー差し替え時のバインディング自動付け替えをどこまでやるか。
3. ~~**未使用の画像口**~~ → **解決済み（2026-10-21 実機確認）**: 未接続の画像口は optional 扱いで、そのまま
   投げられる。TexoS は使わない `LoadImage` 枝を外す（ノード削除＋参照キー削除）だけでよく、画像枚数別の
   ワークフローは不要（詳細は §1-4）。API 形式でも確認済み: 実機の `/history` に残っていた編集の投入本文は
   `images.image_1` のキーしか持たず（未接続の口はキー自体が無い）、それで success していた。
4. ~~**`/interrupt` の prompt_id 指定**~~ → **解決済み**: v0.41.0 の `server.py` は `{"prompt_id"}` 付きなら
   「実行中がそのジョブのときだけ」止める（無ければ全体中断）。実装はこれを使う。
5. **キャッシュ命中で outputs が空**のときの扱い（前回結果を `/history` から引き直す／エラー案内／seed 固定時は
   必ず起きる前提で前回画像を保持しておく）。
6. **複数画像の出力**（バッチ・複数 SaveImage）を複数カードにするか。する場合は既存のファンアウト出力の作法に合わせる。
7. **ComfyUI 側の出力ファイル**を残すか（SaveImage のまま）／TexoS 経由は PreviewImage に差し替えて残さないか。
   `input/texos/` にアップロードした画像の掃除を TexoS がやるか。
8. **進捗表示に WebSocket を入れるか**（依存追加の可否。Phase 2）。
9. **起動管理**: Phase 1 は外部起動のみ。起動コマンドを設定に持って TexoS が起こす案を入れるか（Ollama の扱いと揃える）。
10. **認証**: 認証付きリバースプロキシ越しの ComfyUI（Bearer ヘッダ等）を最初から許すか。
11. **自前デーモンの `/unload`** を新設するか、停止で代用するか（SERVER_VERSION を上げる必要がある＝Rust の
    `T2I_SERVER_VERSION` と両方）。t23d デーモンも同様。
12. **GPU 共有方針の既定**（排他／共存）と、Ollama の常駐（keep_alive）まで排他の対象にするか。
13. **「ComfyUI のどのワークフローか」の表示**: カード上にワークフロー名・モデル名をどう出すか（自前は model 名を出している）。
14. **MCP / AI エージェントからの操作**（v1.11 の MCP 接続）: エージェントがワークフロー JSON を差し替えたり
    バインディングを組んだりする操作を許すか。`docs/AIエージェント向け_フロー構築規約.md` に comfyui カードの
    書き方（バインディングの組み方・画像入力の順）を足す必要がある。
15. **t23d への波及**: ComfyUI には 3D 生成（Hunyuan3D 系）ワークフローもある。同じ「API 形式＋差し込み口」の
    仕組みを t23d にも使うか（汎用「ComfyUI カード」に育てるか、t2i の backend に留めるか）。

---

## 8. 段階案（参考）

- **Phase 1**: `backend:"comfyui"`／API 形式の取り込み（貼り付け）＋差し込み口の自動推定と手直し／
  `/upload/image`→`/prompt`→`/history` ポーリング→`/view`／接続テスト（`/system_stats`）／`/object_info` 検証／
  `/interrupt`・`/queue delete` での中断／ローカル時の gpu セマフォ＋`/free`。
  受け入れ: 実績の 2 テンプレート（Qwen-Image 2.1 t2i・画像編集 image_1 のみ）を TexoS のカードで ▶ 実行して
  PNG 実身が出ること。自前デーモンで SD を常駐させた状態から ComfyUI 実行で OOM にならないこと。既存の
  diffusers カードのテストが変わらないこと。
- **Phase 2**: WebSocket 進捗／モデル combo の select 化／複数画像口・複数出力／自前デーモンの `/unload`。
- **Phase 3**: 起動管理・テンプレート共有置き場・t23d への展開（§7-15 の判断次第）。

## 参考（コードの入口）
- `src-tauri/src/flow.rs` — `FilterSpec`（t2i フィールド）・`run_t2i` / `run_t2i_blocking`・`t2i_post_cancel`・
  `T2I_PORT`・`gen_client`（制限時間の共通契約）・`effective_ai_endpoint`（接続先の決め方の手本）。
- `src-tauri/src/flow_run.rs` — `NodeKind::T2i` の入力振り分けと中断時の破棄・資源取得（ai / gpu セマフォ）・
  `run_flow` 終端での `t2i_ensure_server`・`break_flow` からの `t2i_post_cancel`。
- `src-tauri/src/runtime/t2i.rs` — 自前デーモンの起動・停止・版照合。
- `src-tauri/src/lib.rs` — `ResourceLimits`（gpu セマフォ）・`config.json` の t2i 設定。
- `src/ui/T2iSettingsDialog.tsx` / `T2iSetupDialog.tsx` — 設定画面（comfyui 時の欄の出し分け先）。

---

## 実装状況（2026-10-21・Phase 1・未コミット）

dev（`npm run tauri dev`）で実機確認済み。テスト用プロジェクト `X:\Yoshiki\TexoS\ComfyUI画像生成テスト`
（`scripts/build_comfy_test.py` が組む）の3本を MCP 経由の ▶ で完走:
A テキスト→画像（Qwen t2i）62 秒／B 画像＋指示→フィギュア化（Qwen edit）82 秒／C 生成→フィギュア化の2段 143 秒。

- `src-tauri/src/comfy.rs`（新規）— 形式判定（UI 形式は案内付きで拒否・`/prompt` 本文の貼り付けも可）、差し込み口の自動検出
  （出力ノードから遡り、分岐は選ばれた側だけ辿る＝プロンプト強化の TextGenerate や使われない EmptyLatent を口にしない／
  プロンプトは Switch・PreviewAny を越えてリテラルまで辿る）、差し込み（余った LoadImage は枝ごと外す）、`/object_info` 検証
  （ノード有無・入力名・`*_name` のモデル combo）、実行（upload→prompt→history→view）、`comfy_check` / `comfy_analyze` コマンド。
  単体テスト 6 本＋実機テスト `comfy_live_qwen_t2i`（ignored・`TEXOS_COMFY_WF` で API JSON を渡せる）。
- `flow.rs` — FilterSpec に `backend` `comfy_workflow` `comfy_bindings` `comfy_output`。
- `flow_run.rs` — T2i で backend 分岐（画像は全部集めて矢印順に渡す・中断はポーリングで job.cancel を見て自分の prompt_id だけ
  `/interrupt`＋`/queue delete`）。資源: ai セマフォのキー＝ComfyUI の URL、gpu セマフォは**ローカル時だけ**。終端 t2i の
  `t2i_ensure_server` は comfy では呼ばない。preflight の Python 依存チェックも comfy では省く。
- VRAM 排他: ローカル ComfyUI で生成する前に自前 t2i / t23d デーモンが動いていれば**停止**（`/unload` 新設の代わり＝
  §7-11 は「停止で代用」で決着。次に内蔵で生成するとき ensure_server が起こし直す）。内蔵 t2i / t23d で生成する前は
  `POST 127.0.0.1:8188/free`。
- UI — `T2iSettingsDialog` に「生成エンジン」選択、ComfyUI 時は `ComfyT2iForm`（接続テスト・ワークフロー貼り付け→取り込み・解析・
  差し込み口の表・出力ノード・口のある欄だけ入力可＝空欄はワークフローの値）。`T2iSetupDialog` に「ComfyUI で作る」。
- 依存: reqwest に `multipart` feature を追加。
- 足場: `scripts/texos_flow.py` の集約 builtin に `t2i` `t23d` を追加（本体と同じ＝テキスト＋画像の2入力で誤警告していた）。
  ワークフロー見本 `scripts/comfy_workflows/*.json`（実機 `/history` から採取）。

設計からの差分・残り:
- 接続先は**カードの `endpoint` のみ**（空＝`http://127.0.0.1:8188`）。全体設定 `comfyui_url` は未実装。
- 進捗表示（`/ws`）なし。実行ログ（ノードの stderr）に使った seed と注意を残す。
- 設定ダイアログの画面は型検査のみで、目視の操作確認はまだ（カードのダブルクリックで開く）。
- `modules.rs` の依存列挙は comfy でも `t2i`（Python 環境）を要求として出す — comfy 用の区別は未対応。
