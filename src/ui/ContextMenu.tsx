import { useEffect, useLayoutEffect, useRef, useState } from "react";

export interface MenuItem {
  label: string;
  onClick: () => void;
  danger?: boolean;
  disabled?: boolean;
  // v1.3.4: ホバー時の丁寧な説明（showHints が false なら出さない）。
  hint?: string;
  // v1.3.4: 区切り線（label/onClick は無視される）。
  divider?: boolean;
  // v1.3.7: 対応するショートカット（"Ctrl+C" 等）。ラベル右に淡色で併記。
  shortcut?: string;
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
  // v1.3.7 memo①: 画面内に収まるよう、表示後に実寸を測って位置を補正する。
  // 右端/下端で見切れて操作できない問題を解消（サブメニューで高さが変わっても再補正）。
  const [pos, setPos] = useState({ x, y });
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    const PAD = 8;
    let nx = x;
    let ny = y;
    if (x + r.width > window.innerWidth - PAD) nx = Math.max(PAD, window.innerWidth - r.width - PAD);
    if (y + r.height > window.innerHeight - PAD) ny = Math.max(PAD, window.innerHeight - r.height - PAD);
    setPos({ x: nx, y: ny });
  }, [x, y, items]);

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
          left: pos.x,
          top: pos.y,
          minWidth: 180,
          background: "var(--menu-bg, #1c2130)",
          border: "1px solid var(--menu-border, #38445e)",
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
                background: "var(--menu-border, #38445e)",
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
                display: "flex",
                alignItems: "center",
                justifyContent: "space-between",
                gap: 16,
                padding: "7px 12px",
                borderRadius: 5,
                color: item.disabled
                  ? "var(--menu-dim, #5a6378)"
                  : item.danger
                  ? "var(--menu-danger, #ff7b7b)"
                  : "var(--menu-text, #d6dded)",
                cursor: item.disabled ? "default" : "pointer",
              }}
              onMouseEnter={(e) => {
                if (!item.disabled)
                  (e.currentTarget as HTMLElement).style.background = item.danger
                    ? "var(--menu-danger-bg, #3a2030)"
                    : "var(--menu-hover, #2a3346)";
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
              <span>{item.label}</span>
              {item.shortcut && (
                <span style={{ color: "var(--menu-dim, #5a6378)", fontSize: 11, whiteSpace: "nowrap" }}>
                  {item.shortcut}
                </span>
              )}
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
            background: "var(--tip-bg, #11151f)",
            border: "1px solid var(--menu-border, #38445e)",
            borderRadius: 6,
            boxShadow: "0 4px 16px #000000aa",
            padding: "7px 10px",
            zIndex: 1001,
            fontFamily: "sans-serif",
            fontSize: 12,
            lineHeight: 1.5,
            color: "var(--tip-text, #aeb8cc)",
            pointerEvents: "none",
          }}
        >
          {tip.text}
        </div>
      )}
    </>
  );
}
