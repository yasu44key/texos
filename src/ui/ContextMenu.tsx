import { useEffect, useRef, useState } from "react";

export interface MenuItem {
  label: string;
  onClick: () => void;
  danger?: boolean;
  disabled?: boolean;
  // v1.3.4: ホバー時の丁寧な説明（showHints が false なら出さない）。
  hint?: string;
  // v1.3.4: 区切り線（label/onClick は無視される）。
  divider?: boolean;
}

interface ContextMenuProps {
  x: number;
  y: number;
  items: MenuItem[];
  onClose: () => void;
  // v1.3.4: メニュー項目の説明ツールチップを出すか（設定で切替）。
  showHints?: boolean;
}

export function ContextMenu({ x, y, items, onClose, showHints = true }: ContextMenuProps) {
  const ref = useRef<HTMLDivElement>(null);
  // ホバー中の項目の説明と表示位置（自前ツールチップ）。
  const [tip, setTip] = useState<{ text: string; x: number; y: number } | null>(null);

  useEffect(() => {
    const onDown = (e: globalThis.MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    };
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    // capture so it closes before other handlers act
    window.addEventListener("mousedown", onDown, true);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", onDown, true);
      window.removeEventListener("keydown", onKey);
    };
  }, [onClose]);

  return (
    <>
      <div
        ref={ref}
        style={{
          position: "fixed",
          left: x,
          top: y,
          minWidth: 180,
          background: "#1c2130",
          border: "1px solid #38445e",
          borderRadius: 8,
          boxShadow: "0 6px 24px #000000aa",
          padding: 4,
          zIndex: 1000,
          fontFamily: "sans-serif",
          fontSize: 13,
          userSelect: "none",
        }}
        onContextMenu={(e) => e.preventDefault()}
      >
        {items.map((item, i) =>
          item.divider ? (
            <div
              key={i}
              style={{
                height: 1,
                background: "#38445e",
                margin: "4px 8px",
              }}
            />
          ) : (
            <div
              key={i}
              onClick={() => {
                if (item.disabled) return;
                item.onClick();
                onClose();
              }}
              style={{
                padding: "7px 12px",
                borderRadius: 5,
                color: item.disabled ? "#5a6378" : item.danger ? "#ff7b7b" : "#d6dded",
                cursor: item.disabled ? "default" : "pointer",
              }}
              onMouseEnter={(e) => {
                if (!item.disabled)
                  (e.currentTarget as HTMLElement).style.background = item.danger
                    ? "#3a2030"
                    : "#2a3346";
                if (showHints && item.hint) {
                  const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
                  // 既定はメニューの右側。右端に収まらなければ左側へ。
                  const toRight = r.right + 276 < window.innerWidth;
                  setTip({
                    text: item.hint,
                    x: toRight ? r.right + 8 : Math.max(8, r.left - 268),
                    y: r.top,
                  });
                }
              }}
              onMouseLeave={(e) => {
                (e.currentTarget as HTMLElement).style.background = "transparent";
                setTip(null);
              }}
            >
              {item.label}
            </div>
          )
        )}
      </div>
      {tip && (
        <div
          style={{
            position: "fixed",
            left: tip.x,
            top: tip.y,
            maxWidth: 260,
            background: "#11151f",
            border: "1px solid #38445e",
            borderRadius: 6,
            boxShadow: "0 4px 16px #000000aa",
            padding: "7px 10px",
            zIndex: 1001,
            fontFamily: "sans-serif",
            fontSize: 12,
            lineHeight: 1.5,
            color: "#aeb8cc",
            pointerEvents: "none",
          }}
        >
          {tip.text}
        </div>
      )}
    </>
  );
}
