import { useEffect, useRef } from "react";

export interface MenuItem {
  label: string;
  onClick: () => void;
  danger?: boolean;
  disabled?: boolean;
}

interface ContextMenuProps {
  x: number;
  y: number;
  items: MenuItem[];
  onClose: () => void;
}

export function ContextMenu({ x, y, items, onClose }: ContextMenuProps) {
  const ref = useRef<HTMLDivElement>(null);

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
      {items.map((item, i) => (
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
          }}
          onMouseLeave={(e) => {
            (e.currentTarget as HTMLElement).style.background = "transparent";
          }}
        >
          {item.label}
        </div>
      ))}
    </div>
  );
}
