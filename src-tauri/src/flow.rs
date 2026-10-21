// v1.4 オートフローMVP（⑤）: フィルタ実行の純ロジック。
// 「UNIX CUI テキストフィルタパイプラインのGUI再来」の最小単位 —
// バイト列を受けてバイト列を返す。DB・キャンバスへの配線は lib.rs の run_flow。
use serde::Deserialize;

/// virtual_bodies.filter_ref に保存される JSON（V1.3後半_オートフロー設計.md 確定方針2）。
#[derive(Deserialize, Clone)]
pub struct FilterSpec {
    /// 内蔵フィルタ名: text_grep | text_replace | text_head | text_tail | text_sort |
    /// text_count | text_cut | text_convert | img_resize | img_convert | md_codeblock …
    #[serde(default)]
    pub builtin: Option<String>,
    /// 外部コマンド（{in}/{out} プレースホルダ）。builtin が無いときに使う逃げ道
    #[serde(default)]
    pub cmd: Option<String>,
    /// 受け入れMIME: "*/*" | "text/*" | "image/*" 等（接続時型チェック。front と同一規則）
    #[serde(default)]
    pub accept_mime: Option<String>,
    /// 出力拡張子（外部コマンド用。内蔵フィルタは自動決定）
    #[serde(default)]
    pub output_ext: Option<String>,
    // --- 内蔵フィルタの引数 ---
    #[serde(default)]
    pub pattern: Option<String>, // text_grep / text_replace（正規表現）
    #[serde(default)]
    pub replacement: Option<String>, // text_replace
    #[serde(default)]
    pub width: Option<u32>, // img_resize（高さはアスペクト維持）
    #[serde(default)]
    pub format: Option<String>, // img_convert: png | jpeg | webp | bmp | gif
    // --- v1.5.12 P1: Unix標準テキストフィルタ（標準フィルタ充実検討 §3）---
    /// text_head / text_tail: 残す行数（未指定は10）。
    #[serde(default)]
    pub lines: Option<u32>,
    /// text_sort: "asc"（既定）| "desc"。
    #[serde(default)]
    pub order: Option<String>,
    /// text_sort: "number" なら行頭の数値で比較（sort -n 相当）。既定は辞書順。
    #[serde(default)]
    pub compare: Option<String>,
    /// text_sort: "remove" なら並べ替え後に重複行を除去（sort -u 相当）。
    #[serde(default)]
    pub unique: Option<String>,
    /// text_cut: 区切り。"tab"（既定）| "comma" | "space"（連続空白）| 任意のリテラル。
    #[serde(default)]
    pub delimiter: Option<String>,
    /// text_cut: 取り出す列（1始まり）。"1,3-5"・"2-"（末尾まで）・"-3"（先頭から）。
    #[serde(default)]
    pub fields: Option<String>,
    /// text_convert: 入力文字コード。"auto"（既定: BOM→UTF-8厳密→推定）または
    /// encoding_rs のラベル（shift_jis / euc-jp / iso-2022-jp / utf-16le …）。出力は常に UTF-8。
    #[serde(default)]
    pub encoding: Option<String>,
    // --- v1.5.17 P2: 集約系テキストフィルタ（標準フィルタ充実検討 §3 第2候補）---
    /// text_concat: 入力間の区切り。"newline"（既定: 改行境界を保証）| "none"（純cat）|
    /// "blank"（空行を挟む）| "rule"（Markdown 区切り線 --- を挟む）。
    /// text_paste の区切りは text_cut と同じ `delimiter` を共用。
    /// text_diff の前後文脈行数は `lines` を共用（既定3）。
    #[serde(default)]
    pub separator: Option<String>,
    /// 外部コマンドの実行上限秒（未指定は60秒）。暴走・ストール対策。
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    // --- v1.5 フィルタ設定ダイアログ（新形式）。name があれば新形式として解釈 ---
    /// プログラムのファイル名（例: sp2tab.exe）
    #[serde(default)]
    pub name: Option<String>,
    /// プログラムの場所。空=「.texos/filters → 環境変数PATH」で name を探す。
    /// 絶対パス（ファイル選択ダイアログ由来）は最優先
    #[serde(default)]
    pub path: Option<String>,
    /// 実行形式: "filter"（< {in} > {out}）| "command"（引数で {in} {out}）| "generator"（入力不要）
    #[serde(default, rename = "type")]
    pub filter_type: Option<String>,
    /// 想定する入力拡張子（空=制限なし）。合わない入力はスキップ報告
    #[serde(default)]
    pub in_exts: Option<Vec<String>>,
    /// 出力先フォルダ。空=プロジェクト直下にコミット済み実身／指定=そこへ書き出して窓（仮身）
    #[serde(default)]
    pub out_dir: Option<String>,
    /// 出力名の接頭辞（空=従来の「入力名_フィルタ名」命名）。
    /// v1.5.7 セレクタ設定: セレクタでは「下流に送る出力名」— 採用した枝のペイロードの
    /// stem をこの名前に差し替える（"fibo.exe" のように拡張子付きなら ext も変える）。
    #[serde(default)]
    pub prefix: Option<String>,
    /// 接頭辞使用時の次の連番。実行のたびにカードへ書き戻す
    #[serde(default)]
    pub seq: Option<u32>,
    /// v1.5.7 セレクタ設定: エラー時メッセージ。失敗時矢印の枝を採用したとき、枝が運ぶ
    /// エラーテキストの代わりにこの文を text/plain で下流へ送る（空=そのままスルー）。
    #[serde(default)]
    pub fail_text: Option<String>,
    /// ユーザーメモ
    #[serde(default)]
    pub note: Option<String>,
    /// v1.3.11 対話シェル: "cmd" | "powershell"。設定されていれば run_flow は捕捉せず
    /// 対話ウィンドウを detached で開き、cmd（一行目のコマンド・{in}可）を実行して継続する。
    #[serde(default)]
    pub shell: Option<String>,
    /// シェルカード統合（2026-07-06）: ウィンドウモード。"keep"（既定・実行後も開いたまま
    /// = cmd /K・powershell -NoExit）| "close"（ウィンドウで実行して閉じる）|
    /// "hidden"（ウィンドウなし・stdout/stderr を捕捉して結果パネルに表示）。
    /// 未指定は "keep"（既存カードの挙動を変えない）。
    #[serde(default)]
    pub window: Option<String>,
    // --- v1.4.0 AIフィルタ（builtin:"ai"）。デュアルプロバイダ設計 ---
    /// "ollama"（既定・ネイティブ /api/chat）| "lmstudio"（v1.7.3・LM Studio のローカル
    /// サーバ）| "openai"（OpenAI準拠 /v1/chat/completions・任意のサーバ）。
    /// lmstudio と openai はワイヤ形式が同一（is_openai_compat）— 違うのは既定
    /// エンドポイントと案内文、そして到達確認を自動でやるか否かだけ。
    #[serde(default)]
    pub provider: Option<String>,
    /// API ベースURL。空=provider既定（ollama: http://localhost:11434 /
    /// lmstudio・openai: http://localhost:1234/v1）
    #[serde(default)]
    pub endpoint: Option<String>,
    /// モデル名（例: qwen2.5:3b）
    #[serde(default)]
    pub model: Option<String>,
    /// システムプロンプト（空可）
    #[serde(default)]
    pub system: Option<String>,
    /// 生成温度（未指定はモデル既定にまかせる）
    #[serde(default)]
    pub temperature: Option<f64>,
    /// v1.4.0 md_codeblock: 抽出するコードブロックの言語（空=最初のブロック）。
    #[serde(default)]
    pub lang: Option<String>,
    /// v1.4.1 AI動作モード: "transform"（一発変換・既定）| "chat"（対話パネル）。
    #[serde(default)]
    pub mode: Option<String>,
    /// v1.4.5 チャットのMDログ保存名（空=「<ラベル>のログ.md」）。カード毎に変えれば
    /// 同名カードでもログが衝突しない。
    #[serde(default)]
    pub log_name: Option<String>,
    /// v1.8.6 AI: GPU の使い方。"auto"（既定・キー無し＝サーバ任せ）|
    /// "max"（全層を GPU へ強制＝Ollama options.num_gpu=999。VRAM に載らなければ
    /// Ollama 側がロードに失敗するので、載る自信があるときだけ）|
    /// "cpu"（GPU を使わない＝num_gpu=0）。
    /// Ollama ネイティブ経路のみ有効 — OpenAI互換API に層割り当ての標準パラメータは無く、
    /// LM Studio 等はサーバ側の設定で決まる。
    #[serde(default)]
    pub gpu: Option<String>,
    /// v1.8.6 AI: Thinking（思考）モード。"off"（既定・キー無し）| "on" |
    /// "low" | "medium" | "high"（レベルを解釈できないモデルでは on 相当に丸められる）。
    /// Ollama は /api/chat の think へ渡し、思考は message.thinking で分けて届く。
    /// OpenAI互換はサーバによって delta.reasoning_content で来るか本文に
    /// <think>…</think> で混ざるかが分かれるため、両方を受けて分離する。
    #[serde(default)]
    pub think: Option<String>,
    /// v1.9.2 AI: 汎用 OpenAI互換サーバ（provider="openai"）へ思考の指定を reasoning_effort で
    /// 送るか。既定 false＝送らない（未知のパラメータや値で 400 を返すサーバがあるため）。
    /// 送るときは 使わない→"none"・浅く/中/深く→low/medium/high・使う→送らない（サーバ既定）。
    /// LM Studio / Ollama には効かない（あちらは能力から決まる — plan_think）。
    #[serde(default)]
    pub send_reasoning: bool,
    /// v1.9.2 AI: 追加パラメータ。リクエスト本体のトップレベルへそのままマージする
    /// JSON オブジェクト（例: Strata の {"reasoning_budget_tokens": 8000}）。全サーバ共通。
    /// 他の欄が管理するキーは拒否する（check_extra_params）＝送る値の出どころを1つに保つ。
    #[serde(default)]
    pub extra_params: Option<serde_json::Map<String, serde_json::Value>>,
    /// v1.8.9 AI: 実行時のコンテキスト長（Ollama の options.num_ctx）。未指定＝キーを送らない
    /// ＝サーバ既定（学習上限よりずっと小さい）。モデルの学習上限（/api/show の
    /// context_length）とは別物。Ollama ネイティブ経路のみ有効（LM Studio はロード時に決まる）。
    #[serde(default)]
    pub num_ctx: Option<u32>,
    /// v1.4.9 録音モード（ジェネレータ）: true なら stdin をパイプで開いたまま起動し、
    /// 子プロセスへ制御文字列（ffmpeg の 'q' 等）を送れるようにする（汎用 stdin チャネル）。
    /// 録音は自分で終わらない（-t 不要）ので「停止」で 'q' を送って正常終了させ WAV を確定する。
    #[serde(default)]
    pub record: bool,
    // --- v1.5 Text-To-Image（builtin:"t2i"）。AIフィルタと同系統の「データ変換子」。
    //     入力テキスト = プロンプト → diffusers 常駐デーモン（t2i_server.py）→ PNG。
    //     `model` は AI と共用（ここでは SD モデルの friendly name = "sd1.5" 等）。
    //     `width` は img_resize と共用（画像幅）。以下は t2i 専用の追加パラメータ。---
    /// 負プロンプト（除外したい要素）。
    #[serde(default)]
    pub negative: Option<String>,
    /// 推論ステップ数（未指定は 25）。
    #[serde(default)]
    pub steps: Option<u32>,
    /// ガイダンススケール（CFG。未指定は 7.5）。
    #[serde(default)]
    pub guidance: Option<f64>,
    /// 画像の高さ（未指定は 512。幅は既存の `width` を共用）。
    #[serde(default)]
    pub height: Option<u32>,
    /// 乱数シード（未指定/空＝毎回ランダム）。再現したいとき固定する。
    #[serde(default)]
    pub seed: Option<i64>,
    /// img2img の変化度（0=元画像のまま〜1=大きく変える。未指定は 0.6）。元画像入力時のみ有効。
    #[serde(default)]
    pub strength: Option<f64>,
    // --- v1.5.x モディファイア（SD/SDXL 中心）。デーモンが要求状態へリコンサイルする。---
    /// LoRA 指定。"name:weight, name2" の自由記述（重み省略は 1.0）。ファイルは home/loras に配置。
    #[serde(default)]
    pub lora: Option<String>,
    /// VAE 名（home/vae に置いたファイル/フォルダ名。空＝モデル既定 VAE）。
    #[serde(default)]
    pub vae: Option<String>,
    /// サンプラー名（dpm++_2m / dpm++_2m_karras / euler_a / unipc / ddim 等。空＝既定 DPM++）。
    #[serde(default)]
    pub sampler: Option<String>,
    /// Clip skip（A1111 流。1=スキップなし・2=penultimate。SD/SDXL のみ有効）。
    #[serde(default)]
    pub clip_skip: Option<u32>,
    // --- ComfyUI バックエンド（docs/ComfyUIバックエンド設計.md）。`endpoint` は AI と共用
    //     （空＝http://127.0.0.1:8188）。lora/vae/sampler/clip_skip はワークフロー側の責務で使わない。---
    /// t2i の生成バックエンド。未指定 / "diffusers"＝自前デーモン、"comfyui"＝外部 ComfyUI。
    #[serde(default)]
    pub backend: Option<String>,
    /// ComfyUI の API 形式ワークフロー JSON（UI 形式は取り込み時に拒否）。
    #[serde(default)]
    pub comfy_workflow: Option<serde_json::Value>,
    /// 差し込み口（slot → ノード/入力）。
    #[serde(default)]
    pub comfy_bindings: Option<Vec<crate::comfy::ComfyBinding>>,
    /// 画像を取り出す出力ノード ID（空＝outputs の最初の画像）。
    #[serde(default)]
    pub comfy_output: Option<String>,
    /// v1.4.17 インラインパネルの追加プロンプト。接続テキスト入力の後ろに ", " で連結される。
    /// 入力が無ければこれ単独がプロンプトになる（パネルだけで生成できる）。
    #[serde(default)]
    pub prompt: Option<String>,
    /// v1.5.3 WS-P1（オペ層 §5.4.2）: no-data の振る舞い。"propagate"(既定=キー無し)＝
    /// 入力に no-data が1つでもあれば自分も no-data（NULL伝搬・実行しない）／
    /// "absorb"＝欠けは欠けたまま、返ってきたものだけで実行（統合系）。
    #[serde(default)]
    pub nodata: Option<String>,
    // --- v1.7 P1: web_fetch（Webの実身化。V1.7_情報収集経路計画.md §3）---
    /// web_fetch: パラメータ欄の URL 直書き。テキスト実身を入力接続したときは入力が優先。
    #[serde(default)]
    pub url: Option<String>,
    /// web_fetch: プライベートアドレス遮断（§4-5）の開発者向け解除。ダイアログには
    /// 出さない（「JSONで編集」で設定する機微キー）。
    #[serde(default)]
    pub allow_private: bool,
    // --- v1.7 P3: web_feed（RSS/Atom → 記事カード群）---
    /// web_feed: 取り込む最大件数（新しい方から。未指定は10・0 = 全件）。
    /// web_search: 検索語1つあたりの結果件数（未指定は10・1〜50 に丸める）。
    #[serde(default)]
    pub limit: Option<u32>,
    // --- Web検索 P2: web_search（検索語 → SearXNG → 結果一覧 Markdown。
    //     docs/Web検索_SearXNG計画.md §3）---
    /// web_search: パラメータ欄の検索語（1行=1検索語）。入力を接続したときは入力が優先。
    #[serde(default)]
    pub query: Option<String>,
    /// web_search: 使う検索元（カンマ区切り。SearXNG のエンジン名）。未指定は既定の組。
    #[serde(default)]
    pub engines: Option<String>,
    /// web_search: 分類（general / news / it / science）。未指定は general。
    #[serde(default)]
    pub categories: Option<String>,
    /// web_search: 言語（ja / en / all …）。未指定は ja。
    #[serde(default)]
    pub language: Option<String>,
    /// web_search: 期間（day / week / month / year）。未指定・空は絞らない。
    #[serde(default)]
    pub time_range: Option<String>,
    /// web_search: 1回の実行で検索する行数の上限＝**ヒューズ**（AI が大量の行を出しても
    /// その回数は叩かない）。未指定は3・1〜10 に丸める。
    #[serde(default)]
    pub max_queries: Option<u32>,
    /// v1.7 P5 to_url: カード化するリンクの種類（実機FB: 全部拾うととんでもない枚数になる）。
    /// "all"（既定）| "page"（HTMLページ）| "image"（画像直リンク）| "media"（音声・動画・
    /// PDF などのファイル直リンク）。分類は URL 末尾の拡張子ベース。
    #[serde(default)]
    pub links: Option<String>,
    /// v1.7 P5 to_url: カード名の付け方（実機FB: 全カード同名で意味不明になる）。
    /// "fetch"（既定: ページはリンク先の <title> を軽量取得・画像等はURLのファイル名）|
    /// "none"（取得しない: URLのファイル名／単一URLなら入力カード名を引き継ぐ）。
    #[serde(default)]
    pub titles: Option<String>,
    // --- v2.0 t23d（builtin:"t23d"）: Image-To-3D（V2.0_t23d設計.md）。
    //     model/steps/guidance/seed/timeout_secs は t2i と共用。以下は t23d 専用。---
    /// t23d: octree 解像度（メッシュの細かさ。未指定は 256）。
    #[serde(default)]
    pub octree: Option<u32>,
    /// t23d: "on" でテクスチャ付き生成（既定=形状のみ）。文字列なのはフォーム select の
    /// 値をそのまま保存するため（bool にすると select 由来の "on"/"off" が読めない）。
    #[serde(default)]
    pub texture: Option<String>,
    /// t23d: テクスチャ時の面数削減（FaceReducer）上限。未指定は 40000。
    #[serde(default)]
    pub face_count: Option<u32>,
    // --- v1.8 ④反復カード（builtin:"loop"）: 上流サブグラフを条件が満たされるまで
    //     引き直す（V1.8_記法カード計画.md §4 案B＝DAG を保つ）。---
    /// 反復の上限周回数＝**ヒューズ**（V2.0課題 §8-3「反復は max_fires 相当必須」）。
    /// 未指定は3。1〜`LOOP_MAX_ROUNDS` に丸めるので、壊れた値でも暴走しない。
    #[serde(default)]
    pub max_rounds: Option<u32>,
    /// 停止条件の判定モード（`judge_text` の mode）。未指定＝回数だけで止める。
    /// 判定対象はその周の出力テキスト全体、パターンは `pattern` を共用（text_grep と同じ欄）。
    #[serde(default)]
    pub until: Option<String>,
    // --- v1.8 ③問い合わせカード（builtin:"ask"）---
    /// 人に聞くこと。答え付箋の頭に `>` 引用で書き込まれ、キャンバスから読める。
    #[serde(default)]
    pub question: Option<String>,
    /// `"on"` で「**答えが入っていたら上流を動かさない**」。
    ///
    /// 既定（未指定）は上流を評価してから答えを見る＝no-data 伝搬などの意味論が
    /// 他のカードと完全に揃う。ただし AI に原案を書かせて人に聞く形では、2パス目に
    /// **原案が作り直されて人が読んだ案と別物になる**。それを止めるためのスイッチ。
    /// 人がもう判断した後なら、材料を引き直す意味はない、という考え方。
    #[serde(default)]
    pub skip_upstream: Option<String>,
    /// v1.8.7 途中出力スイッチ（`"on"`）: パイプラインの途中で評価されたときも、出力を
    /// 終端と同じ規則（prefix／seq／out_dir）でキャンバスへ着地させる。下流へは従来どおり
    /// メモリで流す。試行錯誤で「ソースと exe は手元に欲しいが、記録カードを挟むのは面倒」
    /// という場面のためのカード単位の正式スイッチ（記録カードを内蔵した形）。
    #[serde(default)]
    pub emit_mid: Option<String>,
}

/// v1.8 ③: 答え付箋の「質問行」の目印。`>` で始まる行は**質問であって答えではない**ので、
/// 答えを読むときに落とす。Markdown の引用と同じ記号なので、付箋のまま読んでも自然に見える。
pub const ASK_QUOTE: &str = ">";

/// 答え付箋の本文から**人が書いた答えだけ**を取り出す（`>` 行＝質問を落として trim）。
pub fn answer_text(note: &str) -> String {
    note.lines()
        .filter(|l| !l.trim_start().starts_with(ASK_QUOTE))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// 反復カードのヒューズ上限（spec の `max_rounds` はこの値に丸められる）。
/// AI を何周も回すので、設定ミスや壊れた JSON で無限に走らせないための天井。
pub const LOOP_MAX_ROUNDS: u32 = 50;

/// v1.8: テキスト1本に対する**決定的な判定**（V2.0課題 §8-2 の判定2段のうち①）。
///
/// 反復カード（`until`）の停止判定に使い、将来のルーターカード（`builtin:"route"`）の
/// 枝選択にもそのまま使えるように、spec にもノードにも依存しない純関数にしてある。
/// ②の AI 分類は「同じ入口・違う判定器」として後から足す（§8-2）。
///
/// mode: `"contains"` | `"not_contains"` | `"regex"` | `"not_regex"` | `"empty"` | `"not_empty"`。
/// `empty` 系はパターンを見ない（空白のみも空とみなす）。未知のモードはエラー
/// （黙って false にすると「止まらない反復」になり、原因が見えないため）。
pub fn judge_text(mode: &str, pattern: &str, text: &str) -> Result<bool, String> {
    match mode.trim() {
        "contains" => Ok(text.contains(pattern)),
        "not_contains" => Ok(!text.contains(pattern)),
        "regex" | "not_regex" => {
            let re = regex::Regex::new(pattern).map_err(|e| format!("正規表現が不正です: {e}"))?;
            let hit = re.is_match(text);
            Ok(if mode.trim() == "regex" { hit } else { !hit })
        }
        "empty" => Ok(text.trim().is_empty()),
        "not_empty" => Ok(!text.trim().is_empty()),
        other => Err(format!("未知の判定モード: {other}")),
    }
}

/// v1.5.3 WS-P1（オペ層 §5.4.2）: フロー矢印の制御属性（relations.attrs の JSON）。
/// NULL/キー無し = 従来（無条件・接続順）＝完全後方互換。
#[derive(Deserialize, Default, Debug, Clone)]
pub struct EdgeAttrs {
    /// 通過条件: "success" | "failure" | 無し="always"。上流の**ジョブ内評価結果**を読む。
    #[serde(default)]
    pub when: Option<String>,
    /// 枝が取られたとき運ぶもの: "stderr"(失敗枝の既定) | "input" | "none"。
    /// 成功枝の既定は上流出力（キー無し＝従来どおり）。
    #[serde(default)]
    pub carry: Option<String>,
    /// IN 順の明示（1始まり）。無し = created_at 接続順（port 昇順 → created_at で安定）。
    #[serde(default)]
    pub port: Option<i64>,
    /// v1.8 役割ポート: この矢印が運ぶものの**意味**。無し＝ふつうの入力（従来）。
    /// `"system"` = 上流の中身を AI カードの**システムプロンプト**として渡す。
    /// 役割つきの矢印は通常入力に数えられない（＝ファンアウトを起こさない）。
    /// 狙いは「AI カードの一番重要な中身＝役割を、面から見える付箋にする」こと。
    ///
    /// `"previous"` = v1.8 ④反復カードの**戻り矢印**（反復カード → 上流のどれか）。
    /// これは通常入力に数える（材料だから）が、**評価では絶対に遡らない**——値は実行時に
    /// 反復カードが文脈へ置いた「前周の出力」から取る。おかげでキャンバスには輪が見えるのに
    /// 評価は DAG のまま＝循環判定にも pull 解決にも触らない（計画 §4 案B）。
    /// 1周目は前周が無いので**空**を運ぶ（no-data ではない＝下流を止めない）。
    ///
    /// `"answer"` = v1.8 ③問い合わせカードの**答え箱**（付箋 → 問い合わせカード）。
    /// `"system"` と同じく通常入力に数えない。付箋が空なら「まだ答えていない」。
    #[serde(default)]
    pub role: Option<String>,
}

/// relations.attrs の JSON を読む（NULL・壊れた JSON は既定＝従来挙動）。
pub fn parse_edge_attrs(s: Option<&str>) -> EdgeAttrs {
    s.and_then(|t| serde_json::from_str(t).ok()).unwrap_or_default()
}

/// v1.4.10 リファクタリング（§4.2 ノード種別の体系化）: いままで eval_filter / run_flow に
/// 散在していた「このカードはどう実行するか」の判定（`filter_type=="generator"`・`builtin`
/// 有無・`builtin=="ai"`・`name`/`cmd` 有無）を1つの列挙に集約する。FilterSpec の各フィールドは
/// そのまま（serde 後方互換を維持）。判別だけをここへ寄せ、execute_node が一本道で分岐できるようにする。
///
/// 注意（挙動不変の要）: 対話シェル（`shell`）は終端カードのときだけ run_flow が特別扱いする
/// 概念で、eval_filter の連鎖評価では一切見ない。そのため NodeKind には Shell を含めない
/// （= execute_node 側の分類は従来の eval_filter と完全一致）。シェル判定は run_flow が
/// `FilterSpec::is_shell()` で行う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    /// 入力を取らず1回実行してファイルを生む外部プログラム（`type:"generator"`）。
    /// `record` 指定なら stdin チャネルを開いたまま起動する（録音の停止制御）。
    Generator,
    /// builtin:"ai" — ネットワーク経由の LLM 変換（同期 blocking・別経路）。
    Ai,
    /// builtin:"t2i" — 常駐 diffusers デーモン経由の画像生成（テキスト→PNG・別経路）。
    T2i,
    /// builtin:"t23d" — 常駐 Hunyuan3D-2 デーモン経由の 3D 生成（画像→glb・v2.0 前倒し）。
    T23d,
    /// builtin:"select" — セレクタ（v1.5.3 WS-P1・オペ層 §5.4.1）。実行はせず port 順の
    /// first-match で「最初に有データを返した枝」を採用する。no-data 吸収を内包。
    /// eval_filter 側で完結し execute_node には到達しない。
    Select,
    /// その他の内蔵フィルタ（text_grep / text_replace / img_* / md_codeblock …）。
    Builtin,
    /// 外部コマンド／プログラム（{in}/{out} 展開）。集約か個別かは is_aggregate() で。
    External,
    /// builtin も cmd も name も無い不正な定義。
    Invalid,
}

impl FilterSpec {
    /// このカードの実行種別を判定する。判定順序は現行 eval_filter の優先順位を保つ:
    /// generator（`type`）→ builtin（`ai` を含む）→ external（`name`/`cmd`）→ invalid。
    pub fn node_kind(&self) -> NodeKind {
        if self.filter_type.as_deref() == Some("generator") {
            return NodeKind::Generator;
        }
        if let Some(b) = self.builtin.as_deref() {
            return match b {
                "ai" => NodeKind::Ai,
                "t2i" => NodeKind::T2i,
                "t23d" => NodeKind::T23d,
                "select" => NodeKind::Select,
                _ => NodeKind::Builtin,
            };
        }
        if self.name.is_some() || self.cmd.is_some() {
            return NodeKind::External;
        }
        NodeKind::Invalid
    }

    /// 終端カードを対話シェルとして開くか（run_flow 専用。連鎖評価では使わない）。
    pub fn is_shell(&self) -> bool {
        self.shell.is_some()
    }

    /// 集約実行（全入力を集めて1回）か。External は CMD の書き方で決まる: {in*} か {in2}
    /// 以降があれば集約、{in}/{in1} のみなら入力ごとに実行（ファンアウト）。
    /// v1.5.17 P2: Builtin にも集約系（text_concat / text_diff / text_paste）を導入 —
    /// こちらは builtin 名で静的に決まる（入力順は既存契約: port 昇順 → 接続順）。
    pub fn is_aggregate(&self) -> bool {
        match self.node_kind() {
            NodeKind::External => cmd_is_aggregate(self.cmd.as_deref().unwrap_or("")),
            NodeKind::Builtin => {
                builtin_is_aggregate(self.builtin.as_deref().unwrap_or(""))
            }
            _ => false,
        }
    }

    /// 出力ファイル（`{out}` / `{out_noext}`）を書かない外部コマンドか。カメラ起動
    /// （`start microsoft.windows.camera:`）や読み上げのような「呼ぶだけ」で完結する
    /// fire-and-forget コマンドで、成功しても取り込む出力が無い（＝生成カードを作らず、
    /// 「出力がありません」をエラーにしない）。CMD に `{out` を書けば従来どおり出力必須。
    pub fn is_self_contained(&self) -> bool {
        // v1.5.15 実機FB: speak など「呼ぶだけ」で完結する builtin も出力ゼロが正常
        // （終端の「入力がありません」案内の対象外にする）。
        // v1.6: import_new（新規カードを登録）はカードを自分で作る＝出力ペイロードなしが正常。
        // v1.10: voice_send（会話へ送る）も渡すだけで出力なし。
        if matches!(self.builtin.as_deref(), Some("speak") | Some("import_new") | Some("voice_send")) {
            return true;
        }
        matches!(self.node_kind(), NodeKind::Generator | NodeKind::External)
            && !self.cmd.as_deref().unwrap_or("").contains("{out")
    }

    /// v1.4.17 t2i インラインパネルの追加プロンプト（空白のみは無いのと同じ）。
    pub fn own_prompt(&self) -> Option<&str> {
        self.prompt.as_deref().map(str::trim).filter(|s| !s.is_empty())
    }

    /// v1.5.3 WS-P1: no-data を吸収するノードか（spec の absorb フラグ or セレクタ）。
    /// 既定は伝搬型＝入力に no-data があれば自分も no-data（オペ層 §5.1-4）。
    pub fn absorbs_nodata(&self) -> bool {
        self.nodata.as_deref() == Some("absorb") || self.node_kind() == NodeKind::Select
    }
}

/// v1.5.7 セレクタ設定: 出力名を stem と拡張子に分ける。"fibo.exe" → ("fibo", Some("exe"))。
/// 拡張子とみなすのは最後の '.' 以降が 1〜8 文字の英数字のときだけ（"v1.5" や末尾ドットは
/// 名前の一部として全体を stem に）。ユーザーが拡張子込みで書いても素直に通すための寛容化。
pub fn split_output_name(name: &str) -> (&str, Option<&str>) {
    if let Some((stem, ext)) = name.rsplit_once('.') {
        let ok = !stem.is_empty()
            && (1..=8).contains(&ext.len())
            && ext.chars().all(|c| c.is_ascii_alphanumeric());
        if ok {
            return (stem, Some(ext));
        }
    }
    (name, None)
}

/// 外部プログラムの解決: 絶対パス → <project>/.texos/filters → 環境変数PATH の順
/// （v1.5 確定方針: 明示パスが見つからなくても後段へフォールバックして探す）。
pub fn resolve_program(
    name: &str,
    path: Option<&str>,
    filters_dir: &std::path::Path,
) -> Option<std::path::PathBuf> {
    if let Some(p) = path.map(str::trim).filter(|p| !p.is_empty()) {
        let pb = std::path::PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
        // フォルダ指定なら name を結合して試す
        let joined = pb.join(name);
        if joined.is_file() {
            return Some(joined);
        }
    }
    let bundled = filters_dir.join(name);
    if bundled.is_file() {
        return Some(bundled);
    }
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|d| d.join(name))
        .find(|c| c.is_file())
}

pub fn parse_spec(json: &str) -> Result<FilterSpec, String> {
    serde_json::from_str(json).map_err(|e| format!("フィルタ定義(JSON)が不正です: {e}"))
}

/// 接続時型チェック。front 側 model.ts の mimeMatches と同じ規則を保つこと:
/// 空・"*/*" は全受け入れ / "text/*" は text/ 系＋JSON / その他は前方一致 or 完全一致。
pub fn mime_matches(accept: &str, mime: &str) -> bool {
    if accept.is_empty() || accept == "*/*" {
        return true;
    }
    if let Some(prefix) = accept.strip_suffix("/*") {
        if prefix == "text" && mime == "application/json" {
            return true;
        }
        return mime.starts_with(&format!("{prefix}/"));
    }
    accept == mime
}

/// v1.5.15 L2 OSアダプタ・ビルトイン第1弾「speak」（標準フィルタ充実検討.md §4）:
/// テキストを OS の読み上げ機能に渡す fire-and-forget — 出力カードは作らない
/// （{out} を書かない自己完結コマンドと同じ契約。実行アームは flow_run.rs 側で分岐）。
/// OS 差はこの関数の内側に閉じ込める（カード定義は全 OS 共通＝プロジェクトの可搬性）。
/// 本文はコマンドラインに載せず UTF-8 一時ファイルで渡す（引用符・長さ・文字化けを回避）。
pub fn run_speak(input: &[u8]) -> Result<(), String> {
    let text = String::from_utf8_lossy(input);
    if text.trim().is_empty() {
        return Err("読み上げるテキストが空です".into());
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path =
        std::env::temp_dir().join(format!("texos_speak_{}_{nanos}.txt", std::process::id()));
    std::fs::write(&path, text.as_bytes())
        .map_err(|e| format!("読み上げ用の一時ファイルを書けません: {e}"))?;
    if let Err(e) = spawn_speaker(&path) {
        let _ = std::fs::remove_file(&path);
        return Err(e);
    }
    Ok(())
}

/// Windows: SAPI（COM の SpVoice）。読み上げ後の一時ファイル削除もスクリプト側で行う。
/// 音声は Windows 設定（時刻と言語 > 音声）で選んだ既定音声に追従する（v1.5.15 実機FB）:
/// 設定画面は OneCore 側の DefaultTokenId を書くが、SAPI/System.Speech の既定は
/// 旧 sapi.cpl 設定を見るため切り替えが効かない。さらに Windows PowerShell 5.1 の
/// System.Speech は Desktop 系音声しか列挙しないため名前照合も不可 —
/// **OneCore のトークンを SpObjectToken.SetId で直接 SpVoice に挿す**のが確実
/// （PS 5.1 実機で "Microsoft Ayumi - Japanese (Japan)" の選択を確認済み）。
/// OneCore キーが無い等の環境では従来の既定音声のまま（try/catch で黙って続行）。
#[cfg(windows)]
fn spawn_speaker(path: &std::path::Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let p = path.to_string_lossy().replace('\'', "''");
    let script = format!(
        "$v = New-Object -ComObject SAPI.SpVoice; \
         try {{ \
           $id = (Get-ItemProperty 'HKCU:\\Software\\Microsoft\\Speech_OneCore\\Voices' -ErrorAction Stop).DefaultTokenId; \
           if ($id) {{ \
             $tok = New-Object -ComObject SAPI.SpObjectToken; \
             $tok.SetId($id, 'HKEY_LOCAL_MACHINE\\SOFTWARE\\Microsoft\\Speech_OneCore\\Voices', $false); \
             $v.Voice = $tok \
           }} \
         }} catch {{}}; \
         $t = [IO.File]::ReadAllText('{p}', [Text.Encoding]::UTF8); \
         $v.Speak($t) | Out-Null; \
         Remove-Item -LiteralPath '{p}'"
    );
    std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW（コンソールを出さない）
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("読み上げを起動できません: {e}"))
}

/// macOS: 標準搭載の say。一時ファイルは OS の掃除に任せる（小さなテキスト）。
#[cfg(target_os = "macos")]
fn spawn_speaker(path: &std::path::Path) -> Result<(), String> {
    std::process::Command::new("say")
        .arg("-f")
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("読み上げを起動できません: {e}"))
}

/// Linux 等: espeak。未導入なら spawn 失敗がそのままエラーになる（黙って失敗しない）。
#[cfg(all(unix, not(target_os = "macos")))]
fn spawn_speaker(path: &std::path::Path) -> Result<(), String> {
    std::process::Command::new("espeak")
        .arg("-f")
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("読み上げを起動できません（espeak の導入が必要です）: {e}"))
}

/// 内蔵フィルタの実行: 入力バイト列 → (出力バイト列, 出力拡張子, 出力MIME)。
pub fn run_builtin(
    spec: &FilterSpec,
    name: &str,
    input: &[u8],
) -> Result<(Vec<u8>, String, String), String> {
    match name {
        "text_grep" => {
            let pat = spec.pattern.as_deref().unwrap_or("");
            let re = regex::Regex::new(pat).map_err(|e| format!("正規表現が不正です: {e}"))?;
            let text = String::from_utf8_lossy(input);
            let out: String = text
                .lines()
                .filter(|l| re.is_match(l))
                .collect::<Vec<_>>()
                .join("\n");
            Ok((out.into_bytes(), "txt".into(), "text/plain".into()))
        }
        "text_replace" => {
            let pat = spec.pattern.as_deref().unwrap_or("");
            let re = regex::Regex::new(pat).map_err(|e| format!("正規表現が不正です: {e}"))?;
            let rep = spec.replacement.as_deref().unwrap_or("");
            let text = String::from_utf8_lossy(input);
            let out = re.replace_all(&text, rep).into_owned();
            Ok((out.into_bytes(), "txt".into(), "text/plain".into()))
        }
        "img_resize" => {
            // 幅指定・アスペクト維持。出力はPNGに統一（劣化なし・確実にエンコード可能）。
            let w = spec.width.unwrap_or(800).max(1);
            let img = image::load_from_memory(input)
                .map_err(|e| format!("画像を読めません: {e}"))?;
            let resized = img.resize(w, u32::MAX, image::imageops::FilterType::Lanczos3);
            let mut buf = std::io::Cursor::new(Vec::new());
            resized
                .write_to(&mut buf, image::ImageFormat::Png)
                .map_err(|e| e.to_string())?;
            Ok((buf.into_inner(), "png".into(), "image/png".into()))
        }
        "img_convert" => {
            let fmt_name = spec.format.as_deref().unwrap_or("png").to_lowercase();
            let (fmt, ext, mime) = match fmt_name.as_str() {
                "png" => (image::ImageFormat::Png, "png", "image/png"),
                "jpeg" | "jpg" => (image::ImageFormat::Jpeg, "jpg", "image/jpeg"),
                "webp" => (image::ImageFormat::WebP, "webp", "image/webp"),
                "bmp" => (image::ImageFormat::Bmp, "bmp", "image/bmp"),
                "gif" => (image::ImageFormat::Gif, "gif", "image/gif"),
                other => return Err(format!("未対応の出力形式: {other}")),
            };
            let img = image::load_from_memory(input)
                .map_err(|e| format!("画像を読めません: {e}"))?;
            let mut buf = std::io::Cursor::new(Vec::new());
            if fmt == image::ImageFormat::Jpeg {
                // JPEG はアルファ非対応 → RGB に落としてからエンコード
                image::DynamicImage::ImageRgb8(img.to_rgb8())
                    .write_to(&mut buf, fmt)
                    .map_err(|e| e.to_string())?;
            } else {
                img.write_to(&mut buf, fmt).map_err(|e| e.to_string())?;
            }
            Ok((buf.into_inner(), ext.into(), mime.into()))
        }
        "md_codeblock" => {
            // v1.4.0: Markdown のフェンス付きコードブロック(```lang ... ```)から中身を取り出す。
            // AI出力(.md)を gcc 等のコンパイラへ渡す前処理。lang 指定で言語を絞り込み、
            // 無指定なら最初のブロック。複数ブロックは最初の1つを採用（Phase1）。
            let text = String::from_utf8_lossy(input);
            let want = spec
                .lang
                .as_deref()
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty());
            let mut in_block = false;
            let mut cur_lang = String::new();
            let mut cur: Vec<&str> = Vec::new();
            let mut found: Option<(String, String)> = None;
            for line in text.lines() {
                let t = line.trim_start();
                if !in_block {
                    if let Some(rest) = t.strip_prefix("```") {
                        in_block = true;
                        cur_lang = rest.trim().to_lowercase();
                        cur.clear();
                    }
                } else if t.starts_with("```") {
                    in_block = false;
                    let ok = want.as_ref().map(|w| &cur_lang == w).unwrap_or(true);
                    if ok && found.is_none() {
                        found = Some((cur_lang.clone(), cur.join("\n")));
                    }
                } else {
                    cur.push(line);
                }
            }
            let (lang, mut code) = found.ok_or_else(|| match &want {
                Some(w) => format!("`{w}` のコードブロックが見つかりません"),
                None => "コードブロック(```...```)が見つかりません".to_string(),
            })?;
            if !code.is_empty() && !code.ends_with('\n') {
                code.push('\n'); // 末尾改行（コンパイラ・POSIXツールが好む）
            }
            // 拡張子: output_ext 指定 > 言語からの推定 > 言語トークン > txt
            let ext = spec
                .output_ext
                .clone()
                .map(|e| e.trim().trim_start_matches('.').to_string())
                .filter(|e| !e.is_empty())
                .unwrap_or_else(|| lang_to_ext(&lang));
            Ok((code.into_bytes(), ext, "text/plain".into()))
        }
        // --- v1.5.12 P1: Unix標準テキストフィルタ（標準フィルタ充実検討 §3）。
        //     text_grep / text_replace と同型（1入力・text/plain 出力・純関数）。---
        "text_head" => {
            let n = spec.lines.unwrap_or(10) as usize;
            let text = String::from_utf8_lossy(input);
            let out = text.lines().take(n).collect::<Vec<_>>().join("\n");
            Ok((out.into_bytes(), "txt".into(), "text/plain".into()))
        }
        "text_tail" => {
            let n = spec.lines.unwrap_or(10) as usize;
            let text = String::from_utf8_lossy(input);
            let all: Vec<&str> = text.lines().collect();
            let out = all[all.len().saturating_sub(n)..].join("\n");
            Ok((out.into_bytes(), "txt".into(), "text/plain".into()))
        }
        "text_sort" => {
            let text = String::from_utf8_lossy(input);
            let mut lines: Vec<&str> = text.lines().collect();
            if spec.compare.as_deref() == Some("number") {
                // sort -n 相当: 行頭（空白除去後）の数値で比較。数値の無い行は 0 扱い。
                lines.sort_by(|a, b| {
                    leading_number(a)
                        .partial_cmp(&leading_number(b))
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
            } else {
                lines.sort();
            }
            if spec.order.as_deref() == Some("desc") {
                lines.reverse();
            }
            if spec.unique.as_deref() == Some("remove") {
                lines.dedup(); // ソート済みなので隣接 dedup = 全体の重複除去
            }
            let out = lines.join("\n");
            Ok((out.into_bytes(), "txt".into(), "text/plain".into()))
        }
        "text_count" => {
            // wc 相当。mode キーは AI の動作モードと共用（builtin が違えば衝突しない）。
            let text = String::from_utf8_lossy(input);
            let lines = text.lines().count();
            let words = text.split_whitespace().count();
            let chars = text.chars().count();
            let out = match spec.mode.as_deref().unwrap_or("all") {
                "lines" => lines.to_string(),
                "words" => words.to_string(),
                "chars" => chars.to_string(),
                _ => format!("行数: {lines}\n単語数: {words}\n文字数: {chars}"),
            };
            Ok((out.into_bytes(), "txt".into(), "text/plain".into()))
        }
        "text_cut" => {
            let ranges = parse_field_ranges(spec.fields.as_deref().unwrap_or(""))?;
            let delim = spec.delimiter.as_deref().unwrap_or("tab");
            // 出力の区切り: 入力と同じ（空白区切りは列境界が曖昧なのでタブに揃える）。
            let sep = match delim {
                "comma" => ",",
                "tab" | "" | "space" => "\t",
                other => other,
            };
            let text = String::from_utf8_lossy(input);
            let mut out_lines = Vec::new();
            for line in text.lines() {
                let cols: Vec<&str> = match delim {
                    "tab" | "" => line.split('\t').collect(),
                    "comma" => line.split(',').collect(),
                    "space" => line.split_whitespace().collect(),
                    other => line.split(other).collect(),
                };
                let mut picked = Vec::new();
                for &(start, end) in &ranges {
                    let end = end.min(cols.len());
                    if start < end {
                        picked.extend_from_slice(&cols[start..end]);
                    }
                }
                out_lines.push(picked.join(sep));
            }
            let out = out_lines.join("\n");
            Ok((out.into_bytes(), "txt".into(), "text/plain".into()))
        }
        "text_convert" => {
            // nkf 相当（文字コード → UTF-8）。auto は閲覧ビュー（notes::decode_text_bytes）と
            // 同じ手順（BOM → UTF-8厳密 → chardetng 推定）＝表示と変換の結果が一致する。
            let enc_name = spec
                .encoding
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("auto");
            let out = if enc_name.eq_ignore_ascii_case("auto") {
                crate::notes::decode_text_bytes(input).0
            } else {
                let enc = encoding_rs::Encoding::for_label(enc_name.as_bytes()).ok_or_else(
                    || format!("未知の文字コード: {enc_name}（shift_jis / euc-jp / utf-16le 等）"),
                )?;
                let (cow, _, _) = enc.decode(input);
                cow.into_owned()
            };
            Ok((out.into_bytes(), "txt".into(), "text/plain".into()))
        }
        other => Err(format!("未知の内蔵フィルタ: {other}")),
    }
}

/// v1.5.17 P2: 集約実行するビルトインか（全入力を集めて1回実行）。
/// External の cmd_is_aggregate（CMD の書き方で判定）に対し、こちらは名前で静的に決まる。
pub fn builtin_is_aggregate(name: &str) -> bool {
    matches!(name, "text_concat" | "text_diff" | "text_paste")
}

/// v1.5.17 P2: 集約系ビルトインの実行（標準フィルタ充実検討 §3 第2候補）。
/// inputs は全入力のバイト列（並びは既存契約: port 昇順 → 接続順。flow_run eval_filter が保証）。
/// run_builtin と同じ契約で (bytes, ext, mime) を返す純関数。
pub fn run_builtin_agg(
    spec: &FilterSpec,
    name: &str,
    inputs: &[&[u8]],
) -> Result<(Vec<u8>, String, String), String> {
    match name {
        "text_concat" => {
            // cat 相当。既定は「改行境界の保証」（前の入力が改行で終わらなくても行が
            // 融合しない）。"none" が純粋な cat（バイト列をそのまま連結）。
            let sep = spec.separator.as_deref().unwrap_or("newline");
            let mut out = String::new();
            for (i, b) in inputs.iter().enumerate() {
                if i > 0 && sep != "none" {
                    if !out.is_empty() && !out.ends_with('\n') {
                        out.push('\n');
                    }
                    match sep {
                        "blank" => out.push('\n'),
                        // Markdown の水平線は前後に空行が要る（無いと setext 見出し化する）
                        "rule" => out.push_str("\n---\n\n"),
                        _ => {} // "newline" は境界保証のみ
                    }
                }
                out.push_str(&String::from_utf8_lossy(b));
            }
            Ok((out.into_bytes(), "txt".into(), "text/plain".into()))
        }
        "text_diff" => {
            // diff -u 相当。入力1=旧・入力2=新（矢印の接続順）。同一なら出力は空
            //（Unix diff と同じ。「差分なし」は空カードが伝える）。
            if inputs.len() != 2 {
                return Err(format!(
                    "diff は入力がちょうど2つ必要です（現在 {} 個）。接続順が 入力1=旧・入力2=新 になります",
                    inputs.len()
                ));
            }
            let old = String::from_utf8_lossy(inputs[0]);
            let new = String::from_utf8_lossy(inputs[1]);
            let ctx = spec.lines.unwrap_or(3) as usize;
            let out = similar::TextDiff::from_lines(old.as_ref(), new.as_ref())
                .unified_diff()
                .context_radius(ctx)
                .header("入力1（旧）", "入力2（新）")
                .to_string();
            Ok((out.into_bytes(), "diff".into(), "text/plain".into()))
        }
        "text_paste" => {
            // paste 相当: 各入力の同じ行番号を区切り文字で横に結合。行数が違う入力は
            // 空文字で埋める（GNU paste と同じ）。区切りは text_cut と同じ delimiter を共用。
            let sep = match spec.delimiter.as_deref().unwrap_or("tab") {
                "tab" | "" => "\t",
                "comma" => ",",
                "space" => " ",
                other => other,
            };
            let texts: Vec<String> =
                inputs.iter().map(|b| String::from_utf8_lossy(b).into_owned()).collect();
            let columns: Vec<Vec<&str>> = texts.iter().map(|t| t.lines().collect()).collect();
            let rows = columns.iter().map(|c| c.len()).max().unwrap_or(0);
            let mut out_lines = Vec::with_capacity(rows);
            for i in 0..rows {
                let row: Vec<&str> =
                    columns.iter().map(|c| c.get(i).copied().unwrap_or("")).collect();
                out_lines.push(row.join(sep));
            }
            let out = out_lines.join("\n");
            Ok((out.into_bytes(), "txt".into(), "text/plain".into()))
        }
        other => Err(format!("未知の集約ビルトイン: {other}")),
    }
}

/// text_sort の数値比較: 行頭（空白を飛ばした位置）の数値を読む（例 "12.5 kg" → 12.5）。
/// 数値が無い・読めない行は 0 扱い（GNU sort -n と同じ流儀）。
fn leading_number(line: &str) -> f64 {
    let t = line.trim_start();
    let mut end = 0;
    for (i, c) in t.char_indices() {
        if c.is_ascii_digit() || c == '.' || ((c == '-' || c == '+') && i == 0) {
            end = i + c.len_utf8();
        } else {
            break;
        }
    }
    t[..end].parse().unwrap_or(0.0)
}

/// text_cut の列指定（1始まり・"1,3-5" / "2-" / "-3"）を 0始まり半開区間の並びへ。
/// 出力の列順は指定順（awk 流。GNU cut はファイル順だが、並べ替えできる方が便利）。
fn parse_field_ranges(s: &str) -> Result<Vec<(usize, usize)>, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("取り出す列を指定してください（例: 1,3-5）".into());
    }
    let mut out = Vec::new();
    for tok in s.split(',') {
        let tok = tok.trim();
        let bad = || format!("列指定が不正です: {tok}（1始まりの番号か範囲で指定）");
        if let Some((a, b)) = tok.split_once('-') {
            let start = if a.trim().is_empty() {
                1
            } else {
                a.trim().parse::<usize>().map_err(|_| bad())?
            };
            let end = if b.trim().is_empty() {
                usize::MAX
            } else {
                b.trim().parse::<usize>().map_err(|_| bad())?
            };
            if start == 0 || end < start {
                return Err(bad());
            }
            out.push((start - 1, end));
        } else {
            let n = tok.parse::<usize>().map_err(|_| bad())?;
            if n == 0 {
                return Err(bad());
            }
            out.push((n - 1, n));
        }
    }
    Ok(out)
}

/// md_codeblock: フェンスの言語トークンから出力拡張子を推定（未知はトークンをそのまま）。
fn lang_to_ext(lang: &str) -> String {
    match lang {
        "c" => "c",
        "cpp" | "c++" | "cxx" | "cc" => "cpp",
        "rust" | "rs" => "rs",
        "python" | "py" => "py",
        "javascript" | "js" => "js",
        "typescript" | "ts" => "ts",
        "go" | "golang" => "go",
        "java" => "java",
        "kotlin" | "kt" => "kt",
        "ruby" | "rb" => "rb",
        "html" => "html",
        "css" => "css",
        "json" => "json",
        "xml" => "xml",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "sql" => "sql",
        "sh" | "bash" | "shell" | "zsh" => "sh",
        "powershell" | "ps1" => "ps1",
        "bat" | "batch" | "cmd" => "bat",
        "" => "txt",
        other => other,
    }
    .to_string()
}

/// v1.7.4 生成系（ai/t2i/t23d）の制限時間の共通契約:
///   未指定（未記入）＝0 と同じ＝**無制限**。0 でも未記入でも同じ挙動にする
///   （「空欄＝無制限」という UI 表記と実装が食い違っていた実機バグの修正）。
/// 無制限でも止められる: t2i/t23d は中断ボタン →デーモンへ /cancel、AI は接続不能を
/// `GEN_CONNECT_TIMEOUT_SECS` で早期に切る（生成中の停止は中断ボタン＝出力破棄）。
const GEN_CONNECT_TIMEOUT_SECS: u64 = 10;

/// 生成系の制限時間を Duration に落とす（None/Some(0) = 無制限）。
fn gen_timeout(timeout_secs: Option<u64>) -> Option<std::time::Duration> {
    match timeout_secs {
        None | Some(0) => None,
        Some(s) => Some(std::time::Duration::from_secs(s)),
    }
}

/// 生成系の HTTP クライアント生成（制限時間の共通契約＋接続タイムアウト）。
///
/// **落とし穴**: `reqwest::blocking::ClientBuilder` は **timeout の既定が 30 秒**
/// （async の Client は無制限だが blocking だけ違う）。`.timeout()` を呼ばなければ
/// 無制限、ではなく 30 秒で切れる。無制限にするには **`None` を明示的に渡す**必要がある。
/// そのため常に `Option` を渡す（v1.7.4 実機バグ — 空欄でも 30 秒で切れた原因）。
fn gen_client(timeout_secs: Option<u64>) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(GEN_CONNECT_TIMEOUT_SECS))
        .timeout(gen_timeout(timeout_secs)) // None = 無制限（既定 30 秒を明示的に外す）
        .build()
        .map_err(|e| format!("HTTPクライアントの生成に失敗: {e}"))
}

/// v1.4.0 AIフィルタ（builtin:"ai"）: 入力テキストをユーザープロンプトとしてLLMに送り、
/// 応答 Markdown を返す。provider="ollama"（既定・ネイティブ /api/chat）または
/// "lmstudio" / "openai"（OpenAI準拠 /v1/chat/completions・is_openai_compat）。
/// Phase 1 は単発変換（履歴なし）。
///
/// 重要: reqwest::blocking は内部で tokio ランタイムを生成・破棄する。run_flow は
/// #[tauri::command(async)] = tokio ワーカースレッド上で走るため、その非同期文脈の中で
/// blocking クライアントの内部ランタイムを drop すると tokio が
/// 「Cannot drop a runtime in a context where blocking is not allowed」でパニックし、
/// 握っている DB Mutex を poison させてアプリ全体を巻き込む。そこで HTTP は
/// tokio 文脈を持たない素の OS スレッドに逃がして実行する（scope で借用は安全）。
/// v1.5.2 WS-C: この spec が実際に叩く AI エンドポイント（資源セマフォのキー導出用）。
/// run_ai / run_ai_chat_stream の既定値と一致させること（ここが実体の写し）。
/// v1.7.3 OpenAI 準拠 API（POST /chat/completions・SSE / GET /models）で話す provider か。
/// "lmstudio" は LM Studio のローカルサーバで、ワイヤ形式は OpenAI 準拠そのまま。
/// 別名を立てたのは (1) 未起動時に「Developer タブでサーバを起動」と正しく案内でき、
/// (2) preflight で到達確認できる（LM Studio は無認証の /models が health 代わりになる）
/// ため — 汎用 "openai" は認証付き遠隔サーバもあり得るのでどちらも安全にはできない。
pub fn is_openai_compat(provider: &str) -> bool {
    matches!(provider, "openai" | "lmstudio")
}

pub fn effective_ai_endpoint(spec: &FilterSpec) -> String {
    let provider = spec.provider.as_deref().unwrap_or("ollama");
    let explicit = spec
        .endpoint
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match (provider, explicit) {
        (_, Some(e)) => e.to_string(),
        (p, None) if is_openai_compat(p) => "http://localhost:1234/v1".to_string(),
        (_, None) => "http://localhost:11434".to_string(),
    }
}

/// v1.5.2 WS-C: 録音ジェネレータの CMD から dshow の音声デバイス名を抜き出す
/// （`-i audio="マイク (…)"` / `-i audio=NAME`）。デバイス単位の排他セマフォのキー。
pub fn dshow_audio_device(cmd: &str) -> Option<String> {
    let re = regex::Regex::new(r#"audio=(?:"([^"]+)"|([^\s"]+))"#).ok()?;
    let c = re.captures(cmd)?;
    c.get(1).or_else(|| c.get(2)).map(|m| m.as_str().to_string())
}

// --- v1.8.6 GPU の使い方と Thinking（思考）モード ----------------------------------
// 方針: 「載るなら GPU」を既定にする（VRAM 量での CPU 転落はしない — 溢れた層は
// Ollama が自動で CPU へ逃がすので、少 VRAM でも GPU を使った方が速い）。明示指定が
// 要るのは (a) 全層 GPU を強制したい（max）、(b) 意図的に GPU を空けたい（cpu）の2つだけ。
//
// Thinking は「本文と思考を混ぜない」ことが本体。届き方が3系統あるので全部受ける:
//   1. Ollama ネイティブ ... /api/chat に think を渡すと message.thinking で分かれて来る
//   2. OpenAI互換 ......... delta.reasoning_content / delta.reasoning（LM Studio 等）
//   3. どちらでもない ..... 本文に <think>…</think> がそのまま混ざる（タグ分離が要る）

/// spec.gpu → Ollama の options.num_gpu（None＝キーを送らない＝サーバ任せ）。
pub(crate) fn ollama_num_gpu(spec: &FilterSpec) -> Option<i64> {
    match spec.gpu.as_deref().map(str::trim).unwrap_or("") {
        // 999 = 「全部の層」。Ollama は層数を超える値をクランプするので実質「最大限」。
        "max" => Some(999),
        "cpu" => Some(0),
        _ => None, // "auto" / "" / 未知の値＝サーバ任せ（＝載る限り GPU）
    }
}

// v1.8.9 思考の送り方は ai_models::plan_think に一本化（能力が no のモデルには送らない —
// Ollama 0.34 は思考非対応モデルへの think を 400 で拒否する。旧 ollama_think は
// 「解さないモデルでは無視される」前提だったが、実機ではそうならなかった）。

/// v1.9.2 追加パラメータで上書きさせないキー（本体の組み立てか、他の欄が管理する）。
/// 画面側（AiSettingsDialog の RESERVED_EXTRA_KEYS）と揃えること。
pub const AI_RESERVED_EXTRA_KEYS: &[&str] =
    &["model", "messages", "stream", "temperature", "think", "reasoning_effort", "options"];

/// v1.9.2 追加パラメータの検査（予約キーを含めば理由を返す）。
pub fn check_extra_params(m: &serde_json::Map<String, serde_json::Value>) -> Result<(), String> {
    let bad: Vec<&str> = m
        .keys()
        .map(String::as_str)
        .filter(|k| AI_RESERVED_EXTRA_KEYS.contains(k))
        .collect();
    if bad.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "追加パラメータに使えないキーがあります: {}（モデル・温度・思考などは専用の欄で指定してください）",
            bad.join(", ")
        ))
    }
}

/// v1.9.2 追加パラメータを本体のトップレベルへマージ（変換・対話・全サーバ共通）。
fn apply_extra_params(body: &mut serde_json::Value, spec: &FilterSpec) -> Result<(), String> {
    let Some(m) = &spec.extra_params else { return Ok(()) };
    check_extra_params(m)?;
    for (k, v) in m {
        body[k.as_str()] = v.clone();
    }
    Ok(())
}

/// v1.8.9 Ollama の options（temperature / num_gpu / num_ctx）。変換と対話で共用。
fn ollama_options(spec: &FilterSpec) -> serde_json::Value {
    let mut options = serde_json::Map::new();
    if let Some(t) = spec.temperature {
        options.insert("temperature".into(), serde_json::json!(t));
    }
    // v1.8.6 GPU の使い方（既定＝キー無し＝載る限り GPU）。
    if let Some(n) = ollama_num_gpu(spec) {
        options.insert("num_gpu".into(), serde_json::json!(n));
    }
    if let Some(n) = spec.num_ctx.filter(|n| *n > 0) {
        options.insert("num_ctx".into(), serde_json::json!(n));
    }
    serde_json::Value::Object(options)
}

/// v1.8.9 送信前の準備: モデルの能力を確かめ（Ollama は /api/tags を1回・digest が
/// 変わっていたときだけ /api/show）、送る思考パラメータを決め、実行ログの雛形を作る。
/// 能力を確かめられなくても実行は止めない（不明＝設定どおり送る）。
/// tokio 文脈外のスレッドから呼ぶこと（reqwest::blocking）。
fn prepare_ai_run(
    spec: &FilterSpec,
    provider: &str,
    model: &str,
) -> (crate::ai_models::ThinkPlan, crate::ai_models::AiRunRecord) {
    use crate::ai_models::*;
    let endpoint = effective_ai_endpoint(spec);
    let mut notes = Vec::new();
    // 汎用 OpenAI互換は能力を取る API が無い（毎回の注記はノイズなので黙って不明扱い）。
    let info = if provider == "openai" {
        None
    } else {
        match ensure_model_info(provider, &endpoint, model) {
            Ok(i) => Some(i),
            Err(e) => {
                notes.push(format!("能力を確認できませんでした（設定どおり送信）: {e}"));
                None
            }
        }
    };
    let plan = plan_think(provider, spec.think.as_deref(), info.as_ref(), spec.send_reasoning);
    notes.extend(plan.note.clone());
    let ollama = !is_openai_compat(provider);
    if !ollama && spec.num_ctx.is_some() {
        notes.push("num_ctx は Ollama でのみ有効です（送信していません）".into());
    }
    let rec = AiRunRecord {
        provider: provider.to_string(),
        endpoint: normalize_endpoint(provider, &endpoint),
        model: model.to_string(),
        digest: info.as_ref().map(|i| i.digest.clone()).filter(|d| !d.is_empty()),
        capabilities: info.as_ref().map(|i| i.capabilities),
        thinking_mode: info.as_ref().map(|i| i.thinking_mode.clone()),
        think_setting: spec.think.clone().filter(|s| !s.trim().is_empty()),
        sent: SentParams {
            tools: false,
            think: plan.ollama_think.clone(),
            reasoning_effort: plan.reasoning_effort.clone(),
            num_ctx: if ollama { spec.num_ctx.filter(|n| *n > 0) } else { None },
            num_gpu: if ollama { ollama_num_gpu(spec) } else { None },
            temperature: spec.temperature,
            extra: spec.extra_params.clone().filter(|m| !m.is_empty()),
        },
        notes,
        thinking_returned: false,
    };
    (plan, rec)
}

/// v1.8.9 エラー応答の本文から理由を取り出す。Ollama は {"error":"..."}、OpenAI準拠は
/// {"error":{"message":"..."}}。対話（ストリーム）経路は従来ステータスしか出しておらず、
/// 「"gemma3" does not support thinking」のような肝心の理由が消えていた。
fn api_error_text(status: reqwest::StatusCode, body: &str) -> String {
    let msg = serde_json::from_str::<serde_json::Value>(body).ok().and_then(|v| {
        v["error"]["message"]
            .as_str()
            .or_else(|| v["error"].as_str())
            .map(String::from)
    });
    match msg {
        Some(m) => format!("AI APIエラー ({status}): {m}"),
        None if !body.trim().is_empty() => {
            let head: String = body.trim().chars().take(300).collect();
            format!("AI APIエラー ({status}): {head}")
        }
        None => format!("AI APIエラー ({status})"),
    }
}

/// v1.8.6 ストリーム断片の種別。本文と思考を取り違えないための区別で、
/// Thinking は表示のみ（履歴にもログにも残さない）＝呼び出し側が捨てられるようにする。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChatChunk {
    /// 会話本文。履歴に積まれる。
    Content,
    /// 思考。表示するだけで保存しない。
    Thinking,
}

/// 思考タグ。`<think>` が主流だが `<thinking>` を出すモデルもある。
const THINK_OPEN: [&str; 2] = ["<think>", "<thinking>"];
const THINK_CLOSE: [&str; 2] = ["</think>", "</thinking>"];

/// 本文に混ざる `<think>…</think>` を切り分ける状態機械。
///
/// ストリームは任意の位置で切れる（"<thi" / "nk>" のようにタグが割れて届く）ので、
/// タグの書きかけになり得る末尾は確定させずに保留し、次のチャンクと繋いでから判定する。
/// これをやらないと、割れたタグが本文としてそのまま画面に出る。
pub(crate) struct ThinkSplitter {
    inside: bool,
    pending: String,
}

impl ThinkSplitter {
    pub(crate) fn new() -> Self {
        Self {
            inside: false,
            pending: String::new(),
        }
    }

    /// チャンクを与え、確定した断片を (種別, テキスト) の順で返す。
    pub(crate) fn push(&mut self, chunk: &str) -> Vec<(ChatChunk, String)> {
        self.pending.push_str(chunk);
        let mut out = Vec::new();
        loop {
            let tags = if self.inside { THINK_CLOSE } else { THINK_OPEN };
            // 最も手前に現れるタグを採る（"<think>" と "<thinking>" は前者が先に一致する
            // ので、位置が同じなら長い方＝"<thinking>" を優先して消す）。
            let hit = tags
                .iter()
                .filter_map(|t| self.pending.find(*t).map(|i| (i, t.len())))
                .min_by_key(|(i, len)| (*i, std::cmp::Reverse(*len)));
            match hit {
                Some((i, len)) => {
                    if i > 0 {
                        out.push((self.kind(), self.pending[..i].to_string()));
                    }
                    self.pending = self.pending[i + len..].to_string();
                    self.inside = !self.inside;
                }
                None => {
                    // タグの書きかけが末尾にあれば次チャンクまで持ち越す。
                    let keep = partial_tag_len(&self.pending, &tags);
                    let cut = self.pending.len() - keep;
                    if cut > 0 {
                        let head = self.pending[..cut].to_string();
                        self.pending = self.pending[cut..].to_string();
                        out.push((self.kind(), head));
                    }
                    break;
                }
            }
        }
        out.retain(|(_, t)| !t.is_empty());
        out
    }

    /// ストリーム終了時。保留中の端切れ（未完のタグ等）を吐き出す。
    pub(crate) fn flush(&mut self) -> Option<(ChatChunk, String)> {
        if self.pending.is_empty() {
            return None;
        }
        let t = std::mem::take(&mut self.pending);
        Some((self.kind(), t))
    }

    fn kind(&self) -> ChatChunk {
        if self.inside {
            ChatChunk::Thinking
        } else {
            ChatChunk::Content
        }
    }
}

/// 末尾が tags のいずれかの「途中まで」になっている長さ（0＝書きかけ無し）。
fn partial_tag_len(s: &str, tags: &[&str; 2]) -> usize {
    let max = tags.iter().map(|t| t.len()).max().unwrap_or(0);
    let n = s.len();
    // 完全一致は find 済みなので、見るのは「最長タグ-1」文字まで（ここを含める —
    // 含め損ねると "<think" のような 6 文字の書きかけを保留できず本文へ漏れる）。
    for k in (1..=max.saturating_sub(1).min(n)).rev() {
        let start = n - k;
        if !s.is_char_boundary(start) {
            continue;
        }
        let tail = &s[start..];
        if tags.iter().any(|t| t.starts_with(tail)) {
            return k;
        }
    }
    0
}

/// 非ストリーム応答から思考を落として本文だけにする（変換モード用）。
/// 変換カードの出力は下流のフィルタが食べるので、思考が混ざると壊れる。
pub(crate) fn strip_think_blocks(text: &str) -> String {
    let mut sp = ThinkSplitter::new();
    let mut out = String::new();
    let mut removed = false;
    let mut chunks = sp.push(text);
    chunks.extend(sp.flush());
    for (kind, t) in chunks {
        match kind {
            ChatChunk::Content => out.push_str(&t),
            ChatChunk::Thinking => removed = true,
        }
    }
    if !removed {
        // タグが無かった＝何も削っていない。既存の出力（前後の空白含む）を変えない。
        return text.to_string();
    }
    // 思考ブロックの前後に残る空白（"…</think>\n\n本文"）を落とす。
    out.trim().to_string()
}

/// 記録を取らない版（実機検証テスト用）。
#[cfg(test)]
pub fn run_ai(spec: &FilterSpec, input: &[u8]) -> Result<(Vec<u8>, String, String), String> {
    let cancel = std::sync::atomic::AtomicBool::new(false);
    run_ai_rec(spec, input, &cancel, &mut None)
}

/// v1.8.9 run_ai ＋実行条件の記録。rec には送信前に雛形が入る（失敗しても残る＝
/// 非対応エラーの実行条件もログに書ける）。
/// v1.11 cancel（中断ボタン／MCP cancel_job）が立てば接続を閉じて Err("中断されました")。
/// 以前は一括 HTTP で応答完走まで止まらなかった → 受信をストリームにして行ごとに見る。
pub fn run_ai_rec(
    spec: &FilterSpec,
    input: &[u8],
    cancel: &std::sync::atomic::AtomicBool,
    rec: &mut Option<crate::ai_models::AiRunRecord>,
) -> Result<(Vec<u8>, String, String), String> {
    std::thread::scope(|s| {
        s.spawn(|| run_ai_blocking(spec, input, cancel, rec))
            .join()
            .map_err(|_| "AI実行スレッドが異常終了しました".to_string())?
    })
}

fn run_ai_blocking(
    spec: &FilterSpec,
    input: &[u8],
    cancel: &std::sync::atomic::AtomicBool,
    rec: &mut Option<crate::ai_models::AiRunRecord>,
) -> Result<(Vec<u8>, String, String), String> {
    let provider = spec.provider.as_deref().unwrap_or("ollama");
    let model = spec
        .model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or("AIフィルタにモデルが指定されていません")?;
    let user = String::from_utf8_lossy(input).into_owned();
    if user.trim().is_empty() {
        return Err("入力テキストが空です".into());
    }
    let system = spec
        .system
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let mut messages = Vec::new();
    if let Some(s) = system {
        messages.push(serde_json::json!({ "role": "system", "content": s }));
    }
    messages.push(serde_json::json!({ "role": "user", "content": user }));

    // v1.8.9 能力の確認と思考パラメータの決定（記録の雛形はここで rec へ）。
    let (plan, record) = prepare_ai_run(spec, provider, model);
    let r = rec.insert(record);

    // 本文は生のまま全部つなぎ、最後に <think> をまとめて落とす（一括時代と同じ結果）。
    // 別フィールドで来る思考（message.thinking / reasoning_content）は読み捨てて記録だけ。
    let mut content = String::new();
    let mut thinking = false;
    let cancelled = stream_ai_chat(spec, provider, model, &plan, &messages, cancel, &mut |k, t| {
        match k {
            ChatChunk::Content => content.push_str(t),
            ChatChunk::Thinking => thinking = true,
        }
    })?;
    if thinking {
        r.thinking_returned = true;
    }
    if cancelled {
        // 「中断＝出力は破棄」の既存契約（書きかけは返さない）。
        return Err("中断されました".into());
    }

    // v1.8.6 変換カードの出力は下流のフィルタが食べる → 思考は必ず落とす。
    // think を解さないサーバや、think を無視して本文に混ぜるモデルへの保険でもある。
    let stripped = strip_think_blocks(&content);
    if stripped != content {
        r.thinking_returned = true;
    }

    Ok((stripped.into_bytes(), "md".into(), "text/markdown".into()))
}

/// v1.5 Text-To-Image デーモン（t2i_server.py）の待受ポート。TexoS が system Python で
/// 起動・管理する（Ollama 相当の常駐サーバー）。固定ポート＝接続先は環境非依存。
pub const T2I_PORT: u16 = 8765;

/// 実行中の生成をデーモンに中断させる（best-effort・短時間）。タイムアウトや中断ボタンで使う。
/// tokio 文脈から呼ばれても安全なよう素の OS スレッドで実行する。
pub fn t2i_post_cancel() {
    let url = format!("http://127.0.0.1:{T2I_PORT}/cancel");
    let _ = std::thread::scope(|s| {
        s.spawn(|| {
            if let Ok(c) = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(3))
                .build()
            {
                let _ = c.post(&url).send();
            }
        })
        .join()
    });
}

/// builtin:"t2i": 入力テキストをプロンプトとして diffusers 常駐デーモンに送り、PNG を得る。
/// `init_image` を渡すと img2img（元画像を部分修正）になる。
/// run_ai と同じ理由で reqwest::blocking を tokio 文脈外の素の OS スレッドで実行する
/// （内部ランタイム drop パニック → DB Mutex poison → 全体クラッシュ を避ける）。
pub fn run_t2i(
    spec: &FilterSpec,
    prompt_bytes: &[u8],
    init_image: Option<&[u8]>,
) -> Result<(Vec<u8>, String, String), String> {
    std::thread::scope(|s| {
        s.spawn(|| run_t2i_blocking(spec, prompt_bytes, init_image))
            .join()
            .map_err(|_| "画像生成スレッドが異常終了しました".to_string())?
    })
}

fn run_t2i_blocking(
    spec: &FilterSpec,
    prompt_bytes: &[u8],
    init_image: Option<&[u8]>,
) -> Result<(Vec<u8>, String, String), String> {
    let prompt = String::from_utf8_lossy(prompt_bytes).trim().to_string();
    if prompt.is_empty() {
        return Err("入力テキスト（プロンプト）が空です".into());
    }
    // タイムアウト: 未指定/0 = 無制限（生成系共通契約 — gen_client）。
    let client = gen_client(spec.timeout_secs)?;

    let mut body = serde_json::json!({ "prompt": prompt });
    if let Some(m) = spec.model.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        body["model"] = serde_json::json!(m);
    }
    if let Some(n) = spec.negative.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        body["negative_prompt"] = serde_json::json!(n);
    }
    if let Some(v) = spec.steps {
        body["steps"] = serde_json::json!(v);
    }
    if let Some(v) = spec.guidance {
        body["guidance"] = serde_json::json!(v);
    }
    if let Some(v) = spec.width {
        body["width"] = serde_json::json!(v);
    }
    if let Some(v) = spec.height {
        body["height"] = serde_json::json!(v);
    }
    if let Some(v) = spec.seed {
        body["seed"] = serde_json::json!(v);
    }
    // モディファイア（SD/SDXL 中心）。空/未指定は送らない＝デーモン側は既定のまま。
    if let Some(s) = spec.lora.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        body["lora"] = serde_json::json!(s);
    }
    if let Some(s) = spec.vae.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        body["vae"] = serde_json::json!(s);
    }
    if let Some(s) = spec.sampler.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        body["sampler"] = serde_json::json!(s);
    }
    if let Some(v) = spec.clip_skip {
        body["clip_skip"] = serde_json::json!(v);
    }
    // img2img: 元画像を base64 で送る（あれば）。strength は変化度。
    if let Some(img) = init_image {
        use base64::Engine as _;
        body["init_image"] = serde_json::json!(base64::engine::general_purpose::STANDARD.encode(img));
        if let Some(s) = spec.strength {
            body["strength"] = serde_json::json!(s);
        }
    }

    let url = format!("http://127.0.0.1:{T2I_PORT}/generate");
    let resp = client.post(&url).json(&body).send().map_err(|e| {
        if e.is_connect() {
            "画像生成サーバーに接続できません。設定（T2Iセットアップ）でモデルを取得してサーバーを起動してください"
                .to_string()
        } else if e.is_timeout() {
            // タイムアウトしてもデーモン側の生成はまだ走っている（GPU を握り続ける）。
            // /cancel を投げて次ステップ境界で止めさせる（GPU を解放）。
            t2i_post_cancel();
            "画像生成がタイムアウトしました。ステップ数を減らすか制限時間を延ばしてください（生成は中断しました）".to_string()
        } else {
            format!("画像生成リクエストに失敗: {e}")
        }
    })?;
    let status = resp.status();
    if !status.is_success() {
        // サーバーは失敗を {"error": "..."} で返す
        let v: serde_json::Value = resp.json().unwrap_or_else(|_| serde_json::json!({}));
        let msg = v["error"].as_str().unwrap_or("不明なエラー");
        return Err(format!("画像生成エラー ({status}): {msg}"));
    }
    let bytes = resp
        .bytes()
        .map_err(|e| format!("画像の受信に失敗: {e}"))?
        .to_vec();
    if bytes.is_empty() {
        return Err("画像生成サーバーが空の応答を返しました".into());
    }
    Ok((bytes, "png".into(), "image/png".into()))
}

// --- v2.0 t23d（builtin:"t23d"）: Image-To-3D。Hunyuan3D-2 常駐デーモン（t23d_server.py・
//     runtime/t23d.rs が起動管理）へ画像を送り glb を得る。t2i と同じ流儀の別ポート別プロセス。

/// t23d デーモン（t23d_server.py）の待受ポート。t2i（8765）の隣＝固定ポートで環境非依存。
pub const T23D_PORT: u16 = 8766;

/// 実行中の 3D 生成をデーモンに中断させる（best-effort・短時間）。
pub fn t23d_post_cancel() {
    let url = format!("http://127.0.0.1:{T23D_PORT}/cancel");
    let _ = std::thread::scope(|s| {
        s.spawn(|| {
            if let Ok(c) = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(3))
                .build()
            {
                let _ = c.post(&url).send();
            }
        })
        .join()
    });
}

/// builtin:"t23d": 入力画像を Hunyuan3D-2 デーモンに送り、glb（白メッシュ）を得る。
/// run_t2i と同じ理由で reqwest::blocking を tokio 文脈外の素の OS スレッドで実行する。
pub fn run_t23d(spec: &FilterSpec, image: &[u8]) -> Result<(Vec<u8>, String, String), String> {
    std::thread::scope(|s| {
        s.spawn(|| run_t23d_blocking(spec, image))
            .join()
            .map_err(|_| "3D生成スレッドが異常終了しました".to_string())?
    })
}

fn run_t23d_blocking(spec: &FilterSpec, image: &[u8]) -> Result<(Vec<u8>, String, String), String> {
    if image.is_empty() {
        return Err("入力画像が空です".into());
    }
    let texture = spec.texture.as_deref() == Some("on");
    // タイムアウト: 未指定/0 = 無制限（生成系共通契約 — gen_client）。テクスチャ付きは
    // 数分〜十数分かかるため、既定で打ち切らないことが特に効く。
    let client = gen_client(spec.timeout_secs)?;

    use base64::Engine as _;
    let mut body = serde_json::json!({
        "image": base64::engine::general_purpose::STANDARD.encode(image),
    });
    if let Some(m) = spec.model.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        body["model"] = serde_json::json!(m);
    }
    if let Some(v) = spec.steps {
        body["steps"] = serde_json::json!(v);
    }
    if let Some(v) = spec.guidance {
        body["guidance"] = serde_json::json!(v);
    }
    if let Some(v) = spec.octree {
        body["octree"] = serde_json::json!(v);
    }
    if let Some(v) = spec.seed {
        body["seed"] = serde_json::json!(v);
    }
    if texture {
        body["texture"] = serde_json::json!(true);
        if let Some(v) = spec.face_count {
            body["face_count"] = serde_json::json!(v);
        }
    }

    let url = format!("http://127.0.0.1:{T23D_PORT}/generate");
    let resp = client.post(&url).json(&body).send().map_err(|e| {
        if e.is_connect() {
            "3D生成サーバーに接続できません。3D生成カードの作成からエンジン（Hunyuan3D-2 WinPortable）の場所を設定してください"
                .to_string()
        } else if e.is_timeout() {
            // タイムアウトしてもデーモン側の生成は走り続ける → /cancel で止めて GPU を解放。
            t23d_post_cancel();
            "3D生成がタイムアウトしました。ステップ数か octree 解像度を下げるか、制限時間を延ばしてください（生成は中断しました）".to_string()
        } else {
            format!("3D生成リクエストに失敗: {e}")
        }
    })?;
    let status = resp.status();
    if !status.is_success() {
        let v: serde_json::Value = resp.json().unwrap_or_else(|_| serde_json::json!({}));
        let msg = v["error"].as_str().unwrap_or("不明なエラー");
        return Err(format!("3D生成エラー ({status}): {msg}"));
    }
    let bytes = resp
        .bytes()
        .map_err(|e| format!("3Dモデルの受信に失敗: {e}"))?
        .to_vec();
    if bytes.is_empty() {
        return Err("3D生成サーバーが空の応答を返しました".into());
    }
    Ok((bytes, "glb".into(), "model/gltf-binary".into()))
}

/// 本文として届いた断片をタグ分離に通し、確定分を配信・累積する。
/// サーバが思考を分けてくれる場合でも通す — 分けてくれない場合との分岐を増やさないため、
/// そして think を無視して本文に <think> を混ぜるモデルが実在するため。
fn emit_content(
    raw: &str,
    sp: &mut ThinkSplitter,
    full: &mut String,
    on_token: &mut dyn FnMut(ChatChunk, &str),
) {
    for (kind, t) in sp.push(raw) {
        if kind == ChatChunk::Content {
            full.push_str(&t);
        }
        on_token(kind, &t);
    }
}

/// v1.11 AI のストリーム受信の本体（対話と変換で共用）。送信本体の組み立て・送信・
/// SSE / NDJSON の解読までを行い、届いた断片を on_delta へ渡す:
///   Content ＝本文の生の断片（<think> タグの分離は呼び出し側）
///   Thinking ＝別フィールドで届いた思考（message.thinking / reasoning_content）
/// cancel を見たら応答を drop して接続を閉じ Ok(true)（＝中断）を返す。完走は Ok(false)。
/// tokio 文脈外のスレッドから呼ぶこと（reqwest::blocking）。
fn stream_ai_chat(
    spec: &FilterSpec,
    provider: &str,
    model: &str,
    plan: &crate::ai_models::ThinkPlan,
    messages: &[serde_json::Value],
    cancel: &std::sync::atomic::AtomicBool,
    on_delta: &mut dyn FnMut(ChatChunk, &str),
) -> Result<bool, String> {
    use std::io::BufRead;
    use std::sync::atomic::Ordering;
    // ストリーミングは長時間。生成系共通契約（未指定/0 = 無制限・cancel で停止）。
    let client = gen_client(spec.timeout_secs)?;
    if is_openai_compat(provider) {
        // OpenAI準拠: SSE（"data: {...}" 行、終端は [DONE]）
        let endpoint = spec
            .endpoint
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("http://localhost:1234/v1")
            .trim_end_matches('/');
        let mut body = serde_json::json!({
            "model": model, "messages": messages, "stream": true,
        });
        if let Some(t) = spec.temperature {
            body["temperature"] = serde_json::json!(t);
        }
        // v1.8.9 LM Studio の深さ指定（gpt-oss 等・レベルを受けるモデルだけ）。
        // v1.9.2 汎用 OpenAI互換もカードで「送る」にしたときは届く（plan_think）。
        if let Some(e) = &plan.reasoning_effort {
            body["reasoning_effort"] = serde_json::json!(e);
        }
        apply_extra_params(&mut body, spec)?;
        let resp = client
            .post(format!("{endpoint}/chat/completions"))
            .json(&body)
            .send()
            .map_err(|e| ai_conn_err(provider, endpoint, &e))?;
        if cancel.load(Ordering::Relaxed) {
            return Ok(true);
        }
        if !resp.status().is_success() {
            // v1.8.9 本文の理由まで出す（従来はステータスだけで原因が分からなかった）。
            let status = resp.status();
            return Err(api_error_text(status, &resp.text().unwrap_or_default()));
        }
        for line in std::io::BufReader::new(resp).lines() {
            if cancel.load(Ordering::Relaxed) {
                return Ok(true); // resp を drop ＝接続を閉じる → サーバ側の生成も止まる
            }
            let line = line.map_err(|e| format!("ストリーム読み取り失敗: {e}"))?;
            let data = match line.trim().strip_prefix("data:") {
                Some(d) => d.trim(),
                None => continue,
            };
            if data == "[DONE]" {
                break;
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(data) {
                // ストリーム途中のエラー通知（{"error":{"message":…}}）を空応答にしない。
                if let Some(m) = v["error"]["message"].as_str().or_else(|| v["error"].as_str()) {
                    return Err(format!("AI APIエラー: {m}"));
                }
                let delta = &v["choices"][0]["delta"];
                // v1.8.6 思考を別フィールドで返すサーバ（LM Studio 等）。キー名は
                // reasoning_content / reasoning の二派があるので両方見る。
                if let Some(th) = delta["reasoning_content"]
                    .as_str()
                    .or_else(|| delta["reasoning"].as_str())
                {
                    if !th.is_empty() {
                        on_delta(ChatChunk::Thinking, th);
                    }
                }
                if let Some(tok) = delta["content"].as_str() {
                    if !tok.is_empty() {
                        on_delta(ChatChunk::Content, tok);
                    }
                }
            }
        }
    } else {
        // Ollama ネイティブ: NDJSON（1行=1JSON、done:true で終端）
        let endpoint = spec
            .endpoint
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("http://localhost:11434")
            .trim_end_matches('/');
        let mut body = serde_json::json!({
            "model": model, "messages": messages, "stream": true,
            "keep_alive": "5m", "options": ollama_options(spec),
        });
        // v1.8.6 Thinking。think を付けると思考が message.thinking に分かれて届く。
        // v1.8.9 送る値は能力に応じて plan_think が決める（非対応モデルには送らない）。
        if let Some(t) = &plan.ollama_think {
            body["think"] = t.clone();
        }
        apply_extra_params(&mut body, spec)?;
        let resp = client
            .post(format!("{endpoint}/api/chat"))
            .json(&body)
            .send()
            .map_err(|e| ai_conn_err(provider, endpoint, &e))?;
        if cancel.load(Ordering::Relaxed) {
            return Ok(true);
        }
        if !resp.status().is_success() {
            // v1.8.9 本文の理由まで出す（"… does not support thinking" 等）。
            let status = resp.status();
            return Err(api_error_text(status, &resp.text().unwrap_or_default()));
        }
        for line in std::io::BufReader::new(resp).lines() {
            if cancel.load(Ordering::Relaxed) {
                return Ok(true); // resp を drop ＝接続を閉じる → サーバ側の生成も止まる
            }
            let line = line.map_err(|e| format!("ストリーム読み取り失敗: {e}"))?;
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                if let Some(err) = v["error"].as_str() {
                    return Err(format!("AI APIエラー: {err}"));
                }
                // v1.8.6 思考は message.thinking に分かれて来る（think 指定時）。
                if let Some(th) = v["message"]["thinking"].as_str() {
                    if !th.is_empty() {
                        on_delta(ChatChunk::Thinking, th);
                    }
                }
                if let Some(tok) = v["message"]["content"].as_str() {
                    if !tok.is_empty() {
                        on_delta(ChatChunk::Content, tok);
                    }
                }
                if v["done"].as_bool().unwrap_or(false) {
                    break;
                }
            }
        }
    }
    Ok(false)
}

/// v1.4.1 チャット: ストリーミング応答。messages は system を含む会話履歴全体。
/// on_token はトークン断片ごとに呼ばれる（UIへ逐次表示する）。完成した本文を返す。
/// 呼び出し側は tokio 文脈外の素のOSスレッドで実行すること（run_ai と同じ理由 —
/// reqwest::blocking の内部ランタイム drop パニックを避ける）。cancel で途中停止。
///
/// v1.8.6 Thinking: on_token は断片の種別（ChatChunk::Content / Thinking）を受け取る。
/// 戻り値の本文には **思考は含まれない** — 思考は表示だけして履歴にもログにも残さない、
/// という方針（コンテキストを食わせない・次ターンへ送らない）をここで保証する。
#[cfg(test)]
pub fn run_ai_chat_stream(
    spec: &FilterSpec,
    messages: &[serde_json::Value],
    cancel: &std::sync::atomic::AtomicBool,
    on_token: &mut dyn FnMut(ChatChunk, &str),
) -> Result<String, String> {
    run_ai_chat_stream_rec(spec, messages, cancel, on_token, &mut None)
}

/// v1.8.9 run_ai_chat_stream ＋実行条件の記録（rec は送信前に入る＝失敗時も残る）。
pub fn run_ai_chat_stream_rec(
    spec: &FilterSpec,
    messages: &[serde_json::Value],
    cancel: &std::sync::atomic::AtomicBool,
    outer_on_token: &mut dyn FnMut(ChatChunk, &str),
    rec: &mut Option<crate::ai_models::AiRunRecord>,
) -> Result<String, String> {
    let provider = spec.provider.as_deref().unwrap_or("ollama");
    let model = spec
        .model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or("AIフィルタにモデルが指定されていません")?;

    // v1.8.9 能力の確認と思考パラメータの決定（記録の雛形はここで rec へ）。
    let (plan, record) = prepare_ai_run(spec, provider, model);
    *rec = Some(record);
    // 応答に思考が含まれていたかを記録する（中身は保存しない）。
    let saw_thinking = std::cell::Cell::new(false);
    let mut on_token_rec = |k: ChatChunk, t: &str| {
        if k == ChatChunk::Thinking {
            saw_thinking.set(true);
        }
        outer_on_token(k, t)
    };
    let on_token: &mut dyn FnMut(ChatChunk, &str) = &mut on_token_rec;

    let mut full = String::new();
    // 本文に混ざる <think>…</think> の分離器（チャンク境界で割れても落とさない）。
    let mut sp = ThinkSplitter::new();
    // 中断時は書きかけまでを返す（対話の既存挙動。呼び出し側が cancel を見て扱う）。
    stream_ai_chat(spec, provider, model, &plan, messages, cancel, &mut |k, t| match k {
        ChatChunk::Content => emit_content(t, &mut sp, &mut full, on_token),
        ChatChunk::Thinking => on_token(ChatChunk::Thinking, t),
    })?;
    // 保留中の端切れ（未完のタグ・中断時の書きかけ）を取りこぼさない。
    if let Some((kind, t)) = sp.flush() {
        if kind == ChatChunk::Content {
            full.push_str(&t);
        }
        on_token(kind, &t);
    }
    if let Some(r) = rec.as_mut() {
        r.thinking_returned = saw_thinking.get();
    }
    Ok(full)
}

/// v1.4.2 設定ダイアログのモデル一覧。Ollama=/api/tags（name）、OpenAI準拠=/v1/models（id）。
/// run_ai と同じく tokio 文脈外スレッドへ逃がして blocking 実行する。
pub fn list_models(provider: &str, endpoint: &str) -> Result<Vec<String>, String> {
    std::thread::scope(|s| {
        s.spawn(|| list_models_blocking(provider, endpoint))
            .join()
            .map_err(|_| "モデル取得スレッドが異常終了しました".to_string())?
    })
}

pub(crate) fn list_models_blocking(provider: &str, endpoint: &str) -> Result<Vec<String>, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("HTTPクライアントの生成に失敗: {e}"))?;
    let ep = endpoint.trim().trim_end_matches('/');
    let (url, is_openai) = if is_openai_compat(provider) {
        let base = if ep.is_empty() { "http://localhost:1234/v1" } else { ep };
        (format!("{base}/models"), true)
    } else {
        let base = if ep.is_empty() { "http://localhost:11434" } else { ep };
        (format!("{base}/api/tags"), false)
    };
    let resp = client
        .get(&url)
        .send()
        .map_err(|e| ai_conn_err(provider, &url, &e))?;
    if !resp.status().is_success() {
        return Err(format!("モデル一覧の取得に失敗 ({})", resp.status()));
    }
    let v: serde_json::Value = resp.json().map_err(|e| format!("一覧の解析に失敗: {e}"))?;
    let mut out: Vec<String> = if is_openai {
        v["data"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|m| m["id"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    } else {
        v["models"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|m| m["name"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    out.sort();
    out.dedup();
    Ok(out)
}

/// AI接続失敗を日本語で分かりやすく（サーバ未起動が最頻なので最優先で案内）。
/// v1.7.3: 案内文を provider 別に — 従来は provider に関わらず「Ollama が起動しているか」と
/// 出ていて、LM Studio 運用では誤誘導だった（次に押すべきボタンが違う）。
fn ai_conn_err(provider: &str, endpoint: &str, e: &reqwest::Error) -> String {
    if e.is_connect() {
        let hint = match provider {
            "lmstudio" => "LM Studio の「Developer」タブでローカルサーバを起動（Status: Running）しているか確認してください",
            p if is_openai_compat(p) => "サーバが起動しているか、エンドポイントのURL（/v1 まで含む）が正しいか確認してください",
            _ => "Ollama が起動しているか確認してください",
        };
        format!("{provider} に接続できません（{endpoint}）。{hint}")
    } else if e.is_timeout() {
        format!("AI応答がタイムアウトしました（{endpoint}）。制限時間を延ばすか軽いモデルを試してください")
    } else {
        format!("AIリクエストに失敗（{endpoint}）: {e}")
    }
}

/// 外部コマンドの既定タイムアウト（FilterSpec.timeout_secs で上書き可）。
const DEFAULT_TIMEOUT_SECS: u64 = 60;
/// 暴走プログラムが出力を書き続けたときの強制終了しきい値。
const MAX_OUTPUT_BYTES: u64 = 512 * 1024 * 1024;

/// プロセスをツリーごと止める。Windows は cmd /C 経由で起動しているため
/// child.kill() では cmd だけが死に実体の孫プロセスが残る — taskkill /T で根こそぎ。
#[cfg(windows)]
fn kill_tree(child: &mut std::process::Child) {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/T", "/PID", &child.id().to_string()])
        .creation_flags(0x0800_0000)
        .status();
    let _ = child.wait();
}
#[cfg(not(windows))]
fn kill_tree(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// CMD が「集約実行」（全入力を集めて1回実行）を要求しているか。
/// `{in*}`（全入力一括）または `{in2}` 以降の位置指定があれば集約。
/// `{in}`/`{in1}` のみなら従来の「入力ごとに実行」（ファンアウト）。
pub fn cmd_is_aggregate(cmd: &str) -> bool {
    if cmd.contains("{in*}") {
        return true;
    }
    regex::Regex::new(r"\{in([2-9]|[1-9]\d+)\}")
        .map(|re| re.is_match(cmd))
        .unwrap_or(false)
}

/// v1.4.10 リファクタリング（§4.1）: 外部プロセスを最後まで走らせた結果を「捨てずに」返す型。
/// 従来 run_external_cwd は exit code を握りつぶし、stderr も失敗時しか使っていなかった。
/// 将来の exit code 分岐（§4.3 成功時/失敗時の矢印）を載せられるよう、完走したプロセスの
/// 終了コード・stderr 抜粋・出力バイト列をそのまま持ち上げる。成否ポリシー（非0→エラー /
/// 出力なし→エラー）は into_bytes() に閉じ込め、従来契約（run_external_cwd）はその薄い適用。
#[derive(Debug, Clone)]
pub struct ExternalOutput {
    /// プロセス終了コード（Unix でシグナル終了した場合は None）。
    pub exit_code: Option<i32>,
    /// stderr の先頭 ~500 字（trim 済み。無ければ空文字）。
    pub stderr: String,
    /// {out} を読み出せた場合のみ Some（プログラムが出力を書かなければ None）。
    pub output: Option<Vec<u8>>,
    /// v1.6 {out*}: 複数出力モードの結果（ファイル名, バイト列）。**ファイル名の辞書順**
    /// （＝順序規約: プログラムはゼロ詰め連番等で順序を表す。ChillChordMaker の
    /// `prefix_小節_拍_コード名.wav` が模範）。CMD に {out*} が無ければ None。
    pub multi: Option<Vec<(String, Vec<u8>)>>,
}

impl ExternalOutput {
    /// 終了コード 0 を成功とみなす（std の ExitStatus::success と等価）。
    pub fn success(&self) -> bool {
        self.exit_code == Some(0)
    }

    /// 従来の run_external 契約（成功→出力バイト列 / 失敗→日本語エラー）へ落とす。
    /// エラーメッセージは旧 run_external_cwd と同一文面を保つ（回帰の安全網）。
    pub fn into_bytes(self) -> Result<Vec<u8>, String> {
        if !self.success() {
            let tail = if self.stderr.is_empty() {
                String::new()
            } else {
                format!("\n{}", self.stderr)
            };
            return Err(format!(
                "コマンドが失敗しました (exit {:?}){}",
                self.exit_code, tail
            ));
        }
        self.output
            .ok_or_else(|| "コマンドは成功しましたが {out} に出力がありません".to_string())
    }
}

/// 外部コマンドの実行（逃げ道）: プレースホルダを一時ファイルパスに置換してシェル実行。
/// 一時ファイルは読み取り後すぐ削除する（TEMP孤児を作らない方針）。
///
/// 入力プレースホルダ（v1.5 複数入力 — gcc の分割ソースや画像合成）:
/// - `{in}`  = 最初の入力（従来互換。`{in1}` と同義）
/// - `{inN}` = 接続順の N 番目（1始まり）。ユーザーが `"{in1}"` とクォートして書く
/// - `{in*}` = 全入力。各ファイルを個別にクォートして空白区切りに展開（裸で書く）
///
/// 安全弁（v1.4 実機指摘: フィルタ型プログラムの誤指定で TexoS ごとストール）:
/// - stdin は常に閉じる — 標準入力待ちのプログラムは即 EOF を受けて終わる
/// - タイムアウト・出力サイズ上限・`cancel`（UIの中断ボタン）でツリーごと強制終了
/// - stderr はファイルに受けて、失敗時のエラーメッセージに含める
///
/// §4.1 後: 本体は run_external_full に移り、これは「cwd/stdin 不要・成否のみ欲しい」呼び出し
/// 向けの薄い互換ラッパ（現状はテストが利用）。
#[allow(dead_code)]
pub fn run_external(
    cmdline: &str,
    inputs: &[(&[u8], &str)],
    out_ext: &str,
    timeout_secs: Option<u64>,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<Vec<u8>, String> {
    run_external_cwd(cmdline, inputs, out_ext, timeout_secs, cancel, None, None)
}

/// v1.4.9: 子プロセスへ制御を送る汎用 stdin チャネル。`Some(slot)` を渡すと stdin を
/// パイプで開いたまま起動し、その書き込み端を `slot` に置く（別コマンド flow_send_stdin が
/// 'q' 等を書ける）。`None` なら従来どおり stdin を閉じる（標準入力待ちのストール防止）。
pub type StdinSlot = std::sync::Mutex<Option<std::process::ChildStdin>>;

/// v1.4.2: 作業ディレクトリ(cwd)を指定できる版。cwd を渡すとコマンドはそこを
/// 「ここ」として実行する → 相対名や既定名の副産物（gcc の a.exe 等）が cwd に
/// 落ちる。lib.rs はプロジェクトルートを渡し、副産物がプロジェクト内に収まるようにする。
/// {in}/{out} は絶対パスなので正規の入出力には影響しない。
///
/// §4.1 後: exit code を見ない呼び出し向けに run_external_full + into_bytes を束ねた互換ラッパ。
#[allow(dead_code)]
pub fn run_external_cwd(
    cmdline: &str,
    inputs: &[(&[u8], &str)],
    out_ext: &str,
    timeout_secs: Option<u64>,
    cancel: &std::sync::atomic::AtomicBool,
    cwd: Option<&std::path::Path>,
    stdin_slot: Option<&StdinSlot>,
) -> Result<Vec<u8>, String> {
    run_external_full(cmdline, inputs, out_ext, timeout_secs, cancel, cwd, stdin_slot)?.into_bytes()
}

/// v1.4.10（§4.1）: run_external_cwd の中身。完走したプロセスの exit code・stderr・出力を
/// ExternalOutput としてそのまま返す（成否ポリシーを適用しない）。Err になるのは起動失敗・
/// タイムアウト・中断・出力上限超過・監視失敗といった「そもそも完走しなかった」場合のみ。
/// 従来契約が要るところは run_external_cwd（= 本関数 + into_bytes）を使う。
pub fn run_external_full(
    cmdline: &str,
    inputs: &[(&[u8], &str)],
    out_ext: &str,
    timeout_secs: Option<u64>,
    cancel: &std::sync::atomic::AtomicBool,
    cwd: Option<&std::path::Path>,
    stdin_slot: Option<&StdinSlot>,
) -> Result<ExternalOutput, String> {
    use std::sync::atomic::Ordering;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join("texos_flow");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let out_path = dir.join(format!("out_{nanos}.{out_ext}"));
    let err_path = dir.join(format!("err_{nanos}.txt"));
    let mut in_paths = Vec::new();
    for (i, (bytes, ext)) in inputs.iter().enumerate() {
        let p = dir.join(format!("in_{nanos}_{}.{}", i + 1, ext));
        if let Err(e) = std::fs::write(&p, bytes) {
            for q in &in_paths {
                let _ = std::fs::remove_file(q);
            }
            return Err(e.to_string());
        }
        in_paths.push(p);
    }
    let cleanup_inputs = |paths: &[std::path::PathBuf]| {
        for p in paths {
            let _ = std::fs::remove_file(p);
        }
    };
    let err_file = match std::fs::File::create(&err_path) {
        Ok(f) => f,
        Err(e) => {
            cleanup_inputs(&in_paths);
            return Err(e.to_string());
        }
    };
    // プレースホルダ展開: {in*} → 全入力（各個クォート）/ {inN} → N番目 / {in} → 1番目
    // {out} → 出力ファイル（拡張子つき）/ {out_noext} → 拡張子を除いたパス
    // （whisper.cpp の `-of <ベース名>` のように、ツール側が拡張子を自分で付ける場合に使う。
    //  output_ext を txt にしておけば whisper が書く <base>.txt が {out} と一致する）。
    // v1.6 {out*} → 出力フォルダ（複数ファイル書き出し。終了後に辞書順で全ファイルを収集）。
    // 置換順序: {out*} → {out_noext} → {out}（"{out}" は "{out*}"/"{out_noext}" の
    // 部分文字列にはならないが、順序を明示して将来の追加に備える）。
    let multi_mode = cmdline.contains("{out*}");
    let outs_dir = dir.join(format!("outs_{nanos}"));
    if multi_mode {
        if let Err(e) = std::fs::create_dir_all(&outs_dir) {
            cleanup_inputs(&in_paths);
            let _ = std::fs::remove_file(&err_path);
            return Err(format!("出力フォルダの作成に失敗: {e}"));
        }
    }
    let out_noext = out_path.with_extension("");
    let mut cmd = cmdline.replace("{out*}", &outs_dir.to_string_lossy());
    cmd = cmd.replace("{out_noext}", &out_noext.to_string_lossy());
    cmd = cmd.replace("{out}", &out_path.to_string_lossy());
    if cmd.contains("{in*}") {
        let joined = in_paths
            .iter()
            .map(|p| format!("\"{}\"", p.to_string_lossy()))
            .collect::<Vec<_>>()
            .join(" ");
        cmd = cmd.replace("{in*}", &joined);
    }
    for (i, p) in in_paths.iter().enumerate() {
        cmd = cmd.replace(&format!("{{in{}}}", i + 1), &p.to_string_lossy());
    }
    if let Some(first) = in_paths.first() {
        cmd = cmd.replace("{in}", &first.to_string_lossy());
    }
    // 未解決の入力プレースホルダ = 入力矢印が足りない（集約実行は本数が意味を持つ）
    if let Ok(re) = regex::Regex::new(r"\{in\d*\}") {
        if let Some(m) = re.find(&cmd) {
            cleanup_inputs(&in_paths);
            let _ = std::fs::remove_file(&err_path);
            if multi_mode {
                let _ = std::fs::remove_dir_all(&outs_dir);
            }
            return Err(format!(
                "{} に対応する入力がありません（接続されている入力: {}本）",
                m.as_str(),
                in_paths.len()
            ));
        }
    }

    // v1.4.9: 録音など制御を送りたいときは stdin をパイプで開く。それ以外は閉じる。
    let stdin_cfg = || {
        if stdin_slot.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        }
    };
    #[cfg(windows)]
    let spawned = {
        use std::os::windows::process::CommandExt;
        // コマンドライン全体を raw_arg で「素のまま」cmd に渡す。
        // std の自動クォート（\" エスケープ）は cmd.exe の解釈と非互換で、
        // 引用符を含むコマンドが exit 1 で壊れる（v1.4 実機指摘）。
        // さらに全体を "..." で包む — cmd /C は先頭が引用符のとき最初と最後の
        // 引用符を剥がすため、内側のクォートを無傷で残す定石。
        let mut c = std::process::Command::new("cmd");
        c.raw_arg(format!("/C \"{cmd}\""))
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW（コンソールを出さない）
            .stdin(stdin_cfg())
            .stdout(std::process::Stdio::null())
            .stderr(err_file);
        if let Some(dir) = cwd {
            c.current_dir(dir); // 相対名/既定名の副産物をプロジェクト内に落とす
        }
        c.spawn()
    };
    #[cfg(not(windows))]
    let spawned = {
        let mut c = std::process::Command::new("sh");
        c.args(["-c", &cmd])
            .stdin(stdin_cfg())
            .stdout(std::process::Stdio::null())
            .stderr(err_file);
        if let Some(dir) = cwd {
            c.current_dir(dir);
        }
        c.spawn()
    };

    // v1.3.11: timeout_secs==0 は「無制限」（開発者モード）。None は既定60秒。
    // 無制限でも中断ボタン（cancel）と出力上限は効く＝ハンドルを失わない。
    let timeout: Option<std::time::Duration> = match timeout_secs {
        Some(0) => None,
        Some(s) => Some(std::time::Duration::from_secs(s)),
        None => Some(std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS)),
    };
    let started = std::time::Instant::now();
    let status: Result<std::process::ExitStatus, String> = match spawned {
        Err(e) => Err(format!("コマンド起動に失敗: {e}")),
        Ok(mut child) => loop {
            // v1.4.9: stdin の書き込み端を共有スロットへ（別コマンドが 'q' 等を送れる）。
            // 取り出しは一度だけ（child.stdin は2回目以降 None）。
            if let Some(slot) = stdin_slot {
                if let Some(si) = child.stdin.take() {
                    if let Ok(mut g) = slot.lock() {
                        *g = Some(si);
                    }
                }
            }
            match child.try_wait() {
                Ok(Some(s)) => break Ok(s),
                Ok(None) => {}
                Err(e) => {
                    kill_tree(&mut child);
                    break Err(format!("プロセス監視に失敗: {e}"));
                }
            }
            if cancel.load(Ordering::Relaxed) {
                kill_tree(&mut child);
                break Err("中断されました".into());
            }
            if let Some(to) = timeout {
                if started.elapsed() > to {
                    kill_tree(&mut child);
                    break Err(format!(
                        "タイムアウト（{}秒）のため強制終了しました。標準入力待ちのフィルタ型プログラムを引数型で指定していないか確認してください",
                        to.as_secs()
                    ));
                }
            }
            let mut out_total = std::fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
            if multi_mode {
                // {out*} フォルダの合計も上限の対象（ファイル数は少数想定・100ms間隔の走査は軽い）
                if let Ok(rd) = std::fs::read_dir(&outs_dir) {
                    for e in rd.flatten() {
                        out_total += e.metadata().map(|m| m.len()).unwrap_or(0);
                    }
                }
            }
            if out_total > MAX_OUTPUT_BYTES {
                kill_tree(&mut child);
                break Err("出力が上限（512MB）を超えたため強制終了しました".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        },
    };

    // v1.4.9: プロセス終了後は stdin スロットを空に（以後の送信は「実行中なし」になる）。
    if let Some(slot) = stdin_slot {
        if let Ok(mut g) = slot.lock() {
            *g = None;
        }
    }

    // stderr の先頭だけ拾う（長大ログは切る）。先頭 ~500 字・trim 済み、無ければ空。
    let stderr_snip = || -> String {
        match std::fs::read(&err_path) {
            Ok(b) if !b.is_empty() => {
                let s = String::from_utf8_lossy(&b);
                s.trim().chars().take(500).collect()
            }
            _ => String::new(),
        }
    };
    // 完走したプロセスは exit code・stderr・出力をそのまま持ち上げる（成否判定はしない）。
    // Err は「完走しなかった」場合のみ（起動失敗・タイムアウト・中断・出力上限・監視失敗）。
    // v1.6 {out*}: フォルダ内の全ファイルを**ファイル名の辞書順**で収集（順序規約）。
    let collect_multi = || -> Option<Vec<(String, Vec<u8>)>> {
        if !multi_mode {
            return None;
        }
        let mut entries: Vec<_> = std::fs::read_dir(&outs_dir)
            .ok()?
            .flatten()
            .filter(|e| e.path().is_file())
            .collect();
        entries.sort_by_key(|e| e.file_name());
        Some(
            entries
                .into_iter()
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    std::fs::read(e.path()).ok().map(|b| (name, b))
                })
                .collect(),
        )
    };
    let outcome = match status {
        Err(e) => Err(e),
        Ok(s) => Ok(ExternalOutput {
            exit_code: s.code(),
            stderr: stderr_snip(),
            output: std::fs::read(&out_path).ok(),
            multi: collect_multi(),
        }),
    };
    cleanup_inputs(&in_paths);
    let _ = std::fs::remove_file(&out_path);
    let _ = std::fs::remove_file(&err_path);
    if multi_mode {
        let _ = std::fs::remove_dir_all(&outs_dir);
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(json: &str) -> FilterSpec {
        parse_spec(json).unwrap()
    }

    /// v1.8: 決定的な判定（V2.0課題 §8-2 の判定2段のうち①）。反復の停止条件がこれで、
    /// 将来のルーターカードの枝選択も同じ関数を使う——判定機構を1つにしておくのが狙い。
    #[test]
    fn judge_text_covers_the_deterministic_rules() {
        assert_eq!(judge_text("contains", "合格", "判定: 合格"), Ok(true));
        assert_eq!(judge_text("contains", "合格", "判定: 不足"), Ok(false));
        assert_eq!(judge_text("not_contains", "要修正", "判定: 良好"), Ok(true));
        assert_eq!(judge_text("regex", r"^\s*OK\b", "  OK です"), Ok(true));
        assert_eq!(judge_text("not_regex", r"\d+", "数字はない"), Ok(true));
        assert_eq!(judge_text("empty", "", "   \n "), Ok(true), "空白だけも空とみなす");
        assert_eq!(judge_text("not_empty", "", "x"), Ok(true));
        // 不正な正規表現・未知のモードはエラー。false を返すと「止まらない反復」になり、
        // しかも原因が画面のどこにも出ない。
        assert!(judge_text("regex", "(", "x").is_err());
        assert!(judge_text("いつか", "", "x").is_err());
    }

    /// v1.8 ③: 答え箱の付箋から「人が書いた分」だけを取り出す。`>` 行は質問であって答えでは
    /// ないので落とす —— 落とさないと、自動生成した質問文がそのまま答えとして下流へ流れ、
    /// 「答えていないのにフローが進む」という一番まずい壊れ方をする。
    #[test]
    fn answer_text_drops_the_quoted_question() {
        assert_eq!(answer_text("> この案でよいですか？"), "", "質問だけ＝未回答");
        assert_eq!(answer_text(""), "", "空の付箋＝未回答");
        assert_eq!(answer_text("   \n \n"), "", "空白だけ＝未回答");
        assert_eq!(
            answer_text("> この案でよいですか？\n\nはい。ただし価格は据え置きで。"),
            "はい。ただし価格は据え置きで。"
        );
        // 複数行の答えは形を保つ（箇条書きで答えることがある）。
        assert_eq!(answer_text("> 問\n・A\n・B"), "・A\n・B");
        // 引用は途中に混ざっていても質問扱い（付箋を書き足しても壊れない）。
        assert_eq!(answer_text("答えA\n> 問\n答えB"), "答えA\n答えB");
    }

    /// ヒューズ（§8-3「反復は max_fires 相当必須」）: spec の値をそのまま信じない。
    #[test]
    fn loop_rounds_are_clamped_to_the_fuse() {
        let clamp = |s: &FilterSpec| s.max_rounds.unwrap_or(3).clamp(1, LOOP_MAX_ROUNDS);
        assert_eq!(clamp(&spec(r#"{"builtin":"loop"}"#)), 3, "未指定は3周");
        assert_eq!(clamp(&spec(r#"{"builtin":"loop","max_rounds":0}"#)), 1, "0周は成立しない");
        assert_eq!(
            clamp(&spec(r#"{"builtin":"loop","max_rounds":100000}"#)),
            LOOP_MAX_ROUNDS,
            "巨大値でも天井で止まる"
        );
    }

    /// v1.7.4 生成系の制限時間の契約: 未記入（None）も 0 も**無制限**。
    /// 併せて gen_client が「無制限のとき timeout(None) を明示的に渡す」ことを守る
    /// —— reqwest の blocking ClientBuilder は timeout 既定が 30 秒で、呼ばなければ
    /// 無制限ではなく 30 秒になる（実機で空欄でも 30 秒で切れたバグの原因）。
    #[test]
    fn gen_timeout_treats_blank_and_zero_as_unlimited() {
        use std::time::Duration;
        assert_eq!(gen_timeout(None), None, "未記入は無制限");
        assert_eq!(gen_timeout(Some(0)), None, "0 は無制限");
        assert_eq!(gen_timeout(Some(600)), Some(Duration::from_secs(600)));
        // 無制限でもクライアントは作れる（timeout(None) が有効な引数であること）。
        assert!(gen_client(None).is_ok());
        assert!(gen_client(Some(0)).is_ok());
        assert!(gen_client(Some(600)).is_ok());
    }

    // v1.4.0 AIフィルタの実機検証。ライブの Ollama(localhost:11434) と qwen2.5:3b が要るので
    // 既定では無視。実行: `cargo test --lib -- --ignored ai_live`
    #[test]
    #[ignore = "ライブOllamaが必要"]
    fn ai_live_ollama_roundtrip() {
        let s = spec(
            r#"{"builtin":"ai","provider":"ollama","model":"qwen2.5:3b","system":"地名だけを一語で答えよ。"}"#,
        );
        let (out, ext, mime) = run_ai(&s, "日本の首都は？".as_bytes()).unwrap();
        let text = String::from_utf8(out).unwrap();
        // run_ai の契約は「非空のMDを返す」こと。回答内容の正確さはモデル依存なので問わない。
        assert_eq!(ext, "md");
        assert_eq!(mime, "text/markdown");
        assert!(!text.trim().is_empty(), "空の応答が返った");
        eprintln!("AI応答: {text}");
    }

    // v1.7.3 LM Studio 実機検証。ライブの LM Studio(localhost:1234) と gemma-4-e4b-it が
    // 要るので既定では無視。実行: `cargo test --lib -- --ignored ai_live_lmstudio`
    // 見ているのは「provider=lmstudio が OpenAI準拠の経路に乗り、既定エンドポイントで
    // 疎通し、ストリーミングの SSE を最後まで畳める」こと。
    #[test]
    #[ignore = "ライブLM Studioが必要"]
    fn ai_live_lmstudio_roundtrip() {
        let s = spec(
            r#"{"builtin":"ai","provider":"lmstudio","model":"gemma-4-e4b-it","system":"地名だけを一語で答えよ。","timeout_secs":0}"#,
        );
        let (out, ext, mime) = run_ai(&s, "日本の首都は？".as_bytes()).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!(ext, "md");
        assert_eq!(mime, "text/markdown");
        assert!(!text.trim().is_empty(), "空の応答が返った");
        eprintln!("AI応答(非ストリーム): {text}");

        // チャット側（SSE）も同じ spec で通ること。
        let msgs = vec![serde_json::json!({ "role": "user", "content": "日本の首都は？" })];
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let mut toks = 0usize;
        let full = run_ai_chat_stream(&s, &msgs, &cancel, &mut |_, _| toks += 1).unwrap();
        assert!(!full.trim().is_empty(), "ストリームが空だった");
        assert!(toks > 0, "トークンが1つも配信されなかった");
        eprintln!("AI応答(ストリーム/{toks}トークン): {full}");

        // モデル一覧＝設定ダイアログの「↻取得」と同じ経路。
        let models = list_models("lmstudio", "").unwrap();
        assert!(
            models.iter().any(|m| m == "gemma-4-e4b-it"),
            "一覧にモデルが見当たらない: {models:?}"
        );
    }

    // 回帰テスト: 本番(Tauri #[command(async)])と同じく tokio ランタイム文脈の中から
    // 同期 run_ai を呼ぶ。修正前はここで reqwest::blocking の内部ランタイム drop が
    // 「Cannot drop a runtime in a context where blocking is not allowed」でパニックして
    // アプリごと落ちた。thread::scope で素のOSスレッドに逃がす修正が効いていれば通る。
    // v1.11 変換モードの中断: 長い生成の途中で cancel を立てると、応答の完走を待たずに
    // 数秒で Err("中断されました") が返ること。実行:
    // `cargo test --lib -- --ignored ai_live_transform_cancel --nocapture`
    // （モデルは TEXOS_TEST_MODEL で差し替え可・既定 gemma4:latest）
    #[test]
    #[ignore = "ライブOllamaが必要"]
    fn ai_live_transform_cancel() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let model = std::env::var("TEXOS_TEST_MODEL").unwrap_or("gemma4:latest".into());
        let s = spec(&format!(r#"{{"builtin":"ai","provider":"ollama","model":"{model}"}}"#));
        // 先に1回走らせてモデルをロードしておく（ロード待ちと生成中の中断を区別する）。
        // 思考型モデルなら思考が本文から落ちていること（thinking_returned で確認）も見える。
        let mut wrec = None;
        let (warm, _, _) =
            run_ai_rec(&s, "「はい」とだけ答えよ。".as_bytes(), &AtomicBool::new(false), &mut wrec)
                .unwrap();
        let warm = String::from_utf8_lossy(&warm).into_owned();
        eprintln!("ウォームアップ: {} / 思考={}", warm.trim(), wrec.unwrap().thinking_returned);
        assert!(!warm.contains("<think"), "思考が本文に混ざった");
        let cancel = AtomicBool::new(false);
        let mut rec = None;
        let t0 = std::time::Instant::now();
        let res = std::thread::scope(|sc| {
            sc.spawn(|| {
                std::thread::sleep(std::time::Duration::from_secs(3));
                cancel.store(true, Ordering::Relaxed);
            });
            run_ai_rec(
                &s,
                "日本の歴史を縄文時代から令和まで、各時代5000字以上で詳しく書け。".as_bytes(),
                &cancel,
                &mut rec,
            )
        });
        let after_cancel = t0.elapsed().as_secs_f32() - 3.0;
        eprintln!("中断要求から {after_cancel:.2} 秒で戻った: {res:?}");
        assert!(after_cancel < 3.0, "中断が遅い: {after_cancel:.1} 秒");
        assert_eq!(res.err().as_deref(), Some("中断されました"));
        assert!(rec.is_some(), "中断時も実行条件の記録は残る");
    }

    #[test]
    #[ignore = "ライブOllamaが必要"]
    fn ai_inside_tokio_runtime_does_not_panic() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let res = rt.block_on(async {
            let s = spec(
                r#"{"builtin":"ai","provider":"ollama","model":"qwen2.5:3b","system":"一語で答えよ。"}"#,
            );
            run_ai(&s, "日本の首都は？".as_bytes())
        });
        let (out, ext, mime) = res.expect("tokio文脈内でrun_aiが失敗");
        assert_eq!(ext, "md");
        assert_eq!(mime, "text/markdown");
        assert!(!out.is_empty());
    }

    #[test]
    #[ignore = "ライブOllamaが必要"]
    fn ai_chat_stream_live() {
        use std::sync::atomic::AtomicBool;
        let s = spec(r#"{"builtin":"ai","provider":"ollama","model":"qwen2.5:3b"}"#);
        let msgs = vec![serde_json::json!({"role":"user","content":"1から5まで数えてください。"})];
        let cancel = AtomicBool::new(false);
        let mut tokens = 0usize;
        let mut acc = String::new();
        let full = run_ai_chat_stream(&s, &msgs, &cancel, &mut |kind, t| {
            // 戻り値の本文は Content だけ＝ここも Content だけ積めば一致するはず。
            if kind == ChatChunk::Content {
                tokens += 1;
                acc.push_str(t);
            }
        })
        .unwrap();
        eprintln!("tokens={tokens} full={full}");
        assert!(tokens > 1, "ストリームが断片で届いていない (tokens={tokens})");
        assert_eq!(full, acc, "返却本文と逐次累積が一致");
        assert!(!full.trim().is_empty());
    }

    // --- v1.8.6 Thinking: <think> 分離の状態機械 -------------------------------

    /// 1文字ずつ流し込む（最悪のチャンク分割）ヘルパ。タグが必ず割れる。
    fn split_by_chars(text: &str) -> (String, String) {
        let mut sp = ThinkSplitter::new();
        let (mut content, mut thinking) = (String::new(), String::new());
        let mut chunks: Vec<(ChatChunk, String)> = Vec::new();
        let mut buf = [0u8; 4];
        for ch in text.chars() {
            chunks.extend(sp.push(ch.encode_utf8(&mut buf)));
        }
        chunks.extend(sp.flush());
        for (k, t) in chunks {
            match k {
                ChatChunk::Content => content.push_str(&t),
                ChatChunk::Thinking => thinking.push_str(&t),
            }
        }
        (content, thinking)
    }

    #[test]
    fn think_splitter_separates_content_and_thinking() {
        let mut sp = ThinkSplitter::new();
        let mut got: Vec<(ChatChunk, String)> = Vec::new();
        got.extend(sp.push("前<think>考え中</think>後"));
        got.extend(sp.flush());
        let content: String = got
            .iter()
            .filter(|(k, _)| *k == ChatChunk::Content)
            .map(|(_, t)| t.as_str())
            .collect();
        let thinking: String = got
            .iter()
            .filter(|(k, _)| *k == ChatChunk::Thinking)
            .map(|(_, t)| t.as_str())
            .collect();
        assert_eq!(content, "前後");
        assert_eq!(thinking, "考え中");
    }

    #[test]
    fn think_splitter_survives_tags_split_across_chunks() {
        // ストリームは任意の位置で切れる。1文字ずつでも結果が変わらないこと。
        let (content, thinking) = split_by_chars("A<think>秘密</think>B");
        assert_eq!(content, "AB", "割れたタグが本文に漏れた");
        assert_eq!(thinking, "秘密");

        // <thinking> 表記も同じに扱う。
        let (content, thinking) = split_by_chars("<thinking>t</thinking>本文");
        assert_eq!(content, "本文");
        assert_eq!(thinking, "t");
    }

    #[test]
    fn think_splitter_keeps_plain_text_untouched() {
        // タグが無ければ 1 文字も変えない（"<" を含む本文も壊さない）。
        let src = "a < b && c<d です\n<html>タグの話</html>";
        let (content, thinking) = split_by_chars(src);
        assert_eq!(content, src);
        assert!(thinking.is_empty());
    }

    #[test]
    fn think_splitter_handles_unclosed_tag_at_eos() {
        // 中断・打ち切りで </think> が来ないまま終わっても、保留分を落とさない。
        let (content, thinking) = split_by_chars("本文<think>途中で");
        assert_eq!(content, "本文");
        assert_eq!(thinking, "途中で");
    }

    #[test]
    fn strip_think_blocks_removes_only_when_present() {
        assert_eq!(strip_think_blocks("<think>xy</think>答え"), "答え");
        // タグが無いときは trim もしない＝既存の変換出力を変えない。
        assert_eq!(strip_think_blocks("  答え\n"), "  答え\n");
    }

    #[test]
    fn gpu_and_think_options_map_to_ollama_params() {
        // v1.8.9 think は能力不明（info=None）なら設定どおり送る — 旧 ollama_think と同じ写像。
        let think = |s: &FilterSpec| crate::ai_models::plan_think("ollama", s.think.as_deref(), None, false).ollama_think;
        let auto = spec(r#"{"builtin":"ai","model":"m"}"#);
        assert_eq!(ollama_num_gpu(&auto), None, "既定はサーバ任せ＝キーを送らない");
        assert_eq!(think(&auto), None);
        assert_eq!(ollama_options(&auto), serde_json::json!({}), "既定は options を空に");

        let max = spec(r#"{"builtin":"ai","model":"m","gpu":"max"}"#);
        assert_eq!(ollama_num_gpu(&max), Some(999));

        let cpu = spec(r#"{"builtin":"ai","model":"m","gpu":"cpu"}"#);
        assert_eq!(ollama_num_gpu(&cpu), Some(0));

        let on = spec(r#"{"builtin":"ai","model":"m","think":"on"}"#);
        assert_eq!(think(&on), Some(serde_json::json!(true)));

        let high = spec(r#"{"builtin":"ai","model":"m","think":"high"}"#);
        assert_eq!(think(&high), Some(serde_json::json!("high")));

        let off = spec(r#"{"builtin":"ai","model":"m","think":"off"}"#);
        assert_eq!(think(&off), None);

        // v1.8.9 num_ctx は options.num_ctx へ（0 は未指定扱い）。
        let ctx = spec(r#"{"builtin":"ai","model":"m","num_ctx":16384,"temperature":0.2}"#);
        assert_eq!(ollama_options(&ctx), serde_json::json!({"temperature":0.2,"num_ctx":16384}));
        let zero = spec(r#"{"builtin":"ai","model":"m","num_ctx":0}"#);
        assert_eq!(ollama_options(&zero), serde_json::json!({}));
    }

    #[test]
    fn extra_params_merge_top_level_and_reject_reserved() {
        let s = spec(r#"{"builtin":"ai","model":"m","send_reasoning":true,"extra_params":{"reasoning_budget_tokens":2000,"keep_alive":"30m"}}"#);
        assert!(s.send_reasoning);
        let mut body = serde_json::json!({"model":"m","stream":false,"keep_alive":"5m"});
        apply_extra_params(&mut body, &s).unwrap();
        assert_eq!(body["reasoning_budget_tokens"], 2000);
        assert_eq!(body["keep_alive"], "30m", "予約外のキーは上書きできる");

        // 未指定・空は何もしない。send_reasoning の既定は false。
        let none = spec(r#"{"builtin":"ai","model":"m"}"#);
        assert!(!none.send_reasoning);
        let mut b = serde_json::json!({"model":"m"});
        apply_extra_params(&mut b, &none).unwrap();
        assert_eq!(b, serde_json::json!({"model":"m"}));

        // 他の欄が管理するキーは拒否（本体は変えない）。
        let bad = spec(r#"{"builtin":"ai","model":"m","extra_params":{"stream":true,"reasoning_effort":"xhigh","x":1}}"#);
        let mut b = serde_json::json!({"model":"m","stream":false});
        let e = apply_extra_params(&mut b, &bad).unwrap_err();
        assert!(e.contains("stream") && e.contains("reasoning_effort") && !e.contains(" x"), "{e}");
        assert_eq!(b["stream"], false);
    }

    // 実機検証（ライブの Strata が要る・http://127.0.0.1:8080/v1）。
    // 実行: `cargo test --lib -- --ignored strata_live --nocapture`
    #[test]
    #[ignore = "ライブStrataが必要"]
    fn strata_live_reasoning_effort_and_budget() {
        let model = reqwest::blocking::get("http://127.0.0.1:8080/v1/models").unwrap().json::<serde_json::Value>().unwrap()
            ["data"][0]["id"].as_str().unwrap().to_string();
        let run = |extra: &str| {
            let s = spec(&format!(
                r#"{{"builtin":"ai","provider":"openai","endpoint":"http://127.0.0.1:8080/v1","model":"{model}","timeout_secs":0{extra}}}"#
            ));
            let mut rec = None;
            let t = std::time::Instant::now();
            let out = run_ai_rec(&s, "17*23は？答えの数字だけ".as_bytes(), &std::sync::atomic::AtomicBool::new(false), &mut rec);
            let rec = rec.unwrap();
            eprintln!("{extra:70} {:>5.1}s think={} sent={:?} → {:?}", t.elapsed().as_secs_f32(), rec.thinking_returned, rec.sent, out.as_ref().map(|o| String::from_utf8_lossy(&o.0).trim().to_string()));
            (out, rec)
        };
        // 送らない（既定）: 従来どおり reasoning_effort なし。
        let (o, r) = run(r#","think":"low""#);
        assert!(o.is_ok() && r.sent.reasoning_effort.is_none());
        // 送る: 使わない→none（思考が返らない）・浅く→low・深く→high。どれも 400 にならない。
        let (o, r) = run(r#","think":"off","send_reasoning":true"#);
        assert!(o.is_ok());
        assert_eq!(r.sent.reasoning_effort.as_deref(), Some("none"));
        assert!(!r.thinking_returned, "none で思考が止まる");
        for lv in ["low", "medium", "high"] {
            let (o, r) = run(&format!(r#","think":"{lv}","send_reasoning":true"#));
            assert!(o.is_ok(), "{lv}: {o:?}");
            assert_eq!(r.sent.reasoning_effort.as_deref(), Some(lv));
        }
        // 追加パラメータ: 思考上限を小さくしても回答まで届く。
        let (o, r) = run(r#","think":"high","send_reasoning":true,"extra_params":{"reasoning_budget_tokens":64}"#);
        assert!(o.is_ok(), "{o:?}");
        assert!(r.sent.extra.is_some());
    }

    #[test]
    fn api_error_text_extracts_reason() {
        let s = reqwest::StatusCode::BAD_REQUEST;
        assert_eq!(
            api_error_text(s, r#"{"error":"\"gemma3:latest\" does not support thinking"}"#),
            "AI APIエラー (400 Bad Request): \"gemma3:latest\" does not support thinking"
        );
        assert!(api_error_text(s, r#"{"error":{"message":"Invalid 'reasoning_effort'"}}"#).ends_with("Invalid 'reasoning_effort'"));
        assert_eq!(api_error_text(s, ""), "AI APIエラー (400 Bad Request)");
    }

    #[test]
    fn md_codeblock_extracts_fenced_code() {
        let md = "説明文\n```c\n#include <stdio.h>\nint main(){return 0;}\n```\n末尾の解説。";
        let s = spec(r#"{"builtin":"md_codeblock","lang":"c"}"#);
        let (out, ext, mime) = run_builtin(&s, "md_codeblock", md.as_bytes()).unwrap();
        let code = String::from_utf8(out).unwrap();
        assert_eq!(ext, "c");
        assert_eq!(mime, "text/plain");
        assert!(code.starts_with("#include <stdio.h>"));
        assert!(!code.contains("```"), "フェンスが残っている");
        assert!(!code.contains("説明文"), "ブロック外の文が混入");
        assert!(code.ends_with('\n'), "末尾改行が無い");
    }

    #[test]
    fn md_codeblock_first_block_when_no_lang() {
        let md = "```py\nprint(1)\n```\n```js\nconsole.log(2)\n```";
        let s = spec(r#"{"builtin":"md_codeblock"}"#);
        let (out, ext, _) = run_builtin(&s, "md_codeblock", md.as_bytes()).unwrap();
        assert_eq!(ext, "py"); // 言語トークン py → 拡張子 py
        assert_eq!(String::from_utf8(out).unwrap(), "print(1)\n");
    }

    #[test]
    fn md_codeblock_missing_block_is_error() {
        let s = spec(r#"{"builtin":"md_codeblock","lang":"rust"}"#);
        assert!(run_builtin(&s, "md_codeblock", b"plain text only").is_err());
    }

    #[test]
    fn mime_matching_rules() {
        assert!(mime_matches("*/*", "application/pdf"));
        assert!(mime_matches("", "image/png"));
        assert!(mime_matches("text/*", "text/plain"));
        assert!(mime_matches("text/*", "application/json")); // JSONはテキスト扱い
        assert!(!mime_matches("text/*", "image/png"));
        assert!(mime_matches("image/*", "image/webp"));
        assert!(mime_matches("application/pdf", "application/pdf"));
        assert!(!mime_matches("application/pdf", "application/zip"));
    }

    #[test]
    fn text_grep_keeps_matching_lines() {
        let s = spec(r#"{"builtin":"text_grep","pattern":"foo"}"#);
        let (out, ext, mime) = run_builtin(&s, "text_grep", b"foo bar\nbaz\nfoofoo").unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "foo bar\nfoofoo");
        assert_eq!(ext, "txt");
        assert_eq!(mime, "text/plain");
    }

    #[test]
    fn text_replace_uses_regex() {
        let s = spec(r#"{"builtin":"text_replace","pattern":"a+","replacement":"X"}"#);
        let (out, _, _) = run_builtin(&s, "text_replace", b"aaa b aa").unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "X b X");
    }

    #[test]
    fn invalid_regex_is_a_clean_error() {
        let s = spec(r#"{"builtin":"text_grep","pattern":"("}"#);
        assert!(run_builtin(&s, "text_grep", b"x").is_err());
    }

    // --- v1.5.12 P1: Unix標準テキストフィルタ ---------------------------------

    #[test]
    fn text_head_and_tail_take_lines() {
        let input = b"1\n2\n3\n4\n5";
        let s = spec(r#"{"builtin":"text_head","lines":2}"#);
        let (out, ext, mime) = run_builtin(&s, "text_head", input).unwrap();
        assert_eq!(out, b"1\n2");
        assert_eq!((ext.as_str(), mime.as_str()), ("txt", "text/plain"));
        let s = spec(r#"{"builtin":"text_tail","lines":2}"#);
        let (out, _, _) = run_builtin(&s, "text_tail", input).unwrap();
        assert_eq!(out, b"4\n5");
        // 行数より短い入力は全行そのまま（既定10行）
        let s = spec(r#"{"builtin":"text_tail"}"#);
        let (out, _, _) = run_builtin(&s, "text_tail", b"a\nb").unwrap();
        assert_eq!(out, b"a\nb");
    }

    #[test]
    fn text_sort_orders_lines() {
        let s = spec(r#"{"builtin":"text_sort"}"#);
        let (out, _, _) = run_builtin(&s, "text_sort", b"b\na\nc\na").unwrap();
        assert_eq!(out, b"a\na\nb\nc");
        let s = spec(r#"{"builtin":"text_sort","order":"desc","unique":"remove"}"#);
        let (out, _, _) = run_builtin(&s, "text_sort", b"b\na\nc\na").unwrap();
        assert_eq!(out, b"c\nb\na");
        // 数値比較: 辞書順だと "10" < "2" になる並びが数値で正しく並ぶ
        let s = spec(r#"{"builtin":"text_sort","compare":"number"}"#);
        let (out, _, _) = run_builtin(&s, "text_sort", b"10 x\n2 y\n-1 z").unwrap();
        assert_eq!(out, b"-1 z\n2 y\n10 x");
    }

    #[test]
    fn text_count_reports_wc() {
        let s = spec(r#"{"builtin":"text_count","mode":"lines"}"#);
        let (out, _, _) = run_builtin(&s, "text_count", b"a b\nc\n").unwrap();
        assert_eq!(out, b"2");
        // 既定はまとめレポート。文字数は Unicode スカラー数（バイト数ではない）
        let s = spec(r#"{"builtin":"text_count"}"#);
        let (out, _, _) = run_builtin(&s, "text_count", "あい u\n".as_bytes()).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "行数: 1\n単語数: 2\n文字数: 5"
        );
    }

    #[test]
    fn text_cut_picks_fields() {
        let s = spec(r#"{"builtin":"text_cut","delimiter":"comma","fields":"1,3"}"#);
        let (out, _, _) = run_builtin(&s, "text_cut", b"a,b,c\nx,y,z").unwrap();
        assert_eq!(out, b"a,c\nx,z");
        // 空白区切りは連続空白をまとめ、出力はタブ区切りに揃える。"2-"=2列目から末尾
        let s = spec(r#"{"builtin":"text_cut","delimiter":"space","fields":"2-"}"#);
        let (out, _, _) = run_builtin(&s, "text_cut", b"one  two three").unwrap();
        assert_eq!(out, b"two\tthree");
        // 範囲外の列は黙って落ちる（行ごとに列数が違ってもエラーにしない）
        let s = spec(r#"{"builtin":"text_cut","fields":"5"}"#);
        let (out, _, _) = run_builtin(&s, "text_cut", b"a\tb").unwrap();
        assert_eq!(out, b"");
        // 不正な列指定はきれいなエラー
        let s = spec(r#"{"builtin":"text_cut","fields":"0"}"#);
        assert!(run_builtin(&s, "text_cut", b"a\tb").is_err());
        let s = spec(r#"{"builtin":"text_cut","fields":""}"#);
        assert!(run_builtin(&s, "text_cut", b"a\tb").is_err());
    }

    #[test]
    fn text_convert_decodes_to_utf8() {
        let src = "日本語のテキストです。文字コード変換のテスト。\n二行目もあります。";
        let (sjis, _, _) = encoding_rs::SHIFT_JIS.encode(src);
        // auto: chardetng 推定で Shift_JIS を当てる
        let s = spec(r#"{"builtin":"text_convert"}"#);
        let (out, ext, mime) = run_builtin(&s, "text_convert", &sjis).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), src);
        assert_eq!((ext.as_str(), mime.as_str()), ("txt", "text/plain"));
        // 明示指定（encoding_rs のラベル）でも同じ結果
        let s = spec(r#"{"builtin":"text_convert","encoding":"shift_jis"}"#);
        let (out, _, _) = run_builtin(&s, "text_convert", &sjis).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), src);
        // UTF-8 入力はそのまま素通し
        let s = spec(r#"{"builtin":"text_convert"}"#);
        let (out, _, _) = run_builtin(&s, "text_convert", src.as_bytes()).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), src);
        // 未知のラベルはきれいなエラー
        let s = spec(r#"{"builtin":"text_convert","encoding":"nazo"}"#);
        assert!(run_builtin(&s, "text_convert", b"x").is_err());
    }

    // --- v1.5.17 P2: 集約系（run_builtin_agg）---

    #[test]
    fn text_concat_joins_inputs_in_order() {
        // 既定（newline）: 改行で終わらない入力の後ろでも行が融合しない
        let s = spec(r#"{"builtin":"text_concat"}"#);
        let ins: Vec<&[u8]> = vec![b"a\nb", b"c\n", b"d"];
        let (out, ext, mime) = run_builtin_agg(&s, "text_concat", &ins).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "a\nb\nc\nd");
        assert_eq!((ext.as_str(), mime.as_str()), ("txt", "text/plain"));
        // none = 純 cat（バイト列をそのまま連結 → 行が融合する）
        let s = spec(r#"{"builtin":"text_concat","separator":"none"}"#);
        let (out, _, _) = run_builtin_agg(&s, "text_concat", &ins).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "a\nbc\nd");
        // blank = 空行を挟む / rule = Markdown 区切り線（前後に空行）
        let s = spec(r#"{"builtin":"text_concat","separator":"blank"}"#);
        let ins2: Vec<&[u8]> = vec![b"a", b"b"];
        let (out, _, _) = run_builtin_agg(&s, "text_concat", &ins2).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "a\n\nb");
        let s = spec(r#"{"builtin":"text_concat","separator":"rule"}"#);
        let (out, _, _) = run_builtin_agg(&s, "text_concat", &ins2).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "a\n\n---\n\nb");
        // 入力1個はそのまま
        let s = spec(r#"{"builtin":"text_concat"}"#);
        let (out, _, _) = run_builtin_agg(&s, "text_concat", &[b"solo".as_slice()]).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "solo");
    }

    #[test]
    fn text_diff_unified_two_inputs() {
        let s = spec(r#"{"builtin":"text_diff"}"#);
        let old = b"line1\nline2\nline3\n".as_slice();
        let new = b"line1\nlineX\nline3\n".as_slice();
        let (out, ext, mime) = run_builtin_agg(&s, "text_diff", &[old, new]).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("-line2"), "{text}");
        assert!(text.contains("+lineX"), "{text}");
        assert!(text.contains("入力1（旧）"), "{text}");
        assert_eq!((ext.as_str(), mime.as_str()), ("diff", "text/plain"));
        // 同一入力 → 出力は空（Unix diff と同じ）
        let (out, _, _) = run_builtin_agg(&s, "text_diff", &[old, old]).unwrap();
        assert!(out.is_empty());
        // 入力が2つでなければ明確なエラー
        assert!(run_builtin_agg(&s, "text_diff", &[old]).is_err());
        assert!(run_builtin_agg(&s, "text_diff", &[old, new, old]).is_err());
    }

    #[test]
    fn text_paste_joins_lines_side_by_side() {
        // 既定はタブ区切り。行数が違えば空文字で埋める（GNU paste と同じ）
        let s = spec(r#"{"builtin":"text_paste"}"#);
        let ins: Vec<&[u8]> = vec![b"a1\na2\na3", b"b1\nb2"];
        let (out, ext, mime) = run_builtin_agg(&s, "text_paste", &ins).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "a1\tb1\na2\tb2\na3\t");
        assert_eq!((ext.as_str(), mime.as_str()), ("txt", "text/plain"));
        // カンマ区切り（CSV 組み立て）
        let s = spec(r#"{"builtin":"text_paste","delimiter":"comma"}"#);
        let (out, _, _) = run_builtin_agg(&s, "text_paste", &ins).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "a1,b1\na2,b2\na3,");
    }

    #[test]
    fn img_convert_roundtrip() {
        // 4x4 PNG を作って BMP へ変換 → デコードできること
        let img = image::DynamicImage::ImageRgba8(image::RgbaImage::new(4, 4));
        let mut png = std::io::Cursor::new(Vec::new());
        img.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let s = spec(r#"{"builtin":"img_convert","format":"bmp"}"#);
        let (out, ext, mime) = run_builtin(&s, "img_convert", png.get_ref()).unwrap();
        assert_eq!(ext, "bmp");
        assert_eq!(mime, "image/bmp");
        assert!(image::load_from_memory(&out).is_ok());
    }

    fn no_cancel() -> std::sync::atomic::AtomicBool {
        std::sync::atomic::AtomicBool::new(false)
    }

    /// speak: 空テキストは黙って失敗しない（明確なエラー）。実際の発話は実機確認で。
    #[test]
    fn speak_rejects_empty_text() {
        assert!(run_speak(b"").is_err());
        assert!(run_speak(b"  \n\t").is_err());
    }

    /// プログラム解決の順序: 明示パス → .texos/filters → 環境変数PATH。
    #[cfg(windows)]
    #[test]
    fn resolve_program_search_order() {
        let tmp = std::env::temp_dir().join(format!("texos_rp_{}", std::process::id()));
        let filters = tmp.join("filters");
        std::fs::create_dir_all(&filters).unwrap();
        let bundled = filters.join("prog.exe");
        std::fs::write(&bundled, b"x").unwrap();
        // filters 同梱が見つかる
        assert_eq!(resolve_program("prog.exe", None, &filters), Some(bundled.clone()));
        // 明示パス（ファイル）が最優先
        let explicit = tmp.join("explicit.exe");
        std::fs::write(&explicit, b"x").unwrap();
        assert_eq!(
            resolve_program("prog.exe", Some(explicit.to_str().unwrap()), &filters),
            Some(explicit)
        );
        // 明示パスがフォルダなら name を結合
        assert_eq!(
            resolve_program("prog.exe", Some(filters.to_str().unwrap()), &filters),
            Some(bundled)
        );
        // どこにも無ければ環境変数 PATH へフォールバック
        assert!(resolve_program("cmd.exe", None, &filters).is_some());
        assert!(resolve_program("texos_no_such_prog.exe", None, &filters).is_none());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    fn one(bytes: &'static [u8]) -> Vec<(&'static [u8], &'static str)> {
        vec![(bytes, "txt")]
    }

    /// 引用符付きパスのコマンドが cmd /C 経由で動くこと（raw_arg 修正の回帰テスト）。
    #[cfg(windows)]
    #[test]
    fn external_with_quoted_paths_runs() {
        let out =
            run_external("copy \"{in}\" \"{out}\"", &one(b"hello"), "txt", None, &no_cancel())
                .unwrap();
        assert_eq!(out, b"hello");
    }

    /// {out_noext} が拡張子を除いたパスに展開され、ツールが付ける `.txt` が {out} と
    /// 一致して読み戻せること（whisper.cpp の `-of <ベース名>` 想定の回帰テスト）。
    #[cfg(windows)]
    #[test]
    fn out_noext_placeholder_strips_extension() {
        let out = run_external(
            "copy \"{in}\" \"{out_noext}.txt\"",
            &one(b"hello"),
            "txt",
            None,
            &no_cancel(),
        )
        .unwrap();
        assert_eq!(out, b"hello");
    }

    /// 標準入出力フィルタの形（`prog < {in} > {out}`）がリダイレクトで動くこと。
    #[cfg(windows)]
    #[test]
    fn external_stdio_redirection_runs() {
        let out = run_external(
            "sort < \"{in}\" > \"{out}\"",
            &one(b"b\r\na\r\n"),
            "txt",
            None,
            &no_cancel(),
        )
        .unwrap();
        assert!(String::from_utf8_lossy(&out).starts_with('a'));
    }

    /// v1.6 {out*}: 複数ファイル書き出しモード。フォルダに書かれた全ファイルが
    /// **ファイル名の辞書順**で multi に収まり、単一出力の output とは独立であること。
    #[cfg(windows)]
    #[test]
    fn out_star_collects_files_in_lexicographic_order() {
        let o = run_external_full(
            // 逆順に書いても収集は辞書順（02 → 10 → aa）になることを見る
            "(echo B> \"{out*}\\10_b.txt\") & (echo C> \"{out*}\\aa_c.txt\") & (echo A> \"{out*}\\02_a.txt\")",
            &[],
            "txt",
            Some(10),
            &no_cancel(),
            None,
            None,
        )
        .unwrap();
        assert!(o.success());
        let multi = o.multi.expect("{out*} モードなのに multi が None");
        let names: Vec<&str> = multi.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["02_a.txt", "10_b.txt", "aa_c.txt"]);
        assert!(multi[0].1.starts_with(b"A"));
        assert!(multi[2].1.starts_with(b"C"));
        // {out} は書いていないので単一出力側は None（独立性）
        assert!(o.output.is_none());
    }

    /// v1.6 {out*}: 従来の単一出力 {out} では multi が立たないこと（回帰）。
    #[cfg(windows)]
    #[test]
    fn single_out_does_not_populate_multi() {
        let o = run_external_full(
            "copy \"{in}\" \"{out}\"",
            &one(b"hello"),
            "txt",
            Some(10),
            &no_cancel(),
            None,
            None,
        )
        .unwrap();
        assert!(o.multi.is_none());
        assert_eq!(o.output.as_deref(), Some(b"hello".as_slice()));
    }

    /// v1.6 {out*}: プログラムが1つも書かなければ multi は Some(空)（成功だが出力ゼロ）。
    /// 「出力がありません」エラー化は flow_run 側のポリシー（into_bytes と同じ層）。
    #[cfg(windows)]
    #[test]
    fn out_star_empty_folder_yields_empty_multi() {
        let o = run_external_full(
            "echo done> nul & echo x \"{out*}\" > nul",
            &[],
            "txt",
            Some(10),
            &no_cancel(),
            None,
            None,
        )
        .unwrap();
        assert!(o.success());
        assert_eq!(o.multi.as_deref(), Some(&[][..]));
    }

    /// v1.6 パイロット（実機）: TexoS の Generator ノードが組む cmdline で
    /// ChillChordMaker のヘッドレスアダプタ（cli_render.py）を駆動し、
    /// `{out}` に書かれた WAV を読み戻せること。入力なし＝`{in}`なし＝既定パッチ。
    /// これは「`{out}` 契約は既にある／サウンドエンジン単体でカード化できる」の実証。
    /// python + I:\ChillChordMaker + numpy/scipy が要るので既定では無視。
    /// 実行: `cargo test --lib -- --ignored chillchord`
    #[cfg(windows)]
    #[test]
    #[ignore = "実機: python + I:\\ChillChordMaker + numpy/scipy が必要"]
    fn chillchord_generator_produces_wav() {
        let cmd = "python \"I:\\ChillChordMaker\\cli_render.py\" --out \"{out}\"";
        let out = run_external(cmd, &[], "wav", Some(60), &no_cancel())
            .expect("cli_render.py が WAV を書けなかった");
        // WAV マジック: "RIFF" .... "WAVE"（44バイトの標準ヘッダ以上）
        assert!(out.len() > 44, "WAVヘッダ長に満たない: {}バイト", out.len());
        assert_eq!(&out[0..4], b"RIFF", "RIFF マジックが無い");
        assert_eq!(&out[8..12], b"WAVE", "WAVE マジックが無い");
    }

    /// v1.6 ステップ2（実機）: 進行モード＝カードの cmdline
    /// `python cli_render.py --in {in} --outdir {out*}` で複数 WAV が
    /// ファイル名の辞書順（＝進行順）で収集されること。Two-Five-One は
    /// Dm7 → G7 → CM7 の3ファイルになるはず（コード名までピン留め）。
    #[cfg(windows)]
    #[test]
    #[ignore = "実機: python + I:\\ChillChordMaker + numpy/scipy が必要"]
    fn chillchord_progression_produces_multiple_wavs() {
        let json = br#"{"progression_name":"Two-Five-One","key_note":"C","key_oct":3,
            "bpm":120,"prefix":"t","params":{"note_duration":1.0}}"#;
        let cmd =
            "python \"I:\\ChillChordMaker\\cli_render.py\" --in \"{in}\" --outdir \"{out*}\"";
        let o = run_external_full(
            cmd,
            &[(json.as_slice(), "json")],
            "wav",
            Some(120),
            &no_cancel(),
            None,
            None,
        )
        .expect("cli_render.py 進行モードが完走しなかった");
        assert!(o.success(), "exit {:?} stderr: {}", o.exit_code, o.stderr);
        let multi = o.multi.expect("{out*} なのに multi が None");
        let names: Vec<&str> = multi.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            vec!["t_01_1_Dm7.wav", "t_01_3_G7.wav", "t_02_1_CM7.wav"],
            "Two-Five-One (C) の3コードが進行順で並ぶはず"
        );
        for (n, b) in &multi {
            assert!(
                b.len() > 44 && &b[0..4] == b"RIFF" && &b[8..12] == b"WAVE",
                "{n} が WAV でない"
            );
        }
    }

    /// フィルタ型プログラムを引数型で誤指定（入力リダイレクトなし）してもストール
    /// しないこと — stdin が閉じているので sort は即 EOF を受けて空出力で終わる。
    #[cfg(windows)]
    #[test]
    fn external_stdin_is_closed_so_filter_programs_do_not_stall() {
        let out =
            run_external("sort > \"{out}\"", &one(b"ignored"), "txt", Some(10), &no_cancel())
                .unwrap();
        assert!(out.is_empty() || out == b"\r\n");
    }

    /// 終わらないコマンドはタイムアウトでツリーごと強制終了されること。
    #[cfg(windows)]
    #[test]
    fn external_times_out_and_kills() {
        let started = std::time::Instant::now();
        let err = run_external(
            "ping -n 30 127.0.0.1 > \"{out}\"",
            &one(b""),
            "txt",
            Some(1),
            &no_cancel(),
        )
        .unwrap_err();
        assert!(err.contains("タイムアウト"), "{err}");
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }

    /// cancel フラグ（UIの中断ボタン）で実行中のコマンドが止まること。
    #[cfg(windows)]
    #[test]
    fn external_cancel_breaks_running_command() {
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let c2 = cancel.clone();
        let killer = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(500));
            c2.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        let started = std::time::Instant::now();
        let err = run_external(
            "ping -n 30 127.0.0.1 > \"{out}\"",
            &one(b""),
            "txt",
            Some(60),
            &cancel,
        )
        .unwrap_err();
        killer.join().unwrap();
        assert!(err.contains("中断"), "{err}");
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }

    /// 集約モードの判定: {in*} か {in2} 以降があれば集約、{in}/{in1} だけなら個別。
    #[test]
    fn aggregate_detection() {
        assert!(cmd_is_aggregate("{in*} -o \"{out}\""));
        assert!(cmd_is_aggregate("\"{in1}\" \"{in2}\" -composite \"{out}\""));
        assert!(!cmd_is_aggregate("\"{in}\" \"{out}\""));
        assert!(!cmd_is_aggregate("< \"{in1}\" > \"{out}\""));
    }

    /// 位置指定 {in1}+{in2} で2入力を連結できること（接続順が保たれる）。
    #[cfg(windows)]
    #[test]
    fn external_positional_two_inputs() {
        let inputs: Vec<(&[u8], &str)> = vec![(b"foo", "txt"), (b"bar", "txt")];
        let out = run_external(
            "copy \"{in1}\"+\"{in2}\" \"{out}\"",
            &inputs,
            "txt",
            None,
            &no_cancel(),
        )
        .unwrap();
        assert!(out.starts_with(b"foobar"), "{:?}", String::from_utf8_lossy(&out));
    }

    /// {in*} が全入力のクォート済みパス列に展開されること。
    #[cfg(windows)]
    #[test]
    fn external_in_star_expands_all_inputs() {
        let inputs: Vec<(&[u8], &str)> = vec![(b"a", "txt"), (b"b", "txt")];
        let out = run_external("echo {in*} > \"{out}\"", &inputs, "txt", None, &no_cancel())
            .unwrap();
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains("_1.txt") && s.contains("_2.txt"), "{s}");
    }

    /// 入力本数より大きい {inN} は実行前にエラーになること。
    #[test]
    fn external_missing_positional_input_is_an_error() {
        let inputs: Vec<(&[u8], &str)> = vec![(b"only", "txt")];
        let err = run_external(
            "copy \"{in2}\" \"{out}\"",
            &inputs,
            "txt",
            None,
            &no_cancel(),
        )
        .unwrap_err();
        assert!(err.contains("{in2}"), "{err}");
    }

    /// §4.2 ノード種別判定が現行 eval_filter の分類と一致すること。
    #[test]
    fn node_kind_classification() {
        assert_eq!(spec(r#"{"type":"generator","name":"rec.exe"}"#).node_kind(), NodeKind::Generator);
        assert_eq!(spec(r#"{"builtin":"ai","model":"x"}"#).node_kind(), NodeKind::Ai);
        assert_eq!(spec(r#"{"builtin":"t2i","model":"sd1.5"}"#).node_kind(), NodeKind::T2i);
        assert_eq!(spec(r#"{"builtin":"t23d","model":"hunyuan3d-2"}"#).node_kind(), NodeKind::T23d);
        assert_eq!(spec(r#"{"builtin":"text_grep"}"#).node_kind(), NodeKind::Builtin);
        assert_eq!(spec(r#"{"name":"sp2tab.exe"}"#).node_kind(), NodeKind::External);
        assert_eq!(spec(r#"{"cmd":"sort < {in} > {out}"}"#).node_kind(), NodeKind::External);
        assert_eq!(spec(r#"{}"#).node_kind(), NodeKind::Invalid);
        // generator は builtin/name より優先（eval_filter の早期 return と同順）。
        assert_eq!(
            spec(r#"{"type":"generator","builtin":"text_grep","name":"x"}"#).node_kind(),
            NodeKind::Generator
        );
    }

    /// 集約判定: External は CMD の書き方（{in*}/{in2}〜）で決まる。builtin は名前で
    /// 静的に決まる（v1.5.17 P2: concat/diff/paste）— {in*} を書いても関係しない。
    #[test]
    fn is_aggregate_for_external_and_builtin() {
        assert!(spec(r#"{"cmd":"{in*} -o {out}"}"#).is_aggregate());
        assert!(spec(r#"{"name":"convert","cmd":"\"{in1}\" \"{in2}\" {out}"}"#).is_aggregate());
        assert!(!spec(r#"{"cmd":"\"{in}\" {out}"}"#).is_aggregate());
        assert!(!spec(r#"{"builtin":"text_grep","cmd":"{in*}"}"#).is_aggregate());
        // v1.5.17 P2: 集約系 builtin
        assert!(spec(r#"{"builtin":"text_concat"}"#).is_aggregate());
        assert!(spec(r#"{"builtin":"text_diff"}"#).is_aggregate());
        assert!(spec(r#"{"builtin":"text_paste"}"#).is_aggregate());
        assert!(!spec(r#"{"builtin":"text_sort"}"#).is_aggregate());
    }

    /// 自己完結コマンド判定: {out}/{out_noext} を書かない外部・ジェネレータは fire-and-forget
    /// （カメラ起動・読み上げ）。{out} を書けば従来どおり出力必須。
    #[test]
    fn is_self_contained_detection() {
        // {out} なし → 自己完結（カメラ起動・読み上げ）
        assert!(spec(r#"{"type":"generator","cmd":"start microsoft.windows.camera:"}"#).is_self_contained());
        assert!(spec(r#"{"cmd":"start microsoft.windows.camera:"}"#).is_self_contained());
        // {out}/{out_noext} を書くなら出力必須（従来どおり）
        assert!(!spec(r#"{"type":"generator","cmd":"> \"{out}\""}"#).is_self_contained());
        assert!(!spec(r#"{"cmd":"whisper {in} -o \"{out_noext}\""}"#).is_self_contained());
        // builtin/ai/t2i/invalid は外部コマンドではないので対象外
        assert!(!spec(r#"{"builtin":"text_grep"}"#).is_self_contained());
        assert!(!spec(r#"{}"#).is_self_contained());
        // v1.5.15: speak builtin は「呼ぶだけ」= 出力ゼロが正常（終端の入力なし案内を出さない）
        assert!(spec(r#"{"builtin":"speak","accept_mime":"text/*"}"#).is_self_contained());
    }

    /// shell 判定（run_flow 終端の対話シェル）。
    #[test]
    fn is_shell_detection() {
        assert!(spec(r#"{"shell":"cmd"}"#).is_shell());
        assert!(!spec(r#"{"cmd":"sort"}"#).is_shell());
    }

    /// v1.5.2 WS-C: 資源セマフォのキー導出 — AI エンドポイントの実効値（run_ai の既定と一致）。
    #[test]
    fn effective_ai_endpoint_defaults() {
        assert_eq!(
            effective_ai_endpoint(&spec(r#"{"builtin":"ai","model":"x"}"#)),
            "http://localhost:11434"
        );
        assert_eq!(
            effective_ai_endpoint(&spec(r#"{"builtin":"ai","model":"x","provider":"openai"}"#)),
            "http://localhost:1234/v1"
        );
        assert_eq!(
            effective_ai_endpoint(&spec(
                r#"{"builtin":"ai","model":"x","endpoint":" http://192.168.1.5:11434 "}"#
            )),
            "http://192.168.1.5:11434"
        );
        // v1.7.3 lmstudio は openai と同じ既定（ワイヤ形式が同一・別名は案内と検査のため）。
        assert_eq!(
            effective_ai_endpoint(&spec(r#"{"builtin":"ai","model":"x","provider":"lmstudio"}"#)),
            "http://localhost:1234/v1"
        );
        // 明示エンドポイントは provider に関わらず優先（遠隔の LM Studio を指せる）。
        assert_eq!(
            effective_ai_endpoint(&spec(
                r#"{"builtin":"ai","model":"x","provider":"lmstudio","endpoint":"http://192.168.1.9:1234/v1"}"#
            )),
            "http://192.168.1.9:1234/v1"
        );
        assert!(is_openai_compat("openai") && is_openai_compat("lmstudio"));
        assert!(!is_openai_compat("ollama"));
        // 未知の provider は Ollama 扱いに落ちる（既存カードの挙動を変えない）。
        assert!(!is_openai_compat("anthropic"));
    }

    /// v1.5.3 WS-P1: エッジ属性のパース（NULL/壊れたJSON=既定＝従来）とセレクタ判定。
    #[test]
    fn edge_attrs_and_select_kind() {
        let a = parse_edge_attrs(Some(r#"{"when":"failure","carry":"input","port":2}"#));
        assert_eq!(a.when.as_deref(), Some("failure"));
        assert_eq!(a.carry.as_deref(), Some("input"));
        assert_eq!(a.port, Some(2));
        let d = parse_edge_attrs(None);
        assert!(d.when.is_none() && d.carry.is_none() && d.port.is_none());
        assert!(d.role.is_none(), "role 無し＝ふつうの入力（従来）");
        let broken = parse_edge_attrs(Some("{not json"));
        assert!(broken.when.is_none());
        // v1.8 役割ポート: role は既存キーと同居でき、無い JSON も従来どおり読める。
        let r = parse_edge_attrs(Some(r#"{"role":"system","port":1}"#));
        assert_eq!(r.role.as_deref(), Some("system"));
        assert_eq!(r.port, Some(1));
        assert_eq!(spec(r#"{"builtin":"select"}"#).node_kind(), NodeKind::Select);
        assert!(spec(r#"{"builtin":"select"}"#).absorbs_nodata());
        assert!(spec(r#"{"cmd":"x {out}","nodata":"absorb"}"#).absorbs_nodata());
        assert!(!spec(r#"{"cmd":"x {out}"}"#).absorbs_nodata());
    }

    /// v1.5.7 セレクタ設定: 出力名の stem/拡張子分解（拡張子付きの宣言を寛容に受ける）。
    #[test]
    fn split_output_name_cases() {
        assert_eq!(split_output_name("fibo.exe"), ("fibo", Some("exe")));
        assert_eq!(split_output_name("fibo"), ("fibo", None));
        assert_eq!(split_output_name("report.md"), ("report", Some("md")));
        // 拡張子に見えないものは名前の一部（バージョン表記・末尾ドット・非英数）
        assert_eq!(split_output_name("v1.5"), ("v1", Some("5"))); // 1桁英数は拡張子扱い
        assert_eq!(split_output_name("名前."), ("名前.", None));
        assert_eq!(split_output_name(".gitignore"), (".gitignore", None));
        assert_eq!(split_output_name("a.tar.gz"), ("a.tar", Some("gz")));
        assert_eq!(split_output_name("データ.実験"), ("データ.実験", None));
    }

    /// v1.5.2 WS-C: 録音デバイス名の抽出（dshow・引用あり/なし・無指定は None）。
    #[test]
    fn dshow_audio_device_parse() {
        assert_eq!(
            dshow_audio_device(r#"-f dshow -i audio="マイク (USB MICROPHONE)" -ar 16000 {out}"#),
            Some("マイク (USB MICROPHONE)".to_string())
        );
        assert_eq!(
            dshow_audio_device("-f dshow -i audio=mic0 {out}"),
            Some("mic0".to_string())
        );
        assert_eq!(dshow_audio_device("-t 5 {out}"), None);
    }

    /// シェルカード統合（2026-07-06）: window モードのパースと後方互換
    /// （旧カード＝window 無しは None → run_flow 側で "keep" 扱い）。
    #[test]
    fn shell_window_mode_parse() {
        assert_eq!(spec(r#"{"shell":"cmd"}"#).window, None);
        assert_eq!(
            spec(r#"{"shell":"powershell","window":"hidden"}"#).window.as_deref(),
            Some("hidden")
        );
        assert_eq!(
            spec(r#"{"shell":"cmd","window":"close"}"#).window.as_deref(),
            Some("close")
        );
    }

    /// §4.1 run_external_full が非0終了コードを「捨てずに」持ち上げること。
    #[cfg(windows)]
    #[test]
    fn external_full_captures_nonzero_exit_code() {
        let o = run_external_full("exit 3", &one(b""), "txt", Some(10), &no_cancel(), None, None)
            .unwrap();
        assert_eq!(o.exit_code, Some(3));
        assert!(!o.success());
        // 出力が無いので従来契約（into_bytes）は exit を含むエラーへ落ちる。
        assert!(o.into_bytes().unwrap_err().contains("exit"));
    }

    /// 成功時は exit 0 と出力バイト列をそのまま持つこと。
    #[cfg(windows)]
    #[test]
    fn external_full_captures_success_and_output() {
        let o = run_external_full(
            "copy \"{in}\" \"{out}\"",
            &one(b"hi"),
            "txt",
            None,
            &no_cancel(),
            None,
            None,
        )
        .unwrap();
        assert_eq!(o.exit_code, Some(0));
        assert!(o.success());
        assert_eq!(o.output.as_deref(), Some(&b"hi"[..]));
    }

    #[test]
    fn img_resize_constrains_width() {
        let img = image::DynamicImage::ImageRgba8(image::RgbaImage::new(100, 50));
        let mut png = std::io::Cursor::new(Vec::new());
        img.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let s = spec(r#"{"builtin":"img_resize","width":10}"#);
        let (out, _, _) = run_builtin(&s, "img_resize", png.get_ref()).unwrap();
        let resized = image::load_from_memory(&out).unwrap();
        assert_eq!(resized.width(), 10);
        assert_eq!(resized.height(), 5); // アスペクト維持
    }
}
