import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

interface Props {
  /** committed file path to edit (edit mode). Omitted in read-only view mode. */
  path?: string | null;
  /** v1.3.9: 仮身 id — used to load LIVE content in read-only view mode. */
  objId?: string;
  /** v1.3.9: read-only 閲覧 mode (no commit, no save). Shows an 「編集」 button. */
  readOnly?: boolean;
  /** v1.3.9: invoked when the user presses 「編集」 in read-only mode (= commit→edit). */
  onRequestEdit?: () => void;
  /** vertical offset from the card origin (card height + gap) */
  top: number;
  /** panel width (v1.3.1, user-resizable; persisted by the parent) */
  width?: number | null;
  onResizeEnd?: (width: number) => void;
  onClose: () => void;
  onSaved?: () => void;
}

/** A text panel anchored to its parent card (shares the card's origin, so it
 *  moves together when the card is dragged).
 *  v1.3.9: two modes —
 *    edit mode (path set): editable, 保存/保存して閉じる.
 *    read-only 閲覧 mode (readOnly + objId): loads LIVE content via read_view_text,
 *    no save; an 「編集」 button promotes to edit (commit→実身) via onRequestEdit. */
export function InlineTextEditor({
  path = null,
  objId,
  readOnly = false,
  onRequestEdit,
  top,
  width = null,
  onResizeEnd,
  onClose,
  onSaved,
}: Props) {
  const [content, setContent] = useState("");
  const [loaded, setLoaded] = useState(false);
  // v1.4.13 仮身の閲覧で推定したエンコ名（UTF-8 以外のときだけバッジ表示）。
  const [encoding, setEncoding] = useState<string | null>(null);
  const [dirty, setDirty] = useState(false);
  const [panelW, setPanelW] = useState(width ?? 360);
  const taRef = useRef<HTMLTextAreaElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);

  // 幅変更ドラッグ（右端）。確定時に親へ通知して永続化。
  const startResize = (e: React.MouseEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    e.stopPropagation();
    e.preventDefault();
    const layer = (e.currentTarget as HTMLElement).closest<HTMLElement>("[data-canvas-layer]");
    const scale = parseFloat(layer?.style.getPropertyValue("--canvas-scale") || "1") || 1;
    const startX = e.clientX;
    const startW = panelW;
    let curW = startW;
    const onMove = (me: globalThis.MouseEvent) => {
      curW = Math.max(220, startW + (me.clientX - startX) / scale);
      if (panelRef.current) panelRef.current.style.width = `${curW}px`;
    };
    const onUp = () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
      const w = Math.round(curW);
      setPanelW(w);
      if (w !== startW) onResizeEnd?.(w);
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  };

  useEffect(() => {
    // v1.3.9: read-only view loads LIVE content by id (no commit); edit loads the file.
    // v1.4.13: 閲覧は非UTF-8を推定デコードして {text, encoding} を返す（非破壊）。
    if (readOnly && objId) {
      invoke<{ text: string; encoding: string }>("read_view_text", { id: objId })
        .then((r) => {
          setContent(r.text);
          setEncoding(r.encoding);
          setLoaded(true);
        })
        .catch((e) => {
          setContent(`(読み込みに失敗: ${e})`);
          setLoaded(true);
        });
    } else {
      invoke<string>("read_text_file", { path })
        .then((t) => {
          setContent(t);
          setLoaded(true);
        })
        .catch((e) => {
          setContent(`(読み込みに失敗: ${e})`);
          setLoaded(true);
        });
    }
  }, [path, objId, readOnly]);

  const save = async () => {
    if (readOnly || !path) return;
    try {
      await invoke("write_text_file", { path, text: content });
      setDirty(false);
      onSaved?.();
    } catch (e) {
      console.error("write_text_file failed", e);
    }
  };

  const fileName = (path ?? "").split(/[\\/]/).pop();

  return (
    <div
      ref={panelRef}
      // Anchored to the card origin; do not start a card drag from inside.
      onMouseDown={(e) => e.stopPropagation()}
      onDoubleClick={(e) => e.stopPropagation()}
      style={{
        position: "absolute",
        left: 0,
        top,
        width: panelW,
        background: "var(--panel-bg, #101820)",
        border: `1px solid ${readOnly ? "var(--panel-border, #3a4658)" : "#2f6b5e"}`,
        borderRadius: 8,
        boxShadow: "0 4px 20px #000000aa",
        fontFamily: "sans-serif",
        overflow: "hidden",
        zIndex: 5,
      }}
    >
      <div
        style={{
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          padding: "6px 8px",
          background: "var(--panel-header-bg, #16242a)",
          fontSize: 11,
          color: "var(--panel-header-text, #bfe0d6)",
        }}
      >
        <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
          {readOnly
            ? `👁 閲覧（読み取り専用）${encoding && encoding !== "UTF-8" ? ` ・ ${encoding} 推定` : ""}`
            : `✎ ${fileName}${dirty ? " *" : ""}`}
        </span>
        <div style={{ display: "flex", gap: 6 }}>
          {readOnly ? (
            <button
              onClick={() => onRequestEdit?.()}
              style={btn("#2f6b5e")}
              title="この内容を実身化して編集します"
            >
              編集
            </button>
          ) : (
            <>
              <button onClick={save} style={btn("#2f6b5e")}>保存</button>
              <button
                onClick={() => {
                  if (dirty) save();
                  onClose();
                }}
                style={btn("#3a4658")}
              >
                保存して閉じる
              </button>
            </>
          )}
          <button onClick={onClose} style={btn("#5a2f3a")} title={readOnly ? "閉じる" : "保存せず閉じる"}>✕</button>
        </div>
      </div>
      <textarea
        ref={taRef}
        value={loaded ? content : "読み込み中..."}
        readOnly={!loaded || readOnly}
        onChange={(e) => {
          if (readOnly) return;
          setContent(e.target.value);
          setDirty(true);
        }}
        onKeyDown={(e) => {
          if (!readOnly && (e.ctrlKey || e.metaKey) && e.key === "s") {
            e.preventDefault();
            save();
          }
          e.stopPropagation();
        }}
        spellCheck={false}
        style={{
          width: "100%",
          height: 220,
          boxSizing: "border-box",
          resize: "vertical",
          background: "var(--panel-input-bg, #0b1116)",
          border: "none",
          outline: "none",
          color: "var(--panel-text, #e6eef2)",
          fontSize: 13,
          fontFamily: "ui-monospace, Consolas, monospace",
          lineHeight: 1.5,
          padding: "10px 12px",
        }}
      />
      {/* width-resize handle on the right edge (v1.3.1) */}
      <div
        onMouseDown={startResize}
        title="横幅を変更"
        style={{
          position: "absolute",
          right: 0,
          top: 0,
          bottom: 0,
          width: 7,
          cursor: "ew-resize",
        }}
      />
    </div>
  );
}

function btn(bg: string): React.CSSProperties {
  return {
    padding: "3px 10px",
    background: bg,
    color: "#fff",
    border: "none",
    borderRadius: 5,
    cursor: "pointer",
    fontSize: 11,
    fontFamily: "sans-serif",
  };
}
