interface ConfirmDialogProps {
  title: string;
  message?: string;
  /** red emphasized text (e.g. 外部参照・プログラムリンクの警告) */
  warning?: string;
  okLabel?: string;
  cancelLabel?: string;
  onConfirm: () => void;
  onCancel: () => void;
}

export function ConfirmDialog({
  title,
  message,
  warning,
  okLabel = "OK",
  cancelLabel = "キャンセル",
  onConfirm,
  onCancel,
}: ConfirmDialogProps) {
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
      {/* v1.4.18: 4モーダルのテーマ追従 — 構造色は --panel-*（ビュワー系と同じ変数体系）。 */}
      <div
        onMouseDown={(e) => e.stopPropagation()}
        style={{
          background: "var(--panel-bg, #1c2130)",
          border: "1px solid var(--panel-border, #38445e)",
          borderRadius: 10,
          padding: 20,
          maxWidth: 380,
          boxShadow: "0 8px 32px #000000aa",
          fontFamily: "sans-serif",
          color: "var(--panel-text, #d6dded)",
        }}
      >
        <div style={{ fontSize: 15, fontWeight: 600, marginBottom: message ? 8 : 16 }}>
          {title}
        </div>
        {message && (
          <div style={{
            fontSize: 13,
            lineHeight: 1.6,
            color: "var(--panel-muted, #aeb8cc)",
            marginBottom: warning ? 10 : 16,
            whiteSpace: "pre-wrap",
            wordBreak: "break-all",
          }}>
            {message}
          </div>
        )}
        {warning && (
          <div style={{
            fontSize: 13,
            lineHeight: 1.6,
            color: "var(--panel-danger, #e06060)",
            background: "#3a1c1c55",
            border: "1px solid #6e2e2e",
            borderRadius: 6,
            padding: "8px 10px",
            marginBottom: 16,
            whiteSpace: "pre-wrap",
            wordBreak: "break-all",
            maxHeight: 180,
            overflowY: "auto",
          }}>
            {warning}
          </div>
        )}
        <div style={{ display: "flex", justifyContent: "flex-end", gap: 8 }}>
          <button
            onClick={onCancel}
            style={btn("var(--panel-btn-bg, #2a3346)", "var(--panel-text, #aeb8cc)")}
          >
            {cancelLabel}
          </button>
          <button onClick={onConfirm} style={btn("#3a6fb0", "#ffffff")}>
            {okLabel}
          </button>
        </div>
      </div>
    </div>
  );
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
