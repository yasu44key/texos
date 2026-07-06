// シェルカード統合（2026-07-06）: window:"hidden" シェル実行の結果パネル。
// シェルウィンドウを出さない実行では stdout/stderr の行き場がない（odt 指摘）ため、
// 捕捉した出力（2>&1 時系列マージ＝ウィンドウに見えるものと同じ並び）と終了コードを
// モノスペースで表示する。コピー可・Esc/OK で閉じる。

interface Props {
  /** 実行したシェルカードの表示名（見出し用）。 */
  label: string;
  exitCode: number | null;
  output: string;
  truncated: boolean;
  onClose: () => void;
}

export function ShellOutputDialog({ label, exitCode, output, truncated, onClose }: Props) {
  const ok = exitCode === 0;
  const copy = () => {
    void navigator.clipboard?.writeText(output);
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
      onMouseDown={onClose}
    >
      <div
        onMouseDown={(e) => e.stopPropagation()}
        style={{
          background: "var(--panel-bg, #1c2130)",
          border: "1px solid var(--panel-border, #38445e)",
          borderRadius: 10,
          padding: 20,
          width: 680,
          maxWidth: "92vw",
          maxHeight: "85vh",
          display: "flex",
          flexDirection: "column",
          boxShadow: "0 8px 32px #000000aa",
          fontFamily: "sans-serif",
          color: "var(--panel-text, #d6dded)",
          fontSize: 13,
        }}
      >
        <div style={{ display: "flex", alignItems: "baseline", gap: 10, marginBottom: 10 }}>
          <div style={{ fontSize: 15 }}>実行結果: {label}</div>
          <div
            style={{
              fontSize: 12,
              padding: "2px 8px",
              borderRadius: 10,
              flex: "none",
              background: ok ? "#2f5d3a66" : "#5d2f2f66",
              color: ok ? "#7fd692" : "#d97f7f",
            }}
          >
            {exitCode === null ? "✗ 終了コード不明" : ok ? "✓ 正常終了 (0)" : `✗ 終了コード ${exitCode}`}
          </div>
        </div>
        <pre
          style={{
            flex: 1,
            minHeight: 120,
            margin: 0,
            padding: 10,
            overflow: "auto",
            whiteSpace: "pre-wrap",
            overflowWrap: "anywhere",
            background: "var(--panel-input-bg, #0e121c)",
            border: "1px solid var(--panel-border, #38445e)",
            borderRadius: 6,
            fontFamily: "Consolas, monospace",
            fontSize: 12.5,
            lineHeight: 1.5,
          }}
        >
          {output || "（出力はありません）"}
        </pre>
        {truncated && (
          <div style={{ fontSize: 11, color: "#d9a76a", marginTop: 6 }}>
            ⚠ 出力が長いため以下を省略しました（64K文字まで表示）。
          </div>
        )}
        <div style={{ display: "flex", justifyContent: "flex-end", gap: 8, marginTop: 12 }}>
          <button onClick={copy} style={btnStyle("#2a3346", "#aeb8cc")}>
            コピー
          </button>
          <button onClick={onClose} style={btnStyle("#3a6fb0", "#ffffff")}>
            OK
          </button>
        </div>
      </div>
    </div>
  );
}

function btnStyle(bg: string, color: string): React.CSSProperties {
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
