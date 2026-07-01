import { useRef, useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";

// v1.4.16 付箋の充実: 付箋色（hex）から背景・枠・文字色を決める。色未指定（null）は
// 従来どおりテーマ CSS 変数（fileRef 有無で黄／ティール）。明度で文字色を自動反転。
function noteColors(color: string | null | undefined, fileRef: string | null) {
  const m = color ? /^#?([0-9a-fA-F]{6})$/.exec(color.trim()) : null;
  if (m) {
    const hex = `#${m[1]}`;
    const n = parseInt(m[1], 16);
    const r = (n >> 16) & 255;
    const g = (n >> 8) & 255;
    const b = n & 255;
    const lum = (0.299 * r + 0.587 * g + 0.114 * b) / 255;
    const dark = lum < 0.5;
    return {
      bg: hex,
      editBg: hex,
      border: dark ? "rgba(255,255,255,0.28)" : "rgba(0,0,0,0.32)",
      text: dark ? "#f3ecd0" : "#161616",
    };
  }
  return fileRef
    ? {
        bg: "var(--linked-note-bg, #1f2c2a)",
        editBg: "var(--linked-note-bg, #162421)",
        border: "var(--linked-note-border, #3c6b5e)",
        text: "var(--linked-note-text, #dff0ea)",
      }
    : {
        bg: "var(--note-bg, #3a3520)",
        editBg: "var(--note-edit-bg, #2c2818)",
        border: "var(--note-border, #6b5f2e)",
        text: "var(--note-text, #f3ecd0)",
      };
}

interface Props {
  id: string;
  text: string;
  x: number;
  y: number;
  width?: number;
  fontSize?: number;
  bold?: boolean;
  color?: string | null;
  /** v1.4.16: 先頭行を見出しにするフォントサイズ（null=見出しなし）。 */
  titleSize?: number | null;
  fileRef?: string | null;
  startEditing?: boolean;
  /** stacking order (kept above plain cards, below expanded panels) */
  zIndex?: number;
  /** part of the current selection (rubber-band / shape group) — v1.3.1 */
  selected?: boolean;
  /** Ctrl+click: toggle this note in/out of the selection (v1.3 メモ２追加). */
  onToggleSelect?: (id: string) => void;
  /** 素クリック（未選択時）: このコメントだけを選択（v1.3.8 マウスモデル刷新）。 */
  onSelect?: (id: string) => void;
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
  width = 220, fontSize = 13, bold = false, color = null, titleSize = null, fileRef = null,
  startEditing, zIndex = 0, selected = false,
  onToggleSelect, onSelect, onGroupDragStart, onStartLink, onMeasure, onMoveEnd, onSave, onResize,
}: Props) {
  const c = noteColors(color, fileRef);
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

    // v1.3.8 マウスモデル: Shift = この1枚だけ移動（複数選択中でも単体）。
    const moveSingleOnly = mode === "move" && e.shiftKey;

    // 素クリック（未選択）= このコメントだけを選択（他は解除）。
    if (mode === "move" && !moveSingleOnly && !selected) {
      onSelect?.(id);
    }

    // 複数選択の1枚を素クリック/ドラッグ → 群移動（親がドラッグ or クリックで1枚に絞る）。
    if (mode === "move" && !moveSingleOnly && selected && onGroupDragStart) {
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
        document.body.style.cursor = "grabbing"; // 手のひらはドラッグ開始後（G5）
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
      document.body.style.cursor = "";
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
        background: c.bg,
        border: `1px solid ${c.border}`,
        borderRadius: 8,
        boxShadow: "var(--card-shadow, 0 2px 14px #00000070)",
        outline: selected ? "2px solid var(--accent, #4a90d9)" : "none",
        outlineOffset: 1,
        color: c.text,
        fontSize,
        fontWeight: bold ? 700 : 400,
        fontFamily: "sans-serif",
        lineHeight: 1.5,
        cursor: editing ? "text" : "default", // ホバーは矢印のまま（G5）
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
            background: c.editBg,
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
        titleSize ? (
          // v1.4.16: 先頭行を見出し（大きく太字）、以降を本文サイズで表示。
          (() => {
            const nl = content.indexOf("\n");
            const head = nl === -1 ? content : content.slice(0, nl);
            const body = nl === -1 ? "" : content.slice(nl + 1);
            return (
              <>
                <div style={{ fontSize: titleSize, fontWeight: 700, lineHeight: 1.3 }}>{head}</div>
                {body && (
                  <div
                    style={{
                      fontSize,
                      fontWeight: bold ? 700 : 400,
                      lineHeight: 1.5,
                      marginTop: 4,
                    }}
                  >
                    {body}
                  </div>
                )}
              </>
            );
          })()
        ) : (
          content
        )
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
