import { useEffect, useState } from "react";
import { invoke, convertFileSrc } from "@tauri-apps/api/core";

interface Props {
  objId: string;
  label: string;
  /** "video" → <video>, "pdf" → WebView2 built-in viewer in an iframe */
  kind: "video" | "pdf";
  /** vertical offset from the card origin (card height + gap) */
  top: number;
  onClose: () => void;
}

/** In-canvas PDF / video preview, served through the asset protocol. */
export function MediaPreview({ objId, label, kind, top, onClose }: Props) {
  const [src, setSrc] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    invoke<string>("materialize_view", { id: objId })
      .then((path) => setSrc(convertFileSrc(path)))
      .catch((e) => {
        console.error("materialize_view failed", objId, e);
        setFailed(true);
      });
  }, [objId]);

  const width = kind === "pdf" ? 480 : 420;

  return (
    <div
      onMouseDown={(e) => e.stopPropagation()}
      onDoubleClick={(e) => e.stopPropagation()}
      style={{
        position: "absolute",
        left: 0,
        top,
        width,
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
        <video
          src={src}
          controls
          style={{ display: "block", width: "100%", maxHeight: 420, background: "#000" }}
        />
      ) : (
        <iframe
          src={src}
          title={label}
          style={{ display: "block", width: "100%", height: 600, border: "none", background: "#fff" }}
        />
      )}
    </div>
  );
}
