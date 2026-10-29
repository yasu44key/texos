import { useEffect, useState } from "react";
import { invoke, convertFileSrc } from "@tauri-apps/api/core";
import { usePanelResize, widthHandleStyle, cornerHandleStyle, panelMouseDown } from "./panelResize";

interface Props {
  objId: string;
  label: string;
  /** "video" → <video>, "pdf" → WebView2 built-in viewer in an iframe */
  kind: "video" | "pdf";
  /** vertical offset from the card origin (card height + gap) */
  top: number;
  /** panel size (v1.5.16, user-resizable; persisted by the parent)。height は pdf のみ使う */
  width?: number | null;
  height?: number | null;
  onResizeEnd?: (width: number, height?: number) => void;
  onClose: () => void;
}

/** In-canvas PDF / video preview, served through the asset protocol. */
export function MediaPreview({
  objId, label, kind, top, width = null, height = null, onResizeEnd, onClose,
}: Props) {
  const [src, setSrc] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  // v1.5.16: 動画=右端ハンドル（高さはアスペクト追従）／PDF=右下コーナー（幅高さ独立）。
  const { w, h, panelRef, startWidthDrag, startCornerDrag } = usePanelResize({
    width: width ?? (kind === "pdf" ? 480 : 420),
    height: height ?? 600,
    minW: 260,
    minH: 220,
    onEnd: (rw, rh) => onResizeEnd?.(rw, rh),
  });

  useEffect(() => {
    invoke<string>("materialize_view", { id: objId })
      .then((path) => setSrc(convertFileSrc(path)))
      .catch((e) => {
        console.error("materialize_view failed", objId, e);
        setFailed(true);
      });
  }, [objId]);

  return (
    <div
      ref={panelRef}
      onMouseDown={panelMouseDown}
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
          {kind === "pdf" ? "📄" : "🎬"} {label}
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
      {failed ? (
        <div style={{ color: "var(--panel-danger, #ff7b7b)", fontSize: 12, padding: 20 }}>
          プレビューを生成できませんでした
        </div>
      ) : !src ? (
        <div style={{ color: "var(--panel-muted, #7a8aa6)", fontSize: 12, padding: 30 }}>読み込み中...</div>
      ) : kind === "video" ? (
        // v1.5.16: パネル幅に追従（アスペクト比維持）
        <video
          src={src}
          controls
          style={{ display: "block", width: "100%", height: "auto", background: "#000" }}
        />
      ) : (
        // v1.5.16: 高さはコーナードラッグ中 CSS 変数でライブ反映・確定値は h
        <iframe
          src={src}
          title={label}
          style={{
            display: "block",
            width: "100%",
            height: `var(--preview-body-h, ${h}px)`,
            border: "none",
            background: "#fff",
          }}
        />
      )}
      {kind === "pdf" ? (
        <div onMouseDown={startCornerDrag} title="サイズを変更" style={cornerHandleStyle} />
      ) : (
        <div onMouseDown={startWidthDrag} title="横幅を変更（高さは自動）" style={widthHandleStyle} />
      )}
    </div>
  );
}
