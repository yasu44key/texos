import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

interface Props {
  objId: string;
  label: string;
  top: number;
  onClose: () => void;
}

/** Inline audio player anchored to its card (moves with it). */
export function AudioPlayer({ objId, label, top, onClose }: Props) {
  const [src, setSrc] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    invoke<string>("get_media_data_url", { id: objId })
      .then(setSrc)
      .catch(() => setFailed(true));
  }, [objId]);

  return (
    <div
      onMouseDown={(e) => e.stopPropagation()}
      onDoubleClick={(e) => e.stopPropagation()}
      style={{
        position: "absolute",
        left: 0,
        top,
        width: 300,
        background: "var(--panel-bg, #101820)",
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
          🎵 {label}
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
      <div style={{ padding: 10 }}>
        {failed ? (
          <span style={{ color: "var(--panel-danger, #ff7b7b)", fontSize: 12 }}>再生できませんでした</span>
        ) : src ? (
          <audio src={src} controls autoPlay style={{ width: "100%" }} />
        ) : (
          <span style={{ color: "var(--panel-muted, #7a8aa6)", fontSize: 12 }}>読み込み中...</span>
        )}
      </div>
    </div>
  );
}
