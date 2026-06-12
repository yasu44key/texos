import { useState, useEffect, useRef } from "react";

interface PromptDialogProps {
  title: string;
  initial: string;
  onConfirm: (value: string) => void;
  onCancel: () => void;
}

export function PromptDialog({ title, initial, onConfirm, onCancel }: PromptDialogProps) {
  const [value, setValue] = useState(initial);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    inputRef.current?.focus();
    inputRef.current?.select();
  }, []);

  const confirm = () => {
    const v = value.trim();
    if (v) onConfirm(v);
    else onCancel();
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
          minWidth: 320,
          boxShadow: "0 8px 32px #000000aa",
          fontFamily: "sans-serif",
          color: "#d6dded",
        }}
      >
        <div style={{ fontSize: 14, marginBottom: 12 }}>{title}</div>
        <input
          ref={inputRef}
          value={value}
          onChange={(e) => setValue(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") confirm();
            if (e.key === "Escape") onCancel();
          }}
          style={{
            width: "100%",
            padding: "8px 10px",
            background: "#0e121c",
            border: "1px solid #38445e",
            borderRadius: 6,
            color: "#eef2f9",
            fontSize: 14,
            outline: "none",
            boxSizing: "border-box",
          }}
        />
        <div style={{ display: "flex", justifyContent: "flex-end", gap: 8, marginTop: 16 }}>
          <button onClick={onCancel} style={btnStyle("#2a3346", "#aeb8cc")}>
            キャンセル
          </button>
          <button onClick={confirm} style={btnStyle("#3a6fb0", "#ffffff")}>
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
