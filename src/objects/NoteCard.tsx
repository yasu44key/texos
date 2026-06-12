import { useRef, useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";

interface Props {
  id: string;
  text: string;
  x: number;
  y: number;
  width?: number;
  fontSize?: number;
  bold?: boolean;
  fileRef?: string | null;
  startEditing?: boolean;
  /** stacking order (kept above plain cards, below expanded panels) */
  zIndex?: number;
  /** part of the current selection (rubber-band / shape group) — v1.3.1 */
  selected?: boolean;
  /** Ctrl+click: toggle this note in/out of the selection (v1.3 メモ２追加). */
  onToggleSelect?: (id: string) => void;
  /** When set and selected, delegate the drag to the parent (group move). */
  onGroupDragStart?: (id: string, clientX: number, clientY: number) => void;
  /** Begin dragging an arrow link from this note (v1.3.1). */
  onStartLink?: (fromId: string, clientX: number, clientY: number) => void;
  /** Reports the ACTUAL rendered size (canvas px) so arrows attach exactly. */
  onMeasure?: (id: string, w: number, h: number) => void;
  onMoveEnd?: (id: string, x: number, y: number) => void;
  onSave?: (id: string, text: string) => void;
  onResize?: (id: string, width: number) => void;
}

export function NoteCard({
  id, text, x, y,
  width = 220, fontSize = 13, bold = false, fileRef = null,
  startEditing, zIndex = 0, selected = false,
  onToggleSelect, onGroupDragStart, onStartLink, onMeasure, onMoveEnd, onSave, onResize,
}: Props) {
  const posRef = useRef({ x, y });
  posRef.current = { x, y };
  const [editing, setEditing] = useState(!!startEditing);
  const [content, setContent] = useState(text);
  const [draft, setDraft] = useState(text);
  const [hover, setHover] = useState(false);
  const taRef = useRef<HTMLTextAreaElement>(null);
  const rootRef = useRef<HTMLDivElement>(null);

  // Report the real rendered size (transform-scale independent) so link
  // arrows attach to the actual edge, not an estimate (v1.3.1 fix).
  useEffect(() => {
    const el = rootRef.current;
    if (!el || !onMeasure) return;
    const report = () => onMeasure(id, el.offsetWidth, el.offsetHeight);
    report();
    const ro = new ResizeObserver(report);
    ro.observe(el);
    return () => ro.disconnect();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [id]);

  // For file-linked notes, load fresh content from the file.
  useEffect(() => {
    if (fileRef) {
      invoke<string>("read_text_file", { path: fileRef })
        .then((t) => setContent(t))
        .catch(() => {});
    } else {
      setContent(text);
    }
  }, [fileRef, text]);

  useEffect(() => {
    if (editing) {
      setDraft(content);
      taRef.current?.focus();
      taRef.current?.select();
    }
  }, [editing]); // eslint-disable-line react-hooks/exhaustive-deps

  const commit = () => {
    setEditing(false);
    if (draft !== content) {
      setContent(draft);
      onSave?.(id, draft);
    }
  };

  const startDrag = (e: React.MouseEvent<HTMLElement>, mode: "move" | "resize") => {
    if (e.button !== 0 || e.altKey) return;
    e.stopPropagation();
    e.preventDefault();

    // Ctrl+クリック = 選択に個別追加／解除（ドラッグは始めない）。
    if (mode === "move" && (e.ctrlKey || e.metaKey)) {
      onToggleSelect?.(id);
      return;
    }

    // Part of a multi-selection → the parent moves the whole group (v1.3.1).
    if (mode === "move" && selected && onGroupDragStart) {
      onGroupDragStart(id, e.clientX, e.clientY);
      return;
    }
    const layer = (e.currentTarget as HTMLElement).closest<HTMLElement>("[data-canvas-layer]");
    const scale = parseFloat(layer?.style.getPropertyValue("--canvas-scale") || "1") || 1;
    const startMouseX = e.clientX;
    const startMouseY = e.clientY;
    const card = (e.currentTarget as HTMLElement).closest<HTMLElement>("[data-note-id]")!;
    const startX = posRef.current.x;
    const startY = posRef.current.y;
    const startW = width;
    let curX = startX, curY = startY, curW = startW;

    const onMove = (me: globalThis.MouseEvent) => {
      if (mode === "move") {
        curX = startX + (me.clientX - startMouseX) / scale;
        curY = startY + (me.clientY - startMouseY) / scale;
        card.style.transform = `translate(${curX}px, ${curY}px)`;
      } else {
        curW = Math.max(120, startW + (me.clientX - startMouseX) / scale);
        card.style.width = `${curW}px`;
      }
    };
    const onUp = () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
      if (mode === "move") onMoveEnd?.(id, curX, curY);
      else onResize?.(id, Math.round(curW));
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  };

  return (
    <div
      ref={rootRef}
      data-note-id={id}
      style={{
        position: "absolute",
        left: 0,
        top: 0,
        transform: `translate(${x}px, ${y}px)`,
        width,
        minHeight: 40,
        zIndex,
        background: fileRef ? "var(--linked-note-bg, #1f2c2a)" : "var(--note-bg, #3a3520)",
        border: `1px solid ${fileRef ? "var(--linked-note-border, #3c6b5e)" : "var(--note-border, #6b5f2e)"}`,
        borderRadius: 8,
        boxShadow: "var(--card-shadow, 0 2px 14px #00000070)",
        outline: selected ? "2px solid var(--accent, #4a90d9)" : "none",
        outlineOffset: 1,
        color: fileRef ? "var(--linked-note-text, #dff0ea)" : "var(--note-text, #f3ecd0)",
        fontSize,
        fontWeight: bold ? 700 : 400,
        fontFamily: "sans-serif",
        lineHeight: 1.5,
        cursor: editing ? "text" : "grab",
        willChange: "transform",
        padding: editing ? 0 : "10px 12px",
        whiteSpace: "pre-wrap",
        wordBreak: "break-word",
      }}
      onMouseDown={(e) => startDrag(e, "move")}
      onDoubleClick={(e) => {
        e.stopPropagation();
        setEditing(true);
      }}
      onMouseEnter={() => setHover(true)}
      onMouseLeave={() => setHover(false)}
    >
      {fileRef && !editing && (
        <div style={{ fontSize: 9, color: "#6fae9c", marginBottom: 4, fontWeight: 400 }}>
          🔗 {fileRef.split(/[\\/]/).pop()}
        </div>
      )}
      {editing ? (
        <textarea
          ref={taRef}
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onBlur={commit}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              setDraft(content);
              setEditing(false);
            }
            if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) commit();
            e.stopPropagation();
          }}
          onMouseDown={(e) => e.stopPropagation()}
          placeholder={fileRef ? "テキストを編集（Ctrl+Enterで保存）" : "コメントを入力（Ctrl+Enterで確定）"}
          style={{
            width: "100%",
            minHeight: 100,
            boxSizing: "border-box",
            resize: "vertical",
            background: fileRef ? "var(--linked-note-bg, #162421)" : "var(--note-edit-bg, #2c2818)",
            border: "none",
            outline: "none",
            color: "inherit",
            fontSize,
            fontWeight: bold ? 700 : 400,
            fontFamily: "sans-serif",
            lineHeight: 1.5,
            padding: "10px 12px",
            borderRadius: 8,
          }}
        />
      ) : content ? (
        content
      ) : (
        <span style={{ opacity: 0.55, fontWeight: 400 }}>（ダブルクリックで編集）</span>
      )}

      {/* connection handle on right edge — appears on hover (v1.3.1) */}
      {!editing && onStartLink && (
        <div
          onMouseDown={(e) => {
            if (e.button !== 0) return;
            e.stopPropagation();
            e.preventDefault();
            onStartLink(id, e.clientX, e.clientY);
          }}
          title="ドラッグして他のカードへリンク"
          style={{
            position: "absolute",
            right: -8,
            top: "50%",
            transform: "translateY(-50%)",
            width: 16,
            height: 16,
            borderRadius: "50%",
            background: "var(--accent, #4a90d9)",
            border: "2px solid #cfe3fb",
            boxShadow: "0 1px 4px #00000088",
            cursor: "crosshair",
            opacity: hover ? 1 : 0,
            transition: "opacity 0.12s",
            zIndex: 2,
          }}
        />
      )}

      {/* resize handle (bottom-right) */}
      {!editing && (
        <div
          onMouseDown={(e) => startDrag(e, "resize")}
          title="幅を変更"
          style={{
            position: "absolute",
            right: 2,
            bottom: 2,
            width: 12,
            height: 12,
            cursor: "ew-resize",
            borderRight: "2px solid #ffffff44",
            borderBottom: "2px solid #ffffff44",
            borderBottomRightRadius: 6,
          }}
        />
      )}
    </div>
  );
}
