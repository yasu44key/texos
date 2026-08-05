import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { SpecInfo } from "./T2iSetupDialog";

// v1.4.1 AIカード（builtin:"ai"）専用の設定ダイアログ。生JSON編集の置き換え。
// 既存 spec の未知フィールド（accept_mime/output_ext 等）は保持してマージする。

interface Props {
  initialLabel: string;
  initialSpec: string;
  onSave: (label: string, specJson: string) => void;
  onCancel: () => void;
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

/// 既定エンドポイントのいずれか（＝ユーザーが自分で決めた宛先ではない）か。
/// サーバー切替時にこれを消して既定に戻す＝「LM Studio にしたのに宛先は 11434 のまま」を防ぐ。
const isDefaultEndpoint = (v: string) =>
  Object.values(DEFAULT_ENDPOINT).includes(v.trim().replace(/\/+$/, ""));

function parseServer(v: unknown): Server {
  return v === "openai" || v === "lmstudio" ? v : "ollama";
}

export function AiSettingsDialog({ initialLabel, initialSpec, onSave, onCancel }: Props) {
  const base = useMemo<Record<string, unknown>>(() => {
    try {
      return JSON.parse(initialSpec || "{}");
    } catch {
      return {};
    }
  }, [initialSpec]);
  const s = (k: string, d = "") => (typeof base[k] === "string" ? (base[k] as string) : d);

  const [title, setTitle] = useState(initialLabel);
  const [provider, setProvider] = useState<Server>(parseServer(base.provider));
  const [endpoint, setEndpoint] = useState(s("endpoint"));

  // サーバーを替えたら、宛先が「前のサーバーの既定」のままなら空に戻す（＝新しい既定が効く）。
  // 手で入れた宛先（遠隔・別ポート）は尊重して残す。
  const changeServer = (next: Server) => {
    setProvider(next);
    setEndpoint((cur) => (isDefaultEndpoint(cur) ? "" : cur));
  };
  const [model, setModel] = useState(s("model", "gemma3:latest"));
  const [system, setSystem] = useState(s("system"));
  const [temperature, setTemperature] = useState(
    typeof base.temperature === "number" ? String(base.temperature) : ""
  );
  // v1.4.12 制限時間（秒）。v1.7.4: 生成系共通契約に統一 — 空欄=0=無制限（変換も対話も）。
  const [timeoutSecs, setTimeoutSecs] = useState(
    typeof base.timeout_secs === "number" ? String(base.timeout_secs) : ""
  );
  const [mode, setMode] = useState<"chat" | "transform">(
    base.mode === "chat" ? "chat" : "transform"
  );
  const [logName, setLogName] = useState(s("log_name")); // v1.4.5 ログ保存名（chat）

  // v1.4.28 WS3: PCスペック検出。モデルサイズ選択の根拠として表示する。
  const [spec, setSpec] = useState<SpecInfo | null>(null);
  useEffect(() => {
    invoke<SpecInfo>("detect_spec")
      .then(setSpec)
      .catch(() => setSpec(null));
  }, []);

  // v1.4.2 モデル候補（/api/tags 等から取得）。手入力も併用可（datalist）。
  const [models, setModels] = useState<string[]>([]);
  const [modelsState, setModelsState] = useState<"idle" | "loading" | "error">("idle");
  const fetchModels = useCallback(() => {
    setModelsState("loading");
    invoke<string[]>("ai_list_models", { provider, endpoint: endpoint.trim() || null })
      .then((list) => {
        setModels(list);
        setModelsState("idle");
      })
      .catch(() => {
        setModels([]);
        setModelsState("error");
      });
  }, [provider, endpoint]);
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
    const spec: Record<string, unknown> = { ...base, builtin: "ai", provider, mode };
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
    if (mode === "chat" && logName.trim()) spec.log_name = logName.trim();
    else delete spec.log_name;
    // 接続時型チェック・出力の既定（未設定なら補う）。
    if (!spec.accept_mime) spec.accept_mime = mode === "chat" ? "*/*" : "text/*";
    if (!spec.output_ext) spec.output_ext = "md";
    onSave(title.trim() || `AI: ${spec.model}`, JSON.stringify(spec));
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
          🤖 AIカードの設定
        </div>

        {row(
          "タイトル",
          <input style={inp()} value={title} onChange={(e) => setTitle(e.target.value)} />
        )}

        {row(
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
                    if (e.target.value) setModel(e.target.value);
                  }}
                  style={{ ...inp(), width: 130, flex: "none" }}
                  title="取得したモデルから選ぶ"
                >
                  <option value="">候補 ▼</option>
                  {models.map((m) => (
                    <option key={m} value={m}>
                      {m}
                    </option>
                  ))}
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

        {row(
          "システム\nプロンプト",
          <textarea
            style={{ ...inp(), resize: "vertical", minHeight: 64, fontFamily: "inherit" }}
            value={system}
            rows={4}
            onChange={(e) => setSystem(e.target.value)}
            placeholder="（空欄可）AIの役割・口調・出力形式の指示"
          />
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

        {row(
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

        {mode === "chat" &&
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
          <button onClick={save} style={btn("#3a6fb0", "#ffffff")}>
            保存
          </button>
        </div>
      </div>
    </div>
  );
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
