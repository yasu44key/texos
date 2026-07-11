import { useState, useEffect, useRef } from "react";

interface PromptDialogProps {
  title: string;
  initial: string;
  /// true なら空欄のまま OK できる（空文字を onConfirm に渡す）。既定は空欄=キャンセル。
  allowEmpty?: boolean;
  /// メニュー再編（2026-07-12）: 1つ目の入力欄の見出し（field2 併用時の区別用・省略可）。
  label?: string;
  /// 任意の2つ目の入力欄（例:「新規ファイルを作成 → その他の拡張子」の拡張子欄）。
  /// 指定時は onConfirm の第2引数で値が返る。2欄目が空欄の OK はキャンセル扱い
  ///（1欄目の空欄=キャンセルと同じ流儀。allowEmpty は1欄目にだけ効く）。
  field2?: { label: string; initial?: string };
  onConfirm: (value: string, value2?: string) => void;
  onCancel: () => void;
}

export function PromptDialog({ title, initial, allowEmpty, label, field2, onConfirm, onCancel }: PromptDialogProps) {
  const [value, setValue] = useState(initial);
  const [value2, setValue2] = useState(field2?.initial ?? "");
  const inputRef = useRef<HTMLInputElement>(null);

  // プロンプトを連鎖させる（onConfirm 内で次の setPrompt）とダイアログは
  // 再マウントされないため、props の変化で値を初期化し直す。
  useEffect(() => {
    setValue(initial);
    setValue2(field2?.initial ?? "");
    inputRef.current?.focus();
    inputRef.current?.select();
    // field2 はオブジェクトが毎レンダー新規になり得るので initial 文字列だけを依存にする。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [title, initial, field2?.initial]);

  const confirm = () => {
    const v = value.trim();
    const v2 = value2.trim();
    if (!(v || allowEmpty)) return onCancel();
    if (field2 && !v2) return onCancel();
    onConfirm(v, field2 ? v2 : undefined);
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter") confirm();
    if (e.key === "Escape") onCancel();
  };

  const inputStyle: React.CSSProperties = {
    width: "100%",
    padding: "8px 10px",
    background: "var(--panel-input-bg, #0e121c)",
    border: "1px solid var(--panel-border, #38445e)",
    borderRadius: 6,
    color: "var(--panel-text, #eef2f9)",
    fontSize: 14,
    outline: "none",
    boxSizing: "border-box",
  };
  const fieldLabelStyle: React.CSSProperties = {
    fontSize: 12,
    opacity: 0.8,
    margin: "10px 0 4px",
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
      {/* v1.4.18: 4モーダルのテーマ追従 — 構造色は --panel-*（ビュワー系と同じ変数体系）。 */}
      <div
        onMouseDown={(e) => e.stopPropagation()}
        style={{
          background: "var(--panel-bg, #1c2130)",
          border: "1px solid var(--panel-border, #38445e)",
          borderRadius: 10,
          padding: 20,
          minWidth: 320,
          boxShadow: "0 8px 32px #000000aa",
          fontFamily: "sans-serif",
          color: "var(--panel-text, #d6dded)",
        }}
      >
        <div style={{ fontSize: 14, marginBottom: 12 }}>{title}</div>
        {label && <div style={{ ...fieldLabelStyle, marginTop: 0 }}>{label}</div>}
        <input
          ref={inputRef}
          value={value}
          onChange={(e) => setValue(e.target.value)}
          onKeyDown={onKeyDown}
          style={inputStyle}
        />
        {field2 && (
          <>
            <div style={fieldLabelStyle}>{field2.label}</div>
            <input
              value={value2}
              onChange={(e) => setValue2(e.target.value)}
              onKeyDown={onKeyDown}
              style={inputStyle}
            />
          </>
        )}
        <div style={{ display: "flex", justifyContent: "flex-end", gap: 8, marginTop: 16 }}>
          <button
            onClick={onCancel}
            style={btnStyle("var(--panel-btn-bg, #2a3346)", "var(--panel-text, #aeb8cc)")}
          >
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
