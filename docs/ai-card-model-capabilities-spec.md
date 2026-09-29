# AIカード: モデル能力検出・選択 設計メモ

対象: TexoS の AIカード
目的: ローカルにダウンロード済みのOllamaモデルを、能力（tools / thinking / vision / embedding）に基づいて使い分けられるようにする。

## 0. 未確認事項（Codeがリポジトリで確認すること）

この文書は言語・既存構造を前提にしていない。以下は実装前にリポジトリを調査して合わせる。

- AIカードの既存実装と、LLM呼び出しのProvider抽象の有無
- 実行ログ（ノード単位トレース）の既存スキーマ
- 設定・キャッシュの永続化先（DB / ファイル）
- カードの「途中の出力もキャンバスに出す」スイッチの実装位置
- 型定義は擬似TypeScriptで記述。実装言語に読み替える。

## 1. 骨子

1. AIカードでモデルを選ぶとき、Providerがモデル一覧と能力を返す。
2. カードUIは能力に応じて選択肢・スイッチを制御する。
3. リクエストは能力に応じてパラメータを付与する。
4. 実行ログに、再現に必要なモデル識別子と実際に送った条件を記録する。

## 2. 支える技術（Ollama API）

| 目的 | エンドポイント | 使う項目 |
|---|---|---|
| モデル一覧 | `GET /api/tags` | `models[].name`, `models[].digest` |
| 能力・詳細 | `POST /api/show` `{"model": name}` | `capabilities[]`, `details.parameter_size`, `details.quantization_level`, `model_info["general.architecture"]`, `model_info["<arch>.context_length"]` |
| 実行 | `POST /api/chat` | `tools`, `think`, `options.num_ctx` |

- `capabilities` の値: `completion` / `tools` / `thinking` / `vision` / `embedding` / `insert`
- Ollama Cloud も `https://ollama.com/api/show` で同様に取得できる。
- 制約: `capabilities` はテンプレート等から判定される「機能の有無」であり、精度・品質は示さない。古いOllamaでは `capabilities` フィールド自体が返らない場合がある。

## 3. 関係図

```
[AIカード UI]
   │ モデル選択 / スイッチ操作
   ▼
[ModelCatalog] ──キャッシュ(provider+name+digest)──┐
   │ list() / get()                                 │
   ▼                                                │
[Provider インターフェース]  ← TexoS独自型で返す      │
   ├─ OllamaProvider ── /api/tags, /api/show ───────┘
   └─ その他Provider ── 静的テーブル（能力取得APIなし）
   │
   ▼ 実行時
[リクエスト構築] ── tools/think/num_ctx を能力に応じ付与
   │
   ▼
[実行ログ] ← provider, name, digest, 能力スナップショット, 実送信パラメータ
```

## 4. 章立て

### 4.1 データ型

```ts
type Tri = "yes" | "no" | "unknown";

interface ModelCapabilities {
  tools: Tri;
  thinking: Tri;
  vision: Tri;
  embedding: Tri;
}

type ThinkingMode =
  | { kind: "none" }
  | { kind: "boolean" }                          // think: true/false
  | { kind: "levels"; levels: string[] }         // think: "low"|"medium"|"high"
  | { kind: "unknown" };

interface ModelInfo {
  provider: string;            // "ollama" 等
  name: string;                // "qwen3:8b"
  digest: string;              // /api/tags の digest
  parameterSize?: string;
  quantization?: string;
  trainedContextLength?: number;  // 学習上限。num_ctxとは別物
  capabilities: ModelCapabilities;
  thinkingMode: ThinkingMode;
  fetchedAt: string;           // ISO 8601
}
```

### 4.2 能力の判定規則（Ollama）

- `capabilities` フィールドが存在しない → 全項目 `unknown`
- フィールドが存在する → 配列に含まれれば `yes`、含まれなければ `no`
- 「なし」と「不明」を区別する。`unknown` を `no` として扱ってUIから除外しない。
- `thinkingMode`:
  - `thinking` が `no` → `none`
  - gpt-oss 系 → `levels ["low","medium","high"]`
  - その他の `thinking` が `yes` → `boolean`
  - `unknown` → `unknown`
- gpt-oss 系の判定はモデル名ベースの暗黙ルールになる。実機で `think` に真偽値・文字列を渡した際の挙動を確認し、判定表はコード内で1か所に集約する（モデル系統ごとの例外が増える前提）。

### 4.3 キャッシュ

- キー: `provider + name + digest`
- 取得手順: `/api/tags` を取得 → digest が変わった / 新規のモデルのみ `/api/show` を呼ぶ。
- `/api/show` はモデルごとの呼び出しのため、並列数を制限する。
- 更新タイミング: カード画面を開いたとき、および手動更新。実行のたびには更新しない。
- 削除されたモデル（`/api/tags` に無い）はキャッシュから除去する。

### 4.4 カードUIの挙動

- モデル選択: カードで有効にしている機能（ツール使用・thinking・画像入力）に対し、`no` のモデルは選択不可、`unknown` のモデルは警告付きで選択可。
- スイッチ: 選択モデルが該当能力 `no` のとき、対応スイッチを無効化する。
- 表示: 能力は「機能の有無」として表示する。「精度が高い」等の品質を示唆する表現は使わない。
- コンテキスト長: `trainedContextLength` は「学習上限」と明記して表示する。実行時の `num_ctx` は別設定として持つ（Ollamaのデフォルト `num_ctx` は学習上限より小さいため）。

### 4.5 リクエスト構築

- `tools`: カードのツールスイッチON かつ `tools != no` のときのみ付与。
- `think`: カードのthinkingスイッチON かつ `thinking != no` のときのみ付与。値は `thinkingMode` に従う（boolean / levels）。
- `num_ctx`: カード設定値があれば `options.num_ctx` に付与。
- `unknown` のモデルでAPIがパラメータ非対応エラーを返した場合: 自動リトライしない。エラーを表示し、実行ログに記録する。理由: リトライで条件が変わると、ログ上の実行条件と実際の実行が乖離し、再現性が崩れるため。

### 4.6 実行ログへの記録

各AIカード実行のログに以下を含める。

- `provider`, `model name`, `digest`
- 実行時点の能力スナップショット（4.1の `capabilities`, `thinkingMode`）
- 実際に送信したパラメータ（`tools` の有無、`think` の値、`num_ctx`）
- 応答に `thinking` フィールドが含まれていたか

理由: 同一名（例 `qwen3:8b`）でも `ollama pull` で中身が入れ替わる。名前だけの記録では追記専用ログの再現性が保てない。

### 4.7 Provider抽象

- `Provider.listModels(): ModelInfo[]` は TexoS独自型を返す。Ollama固有のレスポンス形式をカード側に漏らさない。
- Ollama以外のProviderは能力取得APIがない前提で、静的テーブルから `ModelInfo` を生成する。テーブルの保守は手動。

### 4.8 非目標

- 用途に応じたモデルの自動選択（ルーティング）: 行わない。理由: 能力情報だけでは品質差を判断できず、判断基準が未定義のため。手動選択とフィルタ表示のみ。
- モデル品質のスコアリング・ベンチマーク: 行わない。理由: capabilities から得られない情報であり、本機能の範囲外。

### 4.9 未決事項

- thinking の出力（`thinking` フィールド）を、カードの「途中の出力もキャンバスに出す」スイッチの対象に含めるか。
- キャッシュの永続化先と、TTL（digest比較のみで足りるか）。
- 静的テーブルを持つ他Providerの範囲。

## 5. 受け入れ条件

1. ローカルの全モデルについて、`capabilities` と `thinkingMode` が取得・表示される。
2. `capabilities` を返さないOllamaに対し、全能力が `unknown` になり、選択可能（警告付き）である。
3. `tools` が `no` のモデルでは、ツールスイッチが無効化され、`tools` がリクエストに含まれない。
4. gpt-oss 系には `think` に文字列レベルが、boolean系には真偽値が送られる。
5. `ollama pull` でモデルが更新された後、画面更新で digest 差分のモデルのみ `/api/show` が再実行される。
6. 実行ログに digest・能力スナップショット・実送信パラメータが記録される。
7. `unknown` モデルでの非対応エラー時、自動リトライされず、エラーがログに残る。
