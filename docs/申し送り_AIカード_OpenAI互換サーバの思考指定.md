# 申し送り: AIカード — OpenAI互換（汎用）サーバへの思考指定

作成: 2026-10-08（Strata 導入セッションから。別スレッドで着手するための指示書）

> **【完了】2026-10-08 — v1.9.2 としてリリース済み（commit af442e2・push 済み・インストーラ I:\texos_1.9.2_x64-setup.exe）**
>
> 決着した論点（ユーザー合意・いずれも推奨案）:
> - 送る条件 = (B) カード設定「思考の指定を送る（reasoning_effort）」`send_reasoning`（既定オフ）。Strata 専用分岐（A）は作らない
> - 値の対応 = 使わない→`none`／浅く・中・深く→`low`/`medium`/`high`／使う→送らない（サーバ既定）。未知の値も `none` 扱いで、任意の文字列は送らない
> - (C) 追加パラメータ `extra_params` = 全サーバ共通で本体トップレベルへマージ。`model` `messages` `stream` `temperature` `think` `reasoning_effort` `options` は拒否（画面は保存不可・実行時はエラー）。実行ログの「送信パラメータ」に残る
> - システムプロンプトの誤記対策 = API パラメータらしい記述があれば注意書きを出す（送信内容は変えない）
>
> 受け入れ条件の結果:
> - `plan_think` の単体テスト（`plan_generic_openai_sends_only_when_enabled`）・追加パラメータのテスト追加。LM Studio / Ollama の既存テストは変更なしで通過（回帰なし）
> - Strata 実機（ignored テスト `strata_live_reasoning_effort_and_budget`）: none/low/medium/high とも 400 なし、none で思考が返らないことを確認。ユーザーも実機で思考禁止が効くことを確認
> - 未確認: `reasoning_budget_tokens` で Strata コンソールの `thinking: … of max` が上限付近で打ち切られる様子（短い質問では見えなかった）
> - `thinkHint()` 更新済み（「送りません」の文言は削除）
>
> 同じリリースでの付随修正: チャットの抜粋書き出しカードをパネルの右外・上辺から空き段へ重ねずに置く（`ai.rs` の `excerpt_dx` / `free_excerpt_dy`）

## 背景
- ローカルLLMサーバ **Strata**（`C:\Strata\Strata-main`、`http://127.0.0.1:8080/v1`、OpenAI互換＋Anthropic互換）を導入した。モデル `qwen3.8-flash-next-iq2_xs`（画像入力あり）
- TexoS の AIカード（サーバー=「OpenAI互換（その他）」）から画像付き対話まで通っている
- ところが AIカードの「思考」欄（使わない／使う／浅く／中／深く）は、OpenAI互換（汎用）では**何も送らない**。画面の説明文もそう言っている
- ユーザーは思考の深さ・上限をカード側から制御したい。システムプロンプトに `"reasoning_budget_tokens": 16000` と書いて試したが、それは本文として送られるだけで効かない（この誤解を生まない UI にしたい）

## 現状のコード（v1.9.1 / commit 1c78cc5 時点）
- 判定の本体: `src-tauri/src/ai_models.rs` の `plan_think`（225行付近〜）
  - `is_openai_compat(provider)` かつ `provider != "lmstudio"` → `plan.note = "OpenAI互換（汎用）サーバには思考の指定を送りません"` で即 return
  - `lmstudio` のときだけ `plan.reasoning_effort = Some(level)` を立てる（「使う」は levels 専用モデルなら medium、それ以外は送らない）
- 送信: `src-tauri/src/flow.rs` 1382行付近・1768行付近 `body["reasoning_effort"] = ...`（`plan.reasoning_effort` があれば送る。汎用 OpenAI 経路でも同じ body 組み立てを通っているか要確認）
- 画面の説明: `src/ui/AiSettingsDialog.tsx` の `thinkHint()`（651行付近）
- 既存テスト: `ai_models.rs` の `plan_lmstudio_uses_reasoning_effort_for_levels_only` ほか（898行付近〜）
- 能力の仕様書: `docs/ai-card-model-capabilities-spec.md`

## Strata 側の事実（ソース `C:\Strata\Strata-main\serve\frontend.py` / `server.py` で確認済み）
- OpenAI Chat Completions の `reasoning_effort` を受け付ける。値の対応（`frontend.py` の `EFFORT`）:
  - `none` / `off` / `minimal` / `disabled` / `false` → 思考なし（`enable_thinking=False`）
  - `low` → low、`medium` → medium
  - `high` / `xhigh` / `max` → xhigh（**このモデルの既定の深さ**）
  - それ以外の文字列は **400 エラー**（未知の値を送ると対話が止まる）
- `reasoning: {"effort": ...}`（Responses 形式）と `chat_template_kwargs: {"enable_thinking": false}`（vLLM/llama.cpp 流）も受け付ける
- 独自拡張 **`reasoning_budget_tokens`**（整数・0=上限なし）をリクエスト本体の**トップレベル**で受け付け、サーバ設定の既定値より優先する
  - サーバ既定は `C:\Strata\Strata-main\strata-iq2_xs.json` の `"reasoning_budget_tokens": 8000`（2026-10-08 に設定。暴走して 32k トークン考え続けた事故の対策）
  - 上限に達すると思考を閉じて回答に移る（回答なしで終わらない）
- Strata かどうかの判別: `GET /health`（`/v1` の付かないルート）が `{"service": "strata", ...}` を返す。`GET /v1/status` は `"dialects"` 等を返す
- モデル一覧 `GET /v1/models` は `architecture.input_modalities: ["text","image"]` を返すが、思考能力（capabilities）は返さない → AIカードの「このサーバは能力を返しません」表示はそのとおり

## やりたいこと（ユーザー要望）
1. 「思考」欄の 使わない／浅く／中／深く を、OpenAI互換（汎用）サーバにも `reasoning_effort` として届ける
2. 思考の上限トークン（`reasoning_budget_tokens`）をカードから指定できる

## 設計の論点（着手前にユーザーと決めること）
- **汎用サーバへ送ってよいか**: 汎用 OpenAI 互換サーバの中には未知のパラメータで 400 を返すものがある（Strata 自身も未知の effort 値は 400）。今「送らない」にしているのはその安全策のはず。選択肢:
  - (A) Strata を自動判別したときだけ送る（`/health` の `service` を見る）。安全だが Strata 専用の分岐になる
  - (B) カード設定に「思考の指定を送る（reasoning_effort）」のオン/オフを足す（既定オフ）。汎用的
  - (C) 「追加パラメータ（JSON）」欄を足し、`{"reasoning_budget_tokens": 16000}` 等を body のトップレベルへマージする。最も汎用。要望2もこれで満たせる
  - 推奨案（叩き台）: **(B)＋(C)**。思考欄は B で正式対応、上限トークンのような独自拡張は C で逃がす。Strata 専用分岐（A）は作らない＝ TexoS の設計理念「OSSに乗る・全部自前で持たない」（memory `project_texos_philosophy`）と整合
- **「使う」の意味**: Strata では無指定＝サーバ既定（xhigh＋予算8000）。LM Studio の扱い（使う＝送らない）と揃えるなら「使う」は送らない。「深く」= `high`（Strata では xhigh に写像）
- **「使わない」**: 現状は何も送らない（＝サーバ既定で思考してしまう）。B をオンにしたら `reasoning_effort: "none"` を送るか？（LM Studio 経路の挙動も確認のこと）
- **システムプロンプトへの誤記対策**: プロンプト欄に `reasoning_budget_tokens` 等の API パラメータ名らしき文字列があれば、説明文で「ここは本文として送られます。パラメータは追加パラメータ欄へ」と注意する程度で良いか

## 受け入れ条件（案）
- `plan_think` の単体テスト追加: provider=openai で B オン時に 浅く/中/深く → low/medium/high、使わない → none（または決めた挙動）、B オフ時は従来どおり何も送らない
- C の追加パラメータは JSON として不正なら保存時にエラー表示。予約キー（`model` `messages` `stream` 等）の上書きは拒否
- 実機確認（Strata 起動中に）:
  - 浅く で送ったとき Strata のコンソールの思考トークン数が明確に減る
  - 追加パラメータ `{"reasoning_budget_tokens": 2000}` で、コンソールの `thinking: … of max` が 2000 付近で打ち切られる
  - 未知の effort 値を送らない（400 にならない）
  - LM Studio / Ollama 経路のテストと挙動が変わらない（回帰なし）
- 画面の説明文 `thinkHint()` を新しい挙動に合わせて更新（「送りません」の文言を消す）

## 進め方の約束（このプロジェクトの流儀）
- 実験リビジョン刻み・使いたい機能優先（feedback_texos_dev_style）。まず dev で動かしてユーザーが Strata で使って確かめる → リリース番号はその後
- 着手前に現状コードを読み、上の「設計の論点」をユーザーに提示して合意してから実装
- コミット・リリースはユーザーの指示があってから

## 参考
- Strata 導入と委任実験の経緯: memory `project_strata_local_claude`
- Strata の設定画面（ブラウザ）からもサーバ既定の `reasoning_budget_tokens` を変更できる（次回起動から有効）
