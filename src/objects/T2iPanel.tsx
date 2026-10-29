import { useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { panelMouseDown } from "./panelResize";

// v1.4.17 画像生成カード（builtin:"t2i"）のインライン生成パネル。ChatPanel と同じ流儀で
// カード下に展開（ダブルクリックでトグル）。ポジティブ（追加プロンプト）とネガティブを
// 編集しつつ、その場で「生成」できる。プロンプト本体は接続テキストカード＝主・ここは追加分
// （入力が無ければパネル単独で生成）。保存先は filter_ref の prompt / negative（update_filter。
// 生成パラメータは従来どおり右クリック→設定）。

interface Props {
  objId: string;
  label: string;
  /** anchored offset from the card top (other panels use CARD_H + 8) */
  top: number;
  /** 現在の filter_ref JSON。保存はこれをベースに prompt/negative だけ差し替える。 */
  filterRef: string | null | undefined;
  onClose: () => void;
  /** ▶実行と同じフロー実行（App.handleRunFlow）。 */
  onRun?: (id: string) => void;
  /** 保存後の refresh（App.onAfterSave）。 */
  onSaved?: () => void;
}

const PANEL_W = 380;

function parseSpec(ref: string | null | undefined): Record<string, unknown> {
  try {
    return JSON.parse(ref ?? "{}") as Record<string, unknown>;
  } catch {
    return {};
  }
}

export function T2iPanel({ objId, label, top, filterRef, onClose, onRun, onSaved }: Props) {
  const init = parseSpec(filterRef);
  const [prompt, setPrompt] = useState<string>((init.prompt as string) ?? "");
  const [negative, setNegative] = useState<string>((init.negative as string) ?? "");
  const [status, setStatus] = useState<string | null>(null);
  const savedRef = useRef({
    prompt: (init.prompt as string) ?? "",
    negative: (init.negative as string) ?? "",
  });

  // 最新の filter_ref をベースに prompt/negative だけ差し込んで保存（他の設定を消さない）。
  // JSON.stringify は undefined のキーを落とす＝「空は送らない」の流儀そのまま。
  const save = async (): Promise<boolean> => {
    if (prompt === savedRef.current.prompt && negative === savedRef.current.negative) return true;
    try {
      const spec = parseSpec(filterRef);
      spec.prompt = prompt.trim() || undefined;
      spec.negative = negative.trim() || undefined;
      await invoke("update_filter", { id: objId, spec: JSON.stringify(spec) });
      savedRef.current = { prompt, negative };
      onSaved?.();
      return true;
    } catch (e) {
      setStatus("保存に失敗しました: " + String(e));
      return false;
    }
  };

  const run = async () => {
    setStatus(null);
    if (!(await save())) return;
    setStatus("生成を開始しました…（結果カードは右に生まれます）");
    onRun?.(objId);
  };

  const close = async () => {
    await save(); // 閉じる＝保存（編集を失わない）
    onClose();
  };

  const onKey = (e: React.KeyboardEvent) => {
    // Ctrl+Enter=生成（textarea 内の Enter は改行のまま）。
    if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      void run();
    }
  };

  return (
    <div
      onMouseDown={panelMouseDown}
      onDoubleClick={(e) => e.stopPropagation()}
      style={{
        position: "absolute",
        top,
        left: 0,
        width: PANEL_W,
        display: "flex",
        flexDirection: "column",
        background: "var(--panel-bg, #11161f)",
        border: "1px solid #2f6a4a",
        borderRadius: 8,
        boxShadow: "0 8px 28px #000000aa",
        zIndex: 10,
        overflow: "hidden",
        cursor: "default",
      }}
    >
      {/* header */}
      <div
        style={{
          display: "flex",
          alignItems: "center",
          gap: 6,
          padding: "6px 8px",
          background: "var(--panel-header-bg, #14281e)",
          borderBottom: "1px solid #2f6a4a",
          fontSize: 12,
          color: "var(--panel-header-text, #7fd8b0)",
        }}
      >
        <span style={{ flex: 1, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
          🎨 {label}
        </span>
        <button onClick={() => void close()} title="閉じる（編集は保存されます）" style={hdrBtn}>
          ✕
        </button>
      </div>

      {/* positive */}
      <div style={{ padding: "8px 8px 0", display: "flex", flexDirection: "column", gap: 4 }}>
        <span style={fieldLab}>追加プロンプト（接続テキストの後ろに足されます・無ければ単独で生成）</span>
        <textarea
          value={prompt}
          onChange={(e) => setPrompt(e.target.value)}
          onKeyDown={onKey}
          placeholder="例: masterpiece, watercolor, soft light"
          rows={4}
          style={ta(13)}
        />
      </div>

      {/* negative（やや小さく） */}
      <div style={{ padding: "6px 8px 0", display: "flex", flexDirection: "column", gap: 4 }}>
        <span style={fieldLab}>ネガティブ（除外したい要素）</span>
        <textarea
          value={negative}
          onChange={(e) => setNegative(e.target.value)}
          onKeyDown={onKey}
          placeholder="例: lowres, blurry, bad hands"
          rows={2}
          style={ta(12)}
        />
      </div>

      {status && (
        <div style={{ padding: "6px 8px 0", fontSize: 11, color: "var(--panel-muted, #9fc9b3)" }}>
          {status}
        </div>
      )}

      {/* footer: 実行 */}
      <div
        style={{
          display: "flex",
          alignItems: "center",
          gap: 8,
          padding: 8,
          marginTop: 6,
          borderTop: "1px solid var(--panel-border, #243043)",
          background: "var(--panel-header-bg, #0e121c)",
        }}
      >
        <span style={{ flex: 1, fontSize: 10, color: "var(--panel-muted, #6b7686)" }}>
          Ctrl+Enter でも実行。パラメータは右クリック → 設定
        </span>
        <button onClick={() => void run()} title="このフローを実行して画像を生成します" style={runBtn}>
          ▶ 生成
        </button>
      </div>
    </div>
  );
}

const fieldLab: React.CSSProperties = {
  fontSize: 11,
  color: "var(--panel-muted, #9fc9b3)",
};

function ta(fontSize: number): React.CSSProperties {
  return {
    resize: "vertical",
    background: "var(--panel-input-bg, #0a0e16)",
    border: "1px solid var(--panel-border, #2a3346)",
    borderRadius: 6,
    color: "var(--panel-text, #e6ecf6)",
    fontSize,
    padding: "6px 8px",
    outline: "none",
    fontFamily: "inherit",
    boxSizing: "border-box",
    width: "100%",
  };
}

const hdrBtn: React.CSSProperties = {
  padding: "2px 8px",
  fontSize: 11,
  color: "#fff",
  background: "var(--panel-btn-bg, #1c2a22)",
  border: "1px solid var(--panel-border, #2f6a4a)",
  borderRadius: 5,
  cursor: "pointer",
  flex: "none",
};

const runBtn: React.CSSProperties = {
  padding: "6px 16px",
  fontSize: 13,
  fontWeight: 600,
  color: "#fff",
  background: "#2f6a4a",
  border: "none",
  borderRadius: 6,
  cursor: "pointer",
  flex: "none",
  fontFamily: "sans-serif",
};
