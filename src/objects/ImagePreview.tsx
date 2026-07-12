import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { usePanelResize, widthHandleStyle } from "./panelResize";

interface Props {
  objId: string;
  label: string;
  /** vertical offset from the card origin (card height + gap) */
  top: number;
  /** panel width (v1.5.16, user-resizable; persisted by the parent) */
  width?: number | null;
  onResizeEnd?: (width: number) => void;
  onClose: () => void;
}

/** Full-image preview anchored to its card (moves with it), proper aspect ratio. */
export function ImagePreview({ objId, label, top, width = null, onResizeEnd, onClose }: Props) {
  const [src, setSrc] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  // v1.5.16: 右端ハンドルで幅リサイズ（高さは画像のアスペクト追従）。確定時に親が永続化。
  const { w, panelRef, startWidthDrag } = usePanelResize({
    width: width ?? 420,
    height: 0,
    minW: 200,
    onEnd: (rw) => onResizeEnd?.(rw),
  });

  useEffect(() => {
    invoke<string>("get_preview", { id: objId })
      .then(setSrc)
      .catch(() => setFailed(true));
  }, [objId]);

  return (
    <div
      ref={panelRef}
      onMouseDown={(e) => e.stopPropagation()}
      onDoubleClick={(e) => e.stopPropagation()}
      style={{
        position: "absolute",
        left: 0,
        top,
        width: w,
        background: "var(--panel-bg, #0d0f16)",
        border: "1px solid var(--panel-border, #2a3a5a)",
        borderRadius: 8,
        boxShadow: "0 4px 20px #000000aa",
        overflow: "hidden",
        zIndex: 5,
        fontFamily: "sans-serif",
      }}
    >
      <div
        style={{
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          padding: "6px 8px",
          background: "var(--panel-header-bg, #161b26)",
          fontSize: 11,
          color: "var(--panel-header-text, #c7d2e6)",
        }}
      >
        <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
          🖼 {label}
        </span>
        <button
          onClick={onClose}
          style={{
            padding: "3px 9px",
            background: "var(--panel-btn-bg, #3a4658)",
            color: "#fff",
            border: "none",
            borderRadius: 5,
            cursor: "pointer",
            fontSize: 11,
          }}
        >
          ✕
        </button>
      </div>
      <div
        style={{
          padding: 8,
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          // checkerboard so transparency is visible
          background:
            "repeating-conic-gradient(var(--panel-checker-a, #1a1f2b) 0% 25%, var(--panel-checker-b, #141821) 0% 50%) 50% / 20px 20px",
        }}
      >
        {failed ? (
          <span style={{ color: "var(--panel-danger, #ff7b7b)", fontSize: 12, padding: 20 }}>
            プレビューを生成できませんでした
          </span>
        ) : src ? (
          <img
            src={src}
            draggable={false}
            // v1.5.16: パネル幅に追従（アスペクト比維持）。広げれば拡大・狭めれば縮小。
            style={{ width: "100%", height: "auto", display: "block", pointerEvents: "none" }}
          />
        ) : (
          <span style={{ color: "var(--panel-muted, #7a8aa6)", fontSize: 12, padding: 30 }}>読み込み中...</span>
        )}
      </div>
      <div onMouseDown={startWidthDrag} title="横幅を変更（高さは自動）" style={widthHandleStyle} />
    </div>
  );
}
