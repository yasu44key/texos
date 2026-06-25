import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

// v1.4.1 AIカード（builtin:"ai"）専用の設定ダイアログ。生JSON編集の置き換え。
// 既存 spec の未知フィールド（accept_mime/output_ext 等）は保持してマージする。

interface Props {
  initialLabel: string;
  initialSpec: string;
  onSave: (label: string, specJson: string) => void;
  onCancel: () => void;
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
  const [provider, setProvider] = useState<"ollama" | "openai">(
    base.provider === "openai" ? "openai" : "ollama"
  );
  const [endpoint, setEndpoint] = useState(s("endpoint"));
  const [model, setModel] = useState(s("model", "qwen2.5:3b"));
  const [system, setSystem] = useState(s("system"));
  const [temperature, setTemperature] = useState(
    typeof base.temperature === "number" ? String(base.temperature) : ""
  );
  const [mode, setMode] = useState<"chat" | "transform">(
    base.mode === "chat" ? "chat" : "transform"
  );
  const [logName, setLogName] = useState(s("log_name")); // v1.4.5 ログ保存名（chat）

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

  const endpointPlaceholder =
    provider === "openai"
      ? "空欄 = http://localhost:1234/v1（LM Studio）"
      : "空欄 = http://localhost:11434（Ollama）";

  const save = () => {
    const spec: Record<string, unknown> = { ...base, builtin: "ai", provider, mode };
    spec.model = model.trim() || "qwen2.5:3b";
    if (endpoint.trim()) spec.endpoint = endpoint.trim();
    else delete spec.endpoint;
    if (system.trim()) spec.system = system;
    else delete spec.system;
    const t = parseFloat(temperature);
    if (temperature.trim() !== "" && !Number.isNaN(t)) spec.temperature = t;
    else delete spec.temperature;
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
          background: "#1c2130",
          border: "1px solid #38445e",
          borderRadius: 10,
          padding: 20,
          width: 520,
          maxHeight: "85vh",
          overflowY: "auto",
          boxShadow: "0 8px 32px #000000aa",
          fontFamily: "sans-serif",
          color: "#d6dded",
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
          "プロバイダ",
          <div style={{ display: "flex", gap: 18 }}>
            {(
              [
                ["ollama", "Ollama（ネイティブ）"],
                ["openai", "OpenAI準拠"],
              ] as ["ollama" | "openai", string][]
            ).map(([p, label]) => (
              <label key={p} style={{ display: "flex", alignItems: "center", gap: 5, cursor: "pointer" }}>
                <input type="radio" checked={provider === p} onChange={() => setProvider(p)} />
                {label}
              </label>
            ))}
          </div>
        )}

        {row(
          "エンドポイント",
          <input
            style={inp()}
            value={endpoint}
            onChange={(e) => setEndpoint(e.target.value)}
            placeholder={endpointPlaceholder}
          />
        )}

        {row(
          "モデル",
          <div>
            <div style={{ display: "flex", gap: 6 }}>
              <input
                style={{ ...inp(), flex: 1 }}
                value={model}
                onChange={(e) => setModel(e.target.value)}
                placeholder="qwen2.5:3b"
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
            <div style={{ fontSize: 11, color: "#8a93a8", marginTop: 3 }}>
              {modelsState === "loading"
                ? "取得中…"
                : modelsState === "error"
                ? "一覧を取得できません（接続先が起動しているか確認）"
                : models.length
                ? `${models.length}件取得 — 「候補▼」から選択・手入力も可`
                : "「↻取得」で候補を表示（手入力も可）"}
            </div>
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
            <span style={{ fontSize: 11, color: "#8a93a8" }}>
              0=厳密 / 0.7=標準 / 高=多様（空欄=モデル既定）
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

function row(label: string, field: React.ReactNode) {
  return (
    <div style={{ display: "flex", alignItems: "flex-start", gap: 10, marginBottom: 10 }}>
      <div
        style={{
          width: 92,
          flex: "none",
          textAlign: "right",
          color: "#aeb8cc",
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
    background: "#0e121c",
    border: "1px solid #38445e",
    borderRadius: 6,
    color: "#eef2f9",
    fontSize: 13,
    outline: "none",
    boxSizing: "border-box",
  };
}

function smallBtn(): React.CSSProperties {
  return {
    padding: "6px 10px",
    background: "#2a3346",
    color: "#aeb8cc",
    border: "1px solid #38445e",
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
