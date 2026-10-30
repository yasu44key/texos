import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { SpecInfo } from "./T2iSetupDialog";

// v1.4.1 AIカード（builtin:"ai"）専用の設定ダイアログ。生JSON編集の置き換え。
// 既存 spec の未知フィールド（accept_mime/output_ext 等）は保持してマージする。

interface Props {
  initialLabel: string;
  initialSpec: string;
  onSave: (label: string, specJson: string) => void;
  onCancel: () => void;
  /** v1.12 ④: "construct" ＝コンストラクタカード（builtin:"construct"）を同じ画面で設定する
   * （モデルの入れ替え・調整を AI カードと同じ使い勝手に・2026-10-23 ユーザー）。 */
  variant?: "ai" | "construct" | "voice" | "agent";
}

// v1.7.3 サーバー選択を3択に。"lmstudio" と "openai" はワイヤ形式が同じ（バックエンドの
// is_openai_compat）で、違うのは既定エンドポイントと案内文、到達確認の可否だけ。
// ドキュメント戦略（ツールチップ＋エラーメッセージによる自己文書化）に従い、外部側で
// 何を設定すべきかは全部ここに書く — 別マニュアルを読ませない。
type Server = "ollama" | "lmstudio" | "openai";

const SERVERS: { id: Server; label: string; tip: string }[] = [
  {
    id: "ollama",
    label: "Ollama",
    tip:
      "ローカルの Ollama（既定 http://localhost:11434）。ネイティブ /api/chat で話します。\n" +
      "未導入でも、対話カードのパネルから導入・サーバ起動・モデル取得まで TexoS が案内します。\n" +
      "モデル名は「gemma3:latest」のようなタグ形式。",
  },
  {
    id: "lmstudio",
    label: "LM Studio",
    tip:
      "LM Studio のローカルサーバ（既定 http://localhost:1234/v1）。OpenAI互換APIで話します。\n" +
      "LM Studio でダウンロード済みのモデル（My Models）をそのまま使えます＝Ollama 側へ\n" +
      "取り込み直す必要はありません。LM Studio 側で「Developer」タブを開き、\n" +
      "Status を Running にしておくこと（手順は下の「LM Studio の準備」参照）。",
  },
  {
    id: "openai",
    label: "OpenAI互換（その他）",
    tip:
      "OpenAI互換API（POST /v1/chat/completions）を持つ任意のサーバ。\n" +
      "llama.cpp の server、vLLM、text-generation-webui など。\n" +
      "エンドポイントには /v1 まで含む URL を入れてください。\n" +
      "※ TexoS は認証ヘッダを送りません — APIキーが必要なサーバには接続できません。",
  },
];

const DEFAULT_ENDPOINT: Record<Server, string> = {
  ollama: "http://localhost:11434",
  lmstudio: "http://localhost:1234/v1",
  openai: "http://localhost:1234/v1",
};


// v1.8.9 モデル能力（バックエンド ai_models::ModelInfo の写し）。
// 能力は「機能の有無」であり、精度・品質は表さない（表示でも品質を示唆しない）。
export type Tri = "yes" | "no" | "unknown";
export interface ModelInfo {
  provider: string;
  endpoint: string;
  name: string;
  digest: string;
  parameter_size?: string;
  quantization?: string;
  /** 学習上限（実行時の num_ctx とは別物） */
  trained_context_length?: number;
  capabilities: { completion: Tri; tools: Tri; thinking: Tri; vision: Tri; embedding: Tri };
  raw_capabilities?: string[] | null;
  thinking_mode: { kind: "none" | "boolean" | "levels" | "boolean_or_levels" | "unknown"; levels?: string[] };
  /** 思考に出してよい値（off/on/low/medium/high の部分集合）。判定はバックエンドの1か所 */
  think_choices: string[];
  fetched_at: string;
}

const ALL_THINK = ["off", "on", "low", "medium", "high"];
const THINK_LABEL: Record<string, string> = {
  off: "使わない",
  on: "使う",
  low: "使う（浅く）",
  medium: "使う（中）",
  high: "使う（深く）",
};

const allUnknown = (m: ModelInfo) => Object.values(m.capabilities).every((t) => t === "unknown");

/// 候補▼の各行に添える能力の印（有るものだけ）。不明は ⚠。
function capBadge(m: ModelInfo): string {
  if (allUnknown(m)) return "  ⚠能力不明";
  const c = m.capabilities;
  let b = "";
  if (c.thinking === "yes") b += "🧠";
  if (c.vision === "yes") b += "👁";
  if (c.tools === "yes") b += "🔧";
  return b ? `  ${b}` : "";
}

/// Ollama は "gemma3" を "gemma3:latest" として扱う。
function findInfo(catalog: ModelInfo[], name: string): ModelInfo | undefined {
  const n = name.trim();
  if (!n) return undefined;
  return catalog.find((m) => m.name === n) ?? catalog.find((m) => m.name === `${n}:latest`);
}

/// v1.9.2 追加パラメータで上書きさせないキー。バックエンド flow.rs の AI_RESERVED_EXTRA_KEYS と揃える。
const RESERVED_EXTRA_KEYS = ["model", "messages", "stream", "temperature", "think", "reasoning_effort", "options"];

/// v1.9.2 追加パラメータ欄の検査。空欄は null（＝送らない）、不正なら error。
function parseExtra(text: string): { value: Record<string, unknown> | null; error: string | null } {
  if (!text.trim()) return { value: null, error: null };
  let v: unknown;
  try {
    v = JSON.parse(text);
  } catch (e) {
    return { value: null, error: `JSON として読めません: ${(e as Error).message}` };
  }
  if (typeof v !== "object" || v === null || Array.isArray(v))
    return { value: null, error: '{ "キー": 値 } の形（オブジェクト）で書いてください' };
  const bad = Object.keys(v).filter((k) => RESERVED_EXTRA_KEYS.includes(k));
  if (bad.length)
    return {
      value: null,
      error: `使えないキー: ${bad.join(", ")}（モデル・温度・思考などは専用の欄で指定してください）`,
    };
  return { value: v as Record<string, unknown>, error: null };
}

/// v1.9.2 システムプロンプトに API パラメータを書いてしまう誤解の検出（本文として送られるだけで効かない）。
const PARAM_LIKE =
  /\b(reasoning_[a-z_]+|max_tokens|max_completion_tokens|top_p|top_k|temperature|enable_thinking|chat_template_kwargs|num_ctx|num_predict)\b"?\s*[:=]/;

function parseServer(v: unknown): Server {
  return v === "openai" || v === "lmstudio" ? v : "ollama";
}

export function AiSettingsDialog({ initialLabel, initialSpec, onSave, onCancel, variant = "ai" }: Props) {
  // V1.13 "agent" ＝エージェントカード（builtin:"agent"）。組む一手がコンストラクタを呼ぶので、
  // 並べる AI カードのモデル・差し戻し回数はコンストラクタと同じ欄（agent.rs → construct.rs が読む）。
  const isAgent = variant === "agent";
  const isConstruct = variant === "construct" || isAgent;
  // v1.12: "voice" ＝音声カードの応答用 AI（設定キーは ai_ 接頭辞・voice.rs ai_conf が読む）。
  const isVoice = variant === "voice";
  const K = (n: string) => (isVoice ? `ai_${n}` : n);
  const base = useMemo<Record<string, unknown>>(() => {
    try {
      return JSON.parse(initialSpec || "{}");
    } catch {
      return {};
    }
  }, [initialSpec]);
  const s = (k: string, d = "") => (typeof base[k] === "string" ? (base[k] as string) : d);

  const [title, setTitle] = useState(initialLabel);
  const [provider, setProvider] = useState<Server>(parseServer(base[K("provider")]));
  const [endpoint, setEndpoint] = useState(s(K("endpoint")));

  // サーバーを替えたら、宛先も新しいサーバーの既定に切り替えて欄に見せる（2026-10-24 ユーザー:
  // Strata の 8080 を入れたまま Ollama に替えると宛先が残った）。この画面で一度使ったサーバーへ戻したときは、
  // そのサーバーで使っていた宛先（遠隔・別ポート）を戻す＝手で入れた宛先を失わない。
  const endpointMemo = useRef<Partial<Record<Server, string>>>({});
  const changeServer = (next: Server) => {
    if (next === provider) return;
    endpointMemo.current[provider] = endpoint;
    setProvider(next);
    setEndpoint(endpointMemo.current[next] || DEFAULT_ENDPOINT[next]);
  };
  const [model, setModel] = useState(s(K("model"), "gemma3:latest"));
  const [system, setSystem] = useState(s("system"));
  const [temperature, setTemperature] = useState(
    typeof base[K("temperature")] === "number" ? String(base[K("temperature")]) : ""
  );
  // v1.4.12 制限時間（秒）。v1.7.4: 生成系共通契約に統一 — 空欄=0=無制限（変換も対話も）。
  const [timeoutSecs, setTimeoutSecs] = useState(
    typeof base.timeout_secs === "number" ? String(base.timeout_secs) : ""
  );
  const [mode, setMode] = useState<"chat" | "transform">(
    base.mode === "chat" ? "chat" : "transform"
  );
  const [logName, setLogName] = useState(s("log_name")); // v1.4.5 ログ保存名（chat）
  // v1.12 ④ コンストラクタ固有（construct.rs が読む）。
  const [cardModel, setCardModel] = useState(s("card_model"));
  const [apply, setApply] = useState<"place" | "draft">(base.apply === "draft" ? "draft" : "place");
  const [maxRetries, setMaxRetries] = useState(
    typeof base.max_retries === "number" ? String(base.max_retries) : "3"
  );
  const [prefix, setPrefix] = useState(s("prefix", isConstruct ? "構築レポート" : ""));
  const [maxSteps, setMaxSteps] = useState(typeof base.max_steps === "number" ? String(base.max_steps) : "8");
  // v1.8.6 GPU の使い方（Ollama のみ）。既定 "auto" = キーを書かない＝サーバ任せ
  // （＝VRAM に載る層は GPU。旧来の「VRAM が少ないから CPU」という既定は廃止）。
  const [gpu, setGpu] = useState<"auto" | "max" | "cpu">(
    base.gpu === "max" || base.gpu === "cpu" ? base.gpu : "auto"
  );
  // v1.8.6 Thinking（思考）モード。off / on / low / medium / high。
  const [think, setThink] = useState<string>(
    typeof base[K("think")] === "string" && base[K("think")] !== "" ? (base[K("think")] as string) : "off"
  );
  // v1.8.9 モデルに合わせて思考の値を直したときの説明（黙って変えない）。
  const [thinkAdjusted, setThinkAdjusted] = useState<string | null>(null);
  // v1.9.2 汎用 OpenAI互換へ思考の指定（reasoning_effort）を送るか。既定オフ。
  const [sendReasoning, setSendReasoning] = useState(base.send_reasoning === true);
  // v1.9.2 追加パラメータ（本体トップレベルへマージする JSON）。全サーバ共通。
  const [extraText, setExtraText] = useState(
    base[K("extra_params")] && typeof base[K("extra_params")] === "object" ? JSON.stringify(base[K("extra_params")]) : ""
  );
  const extra = useMemo(() => parseExtra(extraText), [extraText]);
  // v1.8.9 実行時のコンテキスト長（Ollama の num_ctx）。空欄＝サーバ既定。
  const [numCtx, setNumCtx] = useState(typeof base.num_ctx === "number" ? String(base.num_ctx) : "");

  // v1.4.28 WS3: PCスペック検出。モデルサイズ選択の根拠として表示する。
  const [spec, setSpec] = useState<SpecInfo | null>(null);
  useEffect(() => {
    invoke<SpecInfo>("detect_spec")
      .then(setSpec)
      .catch(() => setSpec(null));
  }, []);

  // v1.4.2 モデル候補（/api/tags 等から取得）。手入力も併用可（datalist）。
  // v1.8.9 能力つきの一覧（ai_model_catalog）。Ollama は digest が変わったモデルだけ
  // /api/show を呼び直す（キャッシュはマシン単位）。
  const [catalog, setCatalog] = useState<ModelInfo[]>([]);
  const models = useMemo(() => catalog.map((m) => m.name), [catalog]);
  const [modelsState, setModelsState] = useState<"idle" | "loading" | "error">("idle");
  const fetchModels = useCallback(() => {
    setModelsState("loading");
    invoke<ModelInfo[]>("ai_model_catalog", { provider, endpoint: endpoint.trim() || null })
      .then((list) => {
        setCatalog(list);
        setModelsState("idle");
      })
      .catch(() => {
        setCatalog([]);
        setModelsState("error");
      });
  }, [provider, endpoint]);
  const info = useMemo(() => findInfo(catalog, model), [catalog, model]);
  const levelsOnly = info?.thinking_mode.kind === "levels";
  const thinkChoices = info?.think_choices ?? ALL_THINK;

  // v1.8.9 選んだモデルで選べない思考の値は、実行時と同じ規則で読み替えて知らせる
  // （思考非対応→使わない／gpt-oss の「使う」→中／深さ非対応→使う）。
  useEffect(() => {
    if (!info || thinkChoices.includes(think)) return;
    const next =
      think === "on" && levelsOnly ? "medium" : think !== "off" && thinkChoices.includes("on") ? "on" : "off";
    setThink(next);
    setThinkAdjusted(
      info.capabilities.thinking === "no"
        ? "このモデルは思考に非対応のため「使わない」にしました"
        : `このモデルでは「${THINK_LABEL[think] ?? think}」を選べないため「${
            levelsOnly && next === "off" ? "指定しない" : THINK_LABEL[next]
          }」にしました`
    );
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [info]);

  // 候補▼で選べない理由（無ければ null）。no は選択不可、unknown は選べる（⚠付き）。
  const unusable = (m: ModelInfo): string | null => {
    if (m.capabilities.completion === "no") return "埋め込み専用・対話不可";
    if (think !== "off" && m.capabilities.thinking === "no") return "思考非対応 — 思考を「使わない」にすると選べます";
    return null;
  };
  // 初回＋プロバイダ切替で自動取得（エンドポイント変更は「↻取得」で明示）。
  useEffect(() => {
    fetchModels();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [provider]);

  const endpointPlaceholder = `空欄 = ${DEFAULT_ENDPOINT[provider]}${
    provider === "openai" ? "（/v1 まで含むURLを指定）" : ""
  }`;

  // 手入力のモデル名が接続先の一覧に無い＝サーバー切替後の取り残しが最頻（Ollama の
  // 「gemma3:latest」は LM Studio には無い）。送信前にここで気づけるようにする。
  const modelUnknown =
    modelsState === "idle" && models.length > 0 && !!model.trim() && !models.includes(model.trim());

  const save = () => {
    if (extra.error) return;
    if (isVoice) {
      // 音声カードは op カード: 他の設定（言い回し・しきい値など）はそのまま、ai_* だけ差し替える。
      const v: Record<string, unknown> = { ...base, ai_provider: provider };
      v.ai_model = model.trim() || (provider === "ollama" ? "gemma3:latest" : models[0] ?? "");
      if (endpoint.trim()) v.ai_endpoint = endpoint.trim();
      else delete v.ai_endpoint;
      if (think !== "off") v.ai_think = think;
      else delete v.ai_think;
      const t = parseFloat(temperature);
      if (temperature.trim() !== "" && !Number.isNaN(t)) v.ai_temperature = t;
      else delete v.ai_temperature;
      if (extra.value && Object.keys(extra.value).length) v.ai_extra_params = extra.value;
      else delete v.ai_extra_params;
      onSave(title.trim() || "音声", JSON.stringify(v));
      return;
    }
    const spec: Record<string, unknown> = isConstruct
      ? { ...base, builtin: isAgent ? "agent" : "construct", provider }
      : { ...base, builtin: "ai", provider, mode };
    if (isConstruct) {
      delete spec.mode;
      if (cardModel.trim()) spec.card_model = cardModel.trim();
      else delete spec.card_model;
      const r = parseInt(maxRetries, 10);
      spec.max_retries = Number.isFinite(r) ? Math.min(6, Math.max(1, r)) : 3;
    }
    if (isConstruct && !isAgent) {
      if (apply === "draft") spec.apply = "draft";
      else delete spec.apply;
      if (prefix.trim()) spec.prefix = prefix.trim();
      else delete spec.prefix;
    }
    if (isAgent) {
      const n = parseInt(maxSteps, 10);
      spec.max_steps = Number.isFinite(n) ? Math.min(16, Math.max(1, n)) : 8;
    }
    // 空欄時の既定: Ollama は導入案内が gemma3 を入れるのでそれに合わせる。
    // 他サーバーは既定名が存在しないので、取得済み一覧の先頭を使う。
    spec.model = model.trim() || (provider === "ollama" ? "gemma3:latest" : models[0] ?? "");
    if (endpoint.trim()) spec.endpoint = endpoint.trim();
    else delete spec.endpoint;
    if (system.trim()) spec.system = system;
    else delete spec.system;
    const t = parseFloat(temperature);
    if (temperature.trim() !== "" && !Number.isNaN(t)) spec.temperature = t;
    else delete spec.temperature;
    const to = parseInt(timeoutSecs, 10);
    if (timeoutSecs.trim() !== "" && !Number.isNaN(to) && to >= 0) spec.timeout_secs = to;
    else delete spec.timeout_secs;
    if (!isConstruct && mode === "chat" && logName.trim()) spec.log_name = logName.trim();
    else delete spec.log_name;
    // v1.8.6 既定値はキーごと落とす（spec を小さく保つ＝JSON編集でも読める）。
    if (gpu !== "auto" && provider === "ollama") spec.gpu = gpu;
    else delete spec.gpu;
    if (think !== "off") spec.think = think;
    else delete spec.think;
    // v1.9.2 送る設定は汎用 OpenAI互換のときだけ意味を持つ（他サーバでは落とす）。
    if (provider === "openai" && sendReasoning) spec.send_reasoning = true;
    else delete spec.send_reasoning;
    if (extra.value && Object.keys(extra.value).length) spec.extra_params = extra.value;
    else delete spec.extra_params;
    // v1.8.9 num_ctx は Ollama のみ（LM Studio はロード時に決まる）。
    const nc = parseInt(numCtx, 10);
    if (provider === "ollama" && numCtx.trim() !== "" && !Number.isNaN(nc) && nc > 0) spec.num_ctx = nc;
    else delete spec.num_ctx;
    // 接続時型チェック・出力の既定（未設定なら補う）。
    if (!spec.accept_mime) spec.accept_mime = !isConstruct && mode === "chat" ? "*/*" : "text/*";
    if (!spec.output_ext) spec.output_ext = "md";
    onSave(
      title.trim() || (isAgent ? "エージェント" : isConstruct ? "コンストラクタ" : `AI: ${spec.model}`),
      JSON.stringify(spec)
    );
  };

  return (
    <div
      style={{
        position: "fixed",
        inset: 0,
        background: "#00000066",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        zIndex: 2000,
      }}
      onMouseDown={onCancel}
    >
      <div
        onMouseDown={(e) => e.stopPropagation()}
        style={{
          // 2026-07-06: テーマ追従（--panel-* 体系）。「設定中のアテンション＝暗色固定」の
          // 2026-06-26 決定は改訂 — シェル/外部プログラム設定と並んだときの不整合を解消。
          background: "var(--panel-bg, #1c2130)",
          border: "1px solid var(--panel-border, #38445e)",
          borderRadius: 10,
          padding: 20,
          width: 520,
          maxHeight: "85vh",
          overflowY: "auto",
          boxShadow: "0 8px 32px #000000aa",
          fontFamily: "sans-serif",
          color: "var(--panel-text, #d6dded)",
          fontSize: 13,
        }}
      >
        <div style={{ fontSize: 15, marginBottom: 14, textAlign: "center" }}>
          {isAgent
            ? "🤖 エージェントの設定"
            : isConstruct
              ? "🏗 コンストラクタの設定"
              : isVoice
                ? "🎙 音声カードの応答 AI"
                : "🤖 AIカードの設定"}
        </div>
        {isAgent && (
          <div style={{ fontSize: 12, color: "var(--panel-muted, #8a93a8)", marginBottom: 12, lineHeight: 1.5 }}>
            カードの下の欄で頼むと、この AI が一手ずつ道具を選びます（答える・索引で調べる・ファイルを読む・
            フローを組む・実行する）。組む一手はコンストラクタと同じ検査と並べ方です。外部プログラム・シェル・
            オペレーションを含むフローは実行せず、▶ を人が押すのを待ちます。一手は形を決めた JSON で受け取ります。
          </div>
        )}
        {isVoice && (
          <div style={{ fontSize: 12, color: "var(--panel-muted, #8a93a8)", marginBottom: 12, lineHeight: 1.5 }}>
            話しかけた内容を聞いて、返事をし、どのカードへ流すかを決める AI です。速さが大事なので軽いモデル向き
            （gemma3 は約 0.3 秒）。Strata など OpenAI 互換も選べますが、裏で考えている最中は順番待ちになります。
            返事は形を決めた JSON で受け取ります（Ollama は format、OpenAI 互換は response_format）。
          </div>
        )}
        {isConstruct && !isAgent && (
          <div style={{ fontSize: 12, color: "var(--panel-muted, #8a93a8)", marginBottom: 12, lineHeight: 1.5 }}>
            つないだ依頼（付箋・音声カードからの発話）を読み、AI がカードの設計図を書きます。配線の決まり
            （scripts/texos_flow.py の検査）で確かめ、違反は理由ごと AI に返して直させてから、いまのプロジェクトの
            いちばん下に枠で囲んで並べます。並べたカードは実行しません（▶ は人が押す）。
          </div>
        )}

        {row(
          "タイトル",
          <input style={inp()} value={title} onChange={(e) => setTitle(e.target.value)} />
        )}

        {!isConstruct && !isVoice && row(
          "モード",
          <div style={{ display: "flex", gap: 18 }}>
            {(
              [
                ["chat", "対話（チャットパネル）"],
                ["transform", "変換（一発・パイプライン）"],
              ] as ["chat" | "transform", string][]
            ).map(([m, label]) => (
              <label key={m} style={{ display: "flex", alignItems: "center", gap: 5, cursor: "pointer" }}>
                <input type="radio" checked={mode === m} onChange={() => setMode(m)} />
                {label}
              </label>
            ))}
          </div>
        )}

        {row(
          "サーバー",
          <div>
            <div style={{ display: "flex", gap: 14, flexWrap: "wrap" }}>
              {SERVERS.map((sv) => (
                <label
                  key={sv.id}
                  title={sv.tip}
                  style={{ display: "flex", alignItems: "center", gap: 5, cursor: "help" }}
                >
                  <input
                    type="radio"
                    checked={provider === sv.id}
                    onChange={() => changeServer(sv.id)}
                    style={{ cursor: "pointer" }}
                  />
                  {sv.label}
                </label>
              ))}
            </div>
            <div style={{ fontSize: 11, color: "var(--panel-muted, #8a93a8)", marginTop: 3 }}>
              各項目にマウスを重ねると説明が出ます（同じPCで Ollama と LM Studio を併用可）
            </div>
          </div>
        )}

        {row(
          "エンドポイント",
          <input
            style={inp()}
            value={endpoint}
            onChange={(e) => setEndpoint(e.target.value)}
            placeholder={endpointPlaceholder}
            title={SERVERS.find((sv) => sv.id === provider)!.tip}
          />
        )}

        {provider !== "ollama" && setupHint(provider)}

        {row(
          "モデル",
          <div>
            <div style={{ display: "flex", gap: 6 }}>
              <input
                style={{ ...inp(), flex: 1 }}
                value={model}
                onChange={(e) => setModel(e.target.value)}
                placeholder={provider === "ollama" ? "gemma3:latest" : "例: gemma-4-e4b-it"}
                title={
                  provider === "ollama"
                    ? "Ollama のモデルタグ（例: gemma3:latest / qwen2.5:3b）"
                    : "サーバーが公開しているモデルID（LM Studio なら My Models の識別子）。\n" +
                      "「↻ 取得」で接続先から実際の一覧を引けます。"
                }
              />
              {models.length > 0 && (
                <select
                  value=""
                  onChange={(e) => {
                    if (e.target.value) {
                      setModel(e.target.value);
                      setThinkAdjusted(null);
                    }
                  }}
                  style={{ ...inp(), width: 130, flex: "none" }}
                  title={
                    "取得したモデルから選ぶ。印は機能の有無（品質を示すものではありません）:\n" +
                    "🧠 思考　👁 画像入力　🔧 ツール　⚠ 能力不明（サーバが返さない）\n" +
                    "選べない行は、いまの設定では使えない機能のモデルです（理由を併記）。"
                  }
                >
                  <option value="">候補 ▼</option>
                  {catalog.map((m) => {
                    const why = unusable(m);
                    return (
                      <option key={m.name} value={m.name} disabled={!!why}>
                        {m.name}
                        {capBadge(m)}
                        {why ? `（${why}）` : ""}
                      </option>
                    );
                  })}
                </select>
              )}
              <button onClick={fetchModels} style={smallBtn()} title="接続先からモデル一覧を取得">
                ↻ 取得
              </button>
            </div>
            <div
              style={{
                fontSize: 11,
                marginTop: 3,
                color: modelsState === "error" || modelUnknown ? "#e0a35c" : "var(--panel-muted, #8a93a8)",
              }}
            >
              {modelsState === "loading"
                ? "取得中…"
                : modelsState === "error"
                ? // v1.7.3 サーバー別に「次に何をするか」まで言う（自己文書化の3本柱＝エラーメッセージ）
                  provider === "lmstudio"
                  ? "一覧を取得できません — LM Studio の「Developer」タブで Status を Running にしてください"
                  : provider === "openai"
                  ? "一覧を取得できません — サーバの起動と、URLに /v1 まで含まれているか確認してください"
                  : "一覧を取得できません（Ollama が起動しているか確認）"
                : modelUnknown
                ? `「${model.trim()}」はこのサーバーの一覧にありません — 「候補▼」から選び直してください`
                : models.length
                ? `${models.length}件取得 — 「候補▼」から選択・手入力も可`
                : "「↻取得」で候補を表示（手入力も可）"}
            </div>
          </div>
        )}

        {info && row("能力", capabilityView(info, mode))}

        {spec &&
          row(
            "この PC",
            <div style={{ fontSize: 11, color: "var(--panel-muted, #8a93a8)", lineHeight: 1.5 }}>
              {spec.gpu
                ? `GPU: ${spec.gpu.name}（VRAM ${(spec.gpu.vram_mb / 1024).toFixed(1)}GB）`
                : "対応 GPU 未検出（CPU 動作）"}
              {spec.ram_mb > 0 && ` ／ RAM: ${(spec.ram_mb / 1024).toFixed(1)}GB`}
              {provider === "ollama" && spec.ollama_hint && (
                <div style={{ color: "#8fb0ca", marginTop: 2 }}>💡 {spec.ollama_hint}</div>
              )}
            </div>
          )}

        {provider === "ollama" && !isVoice &&
          row(
            "GPU",
            <div>
              <select
                style={{ ...inp(), width: 220 }}
                value={gpu}
                onChange={(e) => setGpu(e.target.value as "auto" | "max" | "cpu")}
                title={
                  "自動: Ollama が VRAM に載る分だけ GPU に置き、溢れた層を CPU へ逃がします（推奨）。\n" +
                  "最大限: 全ての層を GPU に強制します（num_gpu=999）。載り切らないとロードに失敗します。\n" +
                  "CPU のみ: GPU を使いません（他の処理に VRAM を空けたいとき）。"
                }
              >
                <option value="auto">自動（載る分だけ GPU・推奨）</option>
                <option value="max">最大限 GPU（全層を強制）</option>
                <option value="cpu">CPU のみ</option>
              </select>
              <div style={{ fontSize: 11, color: "var(--panel-muted, #8a93a8)", marginTop: 3 }}>
                {gpu === "auto"
                  ? "VRAM 容量による CPU 転落はしません — 載る分は必ず GPU を使います"
                  : gpu === "max"
                  ? "VRAM に載り切らないモデルではロードに失敗します（そのときは自動へ戻してください）"
                  : "GPU を使わないため応答は遅くなります"}
              </div>
            </div>
          )}

        {row(
          "思考",
          <div>
            <select
              style={{ ...inp(), width: 220 }}
              value={think}
              disabled={thinkChoices.length <= 1 && think === "off"}
              onChange={(e) => {
                setThink(e.target.value);
                setThinkAdjusted(null);
              }}
              title={
                "思考（Thinking）対応モデルに、答える前の思考を出させます。\n" +
                "思考は本文と分けて折りたたみ表示するだけで、会話履歴にも MDログにも残りません。\n" +
                "Ollama は think パラメータで分離。LM Studio / OpenAI互換は、サーバが返す\n" +
                "reasoning と、本文に混ざる <think>…</think> の両方を拾って分離します。"
              }
            >
              {thinkChoices.map((c) => (
                <option key={c} value={c}>
                  {c === "off" && levelsOnly ? "指定しない（サーバ既定の深さ）" : THINK_LABEL[c] ?? c}
                </option>
              ))}
            </select>
            {provider === "openai" && !isVoice && (
              <label
                style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 12, marginTop: 4 }}
                title={
                  "オンにすると、思考欄の値を OpenAI の reasoning_effort として送ります。\n" +
                  "使わない→none／浅く→low／中→medium／深く→high（使う→送らずサーバ既定）。\n" +
                  "未知のパラメータや値を 400 で拒否するサーバがあるため、既定はオフです。"
                }
              >
                <input type="checkbox" checked={sendReasoning} onChange={(e) => setSendReasoning(e.target.checked)} />
                思考の指定を送る（reasoning_effort）
              </label>
            )}
            {thinkAdjusted && <div style={{ fontSize: 11, color: "#e0a35c", marginTop: 3 }}>{thinkAdjusted}</div>}
            <div style={{ fontSize: 11, color: "var(--panel-muted, #8a93a8)", marginTop: 3 }}>
              {thinkHint(provider, think, info, sendReasoning)}
            </div>
          </div>
        )}

        {provider === "ollama" && !isVoice &&
          row(
            "コンテキスト長",
            <div>
              <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                <input
                  style={{ ...inp(), width: 100 }}
                  value={numCtx}
                  onChange={(e) => setNumCtx(e.target.value)}
                  placeholder="サーバ既定"
                  inputMode="numeric"
                  title={
                    "実行時に確保するコンテキスト長（Ollama の num_ctx・トークン数）。\n" +
                    "空欄なら Ollama の既定（モデルの学習上限よりずっと小さい）。\n" +
                    "長い資料をつなぐときに増やします。大きいほど VRAM を使います。"
                  }
                />
                <span style={{ fontSize: 11, color: "var(--panel-muted, #8a93a8)" }}>
                  トークン（空欄=サーバ既定）
                </span>
              </div>
              {info?.trained_context_length != null && (
                <div
                  style={{
                    fontSize: 11,
                    marginTop: 3,
                    color:
                      parseInt(numCtx, 10) > info.trained_context_length ? "#e0a35c" : "var(--panel-muted, #8a93a8)",
                  }}
                >
                  このモデルの学習上限: {info.trained_context_length.toLocaleString()} トークン
                  {parseInt(numCtx, 10) > info.trained_context_length && " — 学習上限を超えています"}
                </div>
              )}
            </div>
          )}

        {!isVoice && row(
          isAgent ? "追加の\n指示" : isConstruct ? "追加の\n作法" : "システム\nプロンプト",
          <div>
            <textarea
              style={{ ...inp(), resize: "vertical", minHeight: 64, fontFamily: "inherit" }}
              value={system}
              rows={4}
              onChange={(e) => setSystem(e.target.value)}
              placeholder={
                isAgent
                  ? "（空欄可）口調・得意分野・いつも使ってほしい索引など。道具の使い方は組み込み済み"
                  : isConstruct
                  ? "（空欄可）設計の作法を足す。構築の要点は組み込み済み。役割の付箋をつないでも同じ"
                  : "（空欄可）AIの役割・口調・出力形式の指示"
              }
            />
            {PARAM_LIKE.test(system) && (
              <div style={{ fontSize: 11, color: "#e0a35c", marginTop: 3 }}>
                ここに書いた内容は本文として AI に送られるだけで、API のパラメータとしては効きません —
                パラメータは下の「追加パラメータ」欄へ
              </div>
            )}
          </div>
        )}

        {row(
          "温度",
          <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
            <input
              style={{ ...inp(), width: 100 }}
              value={temperature}
              onChange={(e) => setTemperature(e.target.value)}
              placeholder="モデル既定"
              inputMode="decimal"
            />
            <span style={{ fontSize: 11, color: "var(--panel-muted, #8a93a8)" }}>
              0=厳密 / 0.7=標準 / 高=多様（空欄=モデル既定）
            </span>
          </div>
        )}

        {!isVoice && row(
          "制限時間",
          <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
            <input
              style={{ ...inp(), width: 100 }}
              value={timeoutSecs}
              onChange={(e) => setTimeoutSecs(e.target.value)}
              placeholder="空欄=無制限"
              inputMode="numeric"
            />
            <span style={{ fontSize: 11, color: "var(--panel-muted, #8a93a8)" }}>
              秒で打ち切り（空欄・0=無制限＝中断ボタンで停止）。暴走が怖いときだけ秒数を入れる
            </span>
          </div>
        )}

        {row(
          "追加\nパラメータ",
          <div>
            <textarea
              style={{ ...inp(), resize: "vertical", minHeight: 32, fontFamily: "monospace", fontSize: 12 }}
              value={extraText}
              rows={2}
              onChange={(e) => setExtraText(e.target.value)}
              placeholder='（空欄可）例: {"reasoning_budget_tokens": 8000}'
              title={
                "リクエスト本体のトップレベルへそのまま加える JSON（サーバ独自の拡張を使うとき）。\n" +
                "例: Strata の思考上限 {\"reasoning_budget_tokens\": 8000}（0=上限なし）。\n" +
                "model・messages・stream・temperature・think・reasoning_effort・options は\n" +
                "専用の欄が管理するので書けません。サーバが知らないキーは 400 になることがあります。"
              }
            />
            <div
              style={{ fontSize: 11, marginTop: 3, color: extra.error ? "#e0a35c" : "var(--panel-muted, #8a93a8)" }}
            >
              {extra.error ?? "サーバ独自のパラメータをそのまま送ります（全サーバ共通・実行ログに残ります）"}
            </div>
          </div>
        )}

        {isConstruct && (
          <>
            {row(
              "並べる AI\nカードの\nモデル",
              <div>
                <input
                  style={inp()}
                  value={cardModel}
                  onChange={(e) => setCardModel(e.target.value)}
                  list="construct-card-models"
                  placeholder="空欄=上と同じモデル（接続先・アドレスも同じ）"
                />
                <datalist id="construct-card-models">
                  {models.map((m) => (
                    <option key={m} value={m} />
                  ))}
                </datalist>
              </div>
            )}
            {!isAgent && row(
              "実行したら",
              <select style={inp()} value={apply} onChange={(e) => setApply(e.target.value as "place" | "draft")}>
                <option value="place">カードを並べる（UNDO で一度に戻せる）</option>
                <option value="draft">案だけ出す（構築レポートに設計図と検査結果）</option>
              </select>
            )}
            {isAgent && row(
              "手数の\n上限",
              <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                <input
                  style={{ ...inp(), width: 100 }}
                  value={maxSteps}
                  onChange={(e) => setMaxSteps(e.target.value)}
                  inputMode="numeric"
                />
                <span style={{ fontSize: 11, color: "var(--panel-muted, #8a93a8)" }}>
                  1回の頼みで道具を使える回数（1〜16）。超えたらそこで止めて報告します
                </span>
              </div>
            )}
            {row(
              "差し戻す\n回数の上限",
              <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                <input
                  style={{ ...inp(), width: 100 }}
                  value={maxRetries}
                  onChange={(e) => setMaxRetries(e.target.value)}
                  inputMode="numeric"
                />
                <span style={{ fontSize: 11, color: "var(--panel-muted, #8a93a8)" }}>
                  検査で違反が見つかったら AI に直させる回数（1〜6）
                </span>
              </div>
            )}
            {!isAgent &&
              row("レポートの\n名前", <input style={inp()} value={prefix} onChange={(e) => setPrefix(e.target.value)} />)}
          </>
        )}

        {!isConstruct && !isVoice && mode === "chat" &&
          row(
            "ログ保存名",
            <input
              style={inp()}
              value={logName}
              onChange={(e) => setLogName(e.target.value)}
              placeholder="空欄=「<タイトル>のログ.md」（カード毎に変えると衝突しない）"
            />
          )}

        <div style={{ display: "flex", justifyContent: "flex-end", gap: 8, marginTop: 16 }}>
          <button onClick={onCancel} style={btn("#2a3346", "#aeb8cc")}>
            キャンセル
          </button>
          <button
            onClick={save}
            disabled={!!extra.error}
            title={extra.error ? "追加パラメータを直すと保存できます" : undefined}
            style={{ ...btn("#3a6fb0", "#ffffff"), opacity: extra.error ? 0.5 : 1 }}
          >
            保存
          </button>
        </div>
      </div>
    </div>
  );
}

/// v1.8.9 選んだモデルの能力（機能の有無）と学習上限。品質を示唆する表現は使わない。
function capabilityView(m: ModelInfo, mode: "chat" | "transform") {
  const mark = (t: Tri) => (t === "yes" ? "○" : t === "no" ? "×" : "？");
  const c = m.capabilities;
  const items: [string, Tri][] = [
    ["対話", c.completion],
    ["思考", c.thinking],
    ["画像入力", c.vision],
    ["ツール", c.tools],
    ["埋め込み", c.embedding],
  ];
  const facts = [
    m.trained_context_length != null && `学習上限 ${m.trained_context_length.toLocaleString()} トークン`,
    m.parameter_size,
    m.quantization,
  ].filter(Boolean);
  return (
    <div
      style={{ fontSize: 11, lineHeight: 1.6, color: "var(--panel-muted, #8a93a8)", paddingTop: 5 }}
      title={
        "モデルが持つ機能の有無です（サーバの申告による）。精度や品質を示すものではありません。\n" +
        "○ あり　× なし　？ 不明（サーバが能力を返さない）" +
        (m.digest ? `\n中身の識別子: ${m.digest.slice(0, 24)}…` : "")
      }
    >
      <div style={{ color: "var(--panel-text, #d6dded)" }}>{items.map(([k, t]) => `${k}${mark(t)}`).join("　")}</div>
      {facts.length > 0 && <div>{facts.join(" ／ ")}</div>}
      {allUnknown(m) && (
        <div style={{ color: "#e0a35c" }}>
          ⚠ このサーバは能力を返しません。選べますが、非対応の指定はエラーになることがあります（自動では送り直しません）
        </div>
      )}
      {c.completion === "no" && (
        <div style={{ color: "#e0a35c" }}>埋め込み専用モデルです — 対話・変換には使えません</div>
      )}
      {mode === "chat" && c.vision === "no" && <div>画像カードをつないでも、このモデルには見えません</div>}
    </div>
  );
}

/// v1.8.9 思考欄の下の説明（実際に何を送るか）。判定の本体はバックエンドの plan_think。
function thinkHint(provider: Server, think: string, info: ModelInfo | undefined, sendReasoning: boolean): string {
  // v1.9.2 汎用 OpenAI互換は「思考の指定を送る」のときだけ reasoning_effort を送る。
  if (provider === "openai" && sendReasoning) {
    if (think === "off") return 'reasoning_effort="none" を送ります（思考を止める。非対応のサーバは 400 になります）';
    if (think === "on") return "深さはサーバ既定に任せます（reasoning_effort は送りません）";
    return `reasoning_effort="${think}" を送ります`;
  }
  if (think === "off") {
    if (info?.thinking_mode.kind === "levels")
      return "このモデル（gpt-oss 系）は思考を止められません。深さを指定しなければサーバ既定で思考します";
    return "思考型モデルが本文に混ぜてくる <think>…</think> は、この設定に関わらず常に取り除きます";
  }
  if (provider === "openai")
    return "このままでは送りません — 送るには上の「思考の指定を送る」をオンに。返ってきた思考は分離して表示します";
  if (provider === "lmstudio") {
    if (think === "on") return "思考の使う／使わないは LM Studio 側の設定に従います（API からは送りません）";
    return `深さは reasoning_effort="${think}" として送ります`;
  }
  if (!info || info.capabilities.thinking === "unknown")
    return "このモデルの能力は不明です。思考に非対応ならエラーになります（自動では送り直しません）";
  return think === "on" ? "think=true として送ります" : `深さは think="${think}" として送ります`;
}

/// v1.7.3 OpenAI互換サーバー側で何を設定すべきかの案内。ダイアログ内に常設する
/// （ツールチップは「重ねないと読めない」ので、初回に必ず要る手順はこちらへ出す）。
function setupHint(provider: Exclude<Server, "ollama">) {
  const lines =
    provider === "lmstudio"
      ? [
          "① LM Studio 左のサイドバーから「Developer」タブを開く",
          "② 左上のトグルを Status: Running に（既定ポート 1234・URLは /v1 付き）",
          "③ Settings の「Just-in-Time model loading」をON → ここで選んだモデルが自動で読み込まれます",
          "④ 初回のモデル読み込みは数十秒かかることがあります。下の「制限時間」は空欄（＝無制限）のままで可",
        ]
      : [
          "① サーバ側で OpenAI互換API（POST /v1/chat/completions）を有効にする",
          "② 上のエンドポイントに /v1 まで含むベースURLを入れる（例: http://192.168.1.10:8080/v1）",
          "③ 「↻ 取得」でモデル一覧が引ければ接続成功です",
          "※ 認証ヘッダは送りません — APIキーが必要なサーバには接続できません",
        ];
  return (
    <div style={{ display: "flex", gap: 10, marginBottom: 10 }}>
      <div style={{ width: 92, flex: "none" }} />
      <div
        style={{
          flex: 1,
          minWidth: 0,
          fontSize: 11,
          lineHeight: 1.7,
          color: "var(--panel-muted, #8a93a8)",
          background: "var(--panel-input-bg, #0e121c)",
          border: "1px solid var(--panel-border, #38445e)",
          borderRadius: 6,
          padding: "7px 10px",
        }}
      >
        <div style={{ color: "var(--panel-text, #d6dded)", marginBottom: 2 }}>
          💡 {provider === "lmstudio" ? "LM Studio の準備" : "OpenAI互換サーバーの準備"}
        </div>
        {lines.map((l) => (
          <div key={l}>{l}</div>
        ))}
      </div>
    </div>
  );
}

function row(label: string, field: React.ReactNode) {
  return (
    <div style={{ display: "flex", alignItems: "flex-start", gap: 10, marginBottom: 10 }}>
      <div
        style={{
          width: 92,
          flex: "none",
          textAlign: "right",
          color: "var(--panel-muted, #aeb8cc)",
          fontSize: 12,
          paddingTop: 7,
          whiteSpace: "pre-line",
        }}
      >
        {label}
      </div>
      <div style={{ flex: 1, minWidth: 0 }}>{field}</div>
    </div>
  );
}

function inp(): React.CSSProperties {
  return {
    width: "100%",
    padding: "6px 8px",
    background: "var(--panel-input-bg, #0e121c)",
    border: "1px solid var(--panel-border, #38445e)",
    borderRadius: 6,
    color: "var(--panel-text, #eef2f9)",
    fontSize: 13,
    outline: "none",
    boxSizing: "border-box",
  };
}

function smallBtn(): React.CSSProperties {
  return {
    padding: "6px 10px",
    background: "var(--panel-btn-bg, #2a3346)",
    color: "var(--panel-text, #aeb8cc)",
    border: "1px solid var(--panel-border, #38445e)",
    borderRadius: 6,
    cursor: "pointer",
    fontSize: 12,
    fontFamily: "sans-serif",
    flex: "none",
  };
}

function btn(bg: string, color: string): React.CSSProperties {
  return {
    padding: "7px 16px",
    background: bg,
    color,
    border: "none",
    borderRadius: 6,
    cursor: "pointer",
    fontSize: 13,
    fontFamily: "sans-serif",
  };
}
