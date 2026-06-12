import { useRef, useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { InlineTextEditor } from "./InlineTextEditor";
import { ImagePreview } from "./ImagePreview";
import { AudioPlayer } from "./AudioPlayer";
import { MediaPreview } from "./MediaPreview";

// Fixed card geometry so link endpoints are predictable from (x, y) alone.
export const CARD_W = 150;
export const CARD_H = 115;

interface Props {
  id: string;
  realId: string;
  label: string;
  mimeType: string;
  x: number;
  y: number;
  /** committed = an editable real file exists in the project folder */
  committed?: boolean;
  /** interacted with (edited/viewed) this session — transient green marker */
  active?: boolean;
  /** v1.3-⑩ 薄赤: the 仮身's window lost its live target.
   *  "cas" = showing the import-time CAS snapshot; "none" = nothing found. */
  lost?: "cas" | "none" | null;
  /** this card references another TexoS project (navigable) */
  isProject?: boolean;
  /** this card is a web URL link */
  isUrl?: boolean;
  /** this card is a link to an OS folder */
  isFolder?: boolean;
  /** stacking order (computed by the parent; expanded cards come to front) */
  zIndex?: number;
  /** part of the current rubber-band selection */
  selected?: boolean;
  /** Ctrl+click: toggle this card in/out of the selection (v1.3 メモ２追加). */
  onToggleSelect?: (id: string) => void;
  /** When set and this card is selected, delegate the drag to the parent
   *  (group move). (id, clientX, clientY) */
  onGroupDragStart?: (id: string, clientX: number, clientY: number) => void;
  /** optional manual comment shown on the card */
  memo?: string | null;
  /** original file extension (no dot), for icon selection */
  ext?: string | null;
  /** when expanded, an inline text editor is shown anchored to this card */
  expanded?: boolean;
  /** committed file path to edit when expanded */
  editPath?: string | null;
  /** width of the text editor panel (v1.3.1, user-resizable) */
  panelW?: number | null;
  onPanelResize?: (id: string, width: number) => void;
  onCollapse?: (id: string) => void;
  onAfterSave?: () => void;
  onMoveEnd?: (id: string, x: number, y: number) => void;
  /** Begin dragging a link from this card. (fromId, clientX, clientY) */
  onStartLink?: (fromId: string, clientX: number, clientY: number) => void;
  /** Double-click to open/edit this object. */
  onOpen?: (id: string) => void;
}

// Icon by file extension (covers types with no specific MIME).
const EXT_ICON: Record<string, string> = {
  // 3D
  blend: "🧊", obj: "🧊", fbx: "🧊", gltf: "🧊", glb: "🧊", stl: "🧊",
  "3mf": "🧊", dae: "🧊", ply: "🧊", usd: "🧊", usdz: "🧊",
  // design / raster editors
  psd: "🎨", ai: "🎨", xcf: "🎨", kra: "🎨", clip: "🎨", afphoto: "🎨", afdesign: "🎨",
  // office
  doc: "📄", docx: "📄", rtf: "📄",
  xls: "📊", xlsx: "📊",
  ppt: "📽", pptx: "📽",
  // archives
  zip: "🗜", rar: "🗜", "7z": "🗜", tar: "🗜", gz: "🗜",
  // code/config
  py: "💻", js: "💻", ts: "💻", rs: "💻", c: "💻", cpp: "💻", h: "💻", go: "💻",
  html: "💻", css: "💻", json: "📊", csv: "📊", xml: "💻", toml: "💻", yaml: "💻", yml: "💻",
  // exe
  exe: "⚙", msi: "⚙", bat: "⚙", sh: "⚙",
  // docs
  pdf: "📄", txt: "📝", md: "📝",
  // fonts
  ttf: "🔤", otf: "🔤",
};

// Windows shell icons (PNG data URLs) cached per extension / per exe path.
// null = OS has no icon for this type; fall back to the emoji dictionary.
const osIconCache = new Map<string, Promise<string | null>>();

function fetchOsIcon(ext: string | null, path: string | null): Promise<string | null> {
  const lower = path?.toLowerCase() ?? "";
  const perFile = lower.endsWith(".exe") || lower.endsWith(".lnk") || lower.endsWith(".ico");
  const key = perFile ? `path:${path}` : `ext:${ext?.toLowerCase() ?? ""}`;
  let p = osIconCache.get(key);
  if (!p) {
    p = invoke<string | null>("get_os_icon", { ext, path: perFile ? path : null })
      .catch(() => null);
    osIconCache.set(key, p);
  }
  return p;
}

function iconFor(mime: string, ext?: string | null): string {
  if (mime === "application/vnd.texos.project") return "📁";
  if (mime === "application/vnd.texos.url") return "🔗";
  if (mime === "application/vnd.texos.folder") return "📂";
  // Extension first — most specific for unknown MIME types.
  if (ext) {
    const hit = EXT_ICON[ext.toLowerCase()];
    if (hit) return hit;
  }
  if (mime.startsWith("image/")) return "🖼";
  if (mime.startsWith("video/")) return "🎬";
  if (mime.startsWith("audio/")) return "🎵";
  if (mime.startsWith("text/x-") || mime === "text/javascript") return "💻";
  if (mime === "text/plain" || mime === "text/markdown") return "📝";
  if (mime === "application/json" || mime === "text/csv") return "📊";
  if (mime === "application/pdf") return "📄";
  return "📦";
}

export function ObjectCard({
  id, realId, label, mimeType, x, y,
  committed = false, active = false, lost = null,
  isProject = false, isUrl = false, isFolder = false,
  ext = null, memo = null, expanded = false, editPath = null,
  panelW = null, onPanelResize,
  zIndex = 0, selected = false, onToggleSelect, onGroupDragStart,
  onMoveEnd, onStartLink, onOpen, onCollapse, onAfterSave,
}: Props) {
  const posRef = useRef({ x, y });
  posRef.current = { x, y };
  const [thumbnail, setThumbnail] = useState<string | null>(null);
  const [osIcon, setOsIcon] = useState<string | null>(null);
  const [hover, setHover] = useState(false);
  const isImage = mimeType.startsWith("image/");
  const isVideo = mimeType.startsWith("video/");
  const isPdf = mimeType === "application/pdf" || (ext ?? "").toLowerCase() === "pdf";

  useEffect(() => {
    if (isProject || isUrl || !ext) return;
    let alive = true;
    fetchOsIcon(ext, editPath).then((d) => { if (alive) setOsIcon(d); });
    return () => { alive = false; };
  }, [ext, editPath, isProject, isUrl]);

  useEffect(() => {
    if (!isImage) return;
    invoke<string>("get_thumbnail", { id })
      .then(setThumbnail)
      .catch((e) => {
        console.error("get_thumbnail failed", id, e);
        setThumbnail(null);
      });
  }, [id, realId, isImage]);

  const handleMouseDown = (e: React.MouseEvent<HTMLDivElement>) => {
    if (e.button !== 0 || e.altKey) return;
    e.stopPropagation();
    e.preventDefault();

    // Ctrl+クリック = 選択に個別追加／解除（ドラッグは始めない）。
    if (e.ctrlKey || e.metaKey) {
      onToggleSelect?.(id);
      return;
    }

    // Part of a multi-selection → the parent moves the whole group.
    if (selected && onGroupDragStart) {
      onGroupDragStart(id, e.clientX, e.clientY);
      return;
    }

    const layer = (e.currentTarget as HTMLElement).closest<HTMLElement>("[data-canvas-layer]");
    const scaleStr = layer?.style.getPropertyValue("--canvas-scale") ?? "1";
    const scale = parseFloat(scaleStr) || 1;

    const startMouseX = e.clientX;
    const startMouseY = e.clientY;
    const startX = posRef.current.x;
    const startY = posRef.current.y;
    let curX = startX;
    let curY = startY;
    const el = e.currentTarget as HTMLElement;

    // While dragging, float above everything; the prop value is restored on drop.
    el.style.zIndex = "20000";
    const onMove = (me: globalThis.MouseEvent) => {
      curX = startX + (me.clientX - startMouseX) / scale;
      curY = startY + (me.clientY - startMouseY) / scale;
      el.style.transform = `translate(${curX}px, ${curY}px)`;
    };
    const onUp = () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
      el.style.zIndex = String(zIndex);
      onMoveEnd?.(id, curX, curY);
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  };

  // Status dot:
  //   琥珀  = 仮身（参照）— 閲覧してもセッション中も常に琥珀
  //   薄赤  = 仮身で窓の先を喪失（CASの写真を表示中／内容なし）(v1.3-⑩)
  //   緑   = 実身で、このセッションで新規導入または変更（次回起動でグレーに）
  //   グレー = 実身（閲覧のみ・変更なし）
  const status = committed
    ? active
      ? { color: "#3fae6a", border: "#2c4f3c", filled: true, title: "実身（このセッションで導入／変更）" }
      : { color: "#8a93a6", border: "#3a4254", filled: true, title: "実身（コミット済み・プロジェクトのもの）" }
    : lost
    ? {
        color: "#e08a8a",
        border: "#6b3a3a",
        filled: true,
        title:
          lost === "none"
            ? "仮身（内容が見つかりません・元ファイルもCASも喪失）"
            : "仮身（元ファイル不明・取り込み時点の写真を表示中）",
      }
    : { color: "#e8b84a", border: "#5e4a1e", filled: true, title: "仮身（参照）" };

  // Connection handle: begin a link drag without moving the card
  const handleLinkDown = (e: React.MouseEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    e.stopPropagation();
    e.preventDefault();
    onStartLink?.(id, e.clientX, e.clientY);
  };

  return (
    <div
      data-object-id={id}
      style={{
        position: "absolute",
        left: 0,
        top: 0,
        transform: `translate(${x}px, ${y}px)`,
        width: CARD_W,
        height: CARD_H,
        zIndex,
        background: isUrl
          ? "var(--card-bg-url, #172435)"
          : isProject
          ? "var(--card-bg-project, #241f33)"
          : isFolder
          ? "var(--card-bg-folder, #2a2418)"
          : "var(--card-bg, #111120)",
        border: `1px solid ${isUrl ? "#2f5a7a" : isProject ? "#5a4d7a" : isFolder ? "#7a6a3a" : status.border}`,
        outline: selected ? "2px solid var(--accent, #4a90d9)" : "none",
        outlineOffset: 1,
        borderRadius: 10,
        display: "flex",
        flexDirection: "column",
        overflow: "visible",
        color: "var(--card-text, #d0d8e8)",
        fontSize: 11,
        fontFamily: "sans-serif",
        boxShadow: "var(--card-shadow, 0 2px 16px #00000080)",
        cursor: "grab",
        willChange: "transform",
      }}
      onMouseDown={handleMouseDown}
      onDoubleClick={(e) => {
        e.stopPropagation();
        onOpen?.(id);
      }}
      onMouseEnter={() => setHover(true)}
      onMouseLeave={() => setHover(false)}
      title={`${mimeType}\nid: ${id}`}
    >
      {/* inner content clipped to rounded card */}
      <div style={{
        position: "absolute",
        inset: 0,
        borderRadius: 10,
        overflow: "hidden",
        display: "flex",
        flexDirection: "column",
      }}>
        {thumbnail ? (
          <>
            <img
              src={thumbnail}
              style={{
                width: "100%",
                flex: 1,
                objectFit: "cover",
                display: "block",
                pointerEvents: "none",
                minHeight: 0,
              }}
              draggable={false}
            />
            <div style={{
              padding: "4px 8px",
              overflow: "hidden",
              textOverflow: "ellipsis",
              whiteSpace: "nowrap",
              background: "var(--card-label-bg, #00000055)",
            }}>
              {label}
            </div>
          </>
        ) : (
          <div style={{
            flex: 1,
            display: "flex",
            flexDirection: "column",
            alignItems: "center",
            justifyContent: "center",
            gap: 6,
            padding: "8px 6px",
          }}>
            {osIcon ? (
              <img
                src={osIcon}
                width={32}
                height={32}
                style={{ display: "block", pointerEvents: "none" }}
                draggable={false}
              />
            ) : (
              <span style={{ fontSize: 26, lineHeight: 1 }}>{iconFor(mimeType, ext)}</span>
            )}
            <span style={{
              maxWidth: "100%",
              overflow: "hidden",
              textOverflow: "ellipsis",
              whiteSpace: "nowrap",
              textAlign: "center",
            }}>
              {label}
            </span>
            {isProject && (
              <span style={{ fontSize: 9, color: "#9a8cc4" }}>プロジェクト ▸</span>
            )}
            {isUrl && (
              <span style={{ fontSize: 9, color: "#7fb0d8" }}>リンク ▸</span>
            )}
            {isFolder && (
              <span style={{ fontSize: 9, color: "#d8c47f" }}>🔗 リンクフォルダ ▸</span>
            )}
            {memo && (
              <span
                style={{
                  fontSize: 10,
                  color: "#9aa6bb",
                  textAlign: "center",
                  maxWidth: "100%",
                  display: "-webkit-box",
                  WebkitLineClamp: 3,
                  WebkitBoxOrient: "vertical",
                  overflow: "hidden",
                }}
              >
                {memo}
              </span>
            )}
          </div>
        )}
      </div>

      {/* status dot (top-left): 琥珀=仮身 / 緑=実身(今セッション変更) / グレー=実身 */}
      {!isProject && !isUrl && !isFolder && (
      <div
        title={status.title}
        style={{
          position: "absolute",
          left: 7,
          top: 7,
          width: 9,
          height: 9,
          borderRadius: "50%",
          background: status.filled ? status.color : "transparent",
          border: `1.5px solid ${status.color}`,
          boxShadow: status.filled ? `0 0 5px ${status.color}aa` : "none",
          zIndex: 3,
          pointerEvents: "none",
        }}
      />
      )}

      {/* connection handle on right edge — appears on hover */}
      <div
        onMouseDown={handleLinkDown}
        title="ドラッグして他のオブジェクトへリンク"
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

      {/* expansion — anchored to this card, moves with it */}
      {expanded && isImage && (
        <ImagePreview
          objId={id}
          label={label}
          top={CARD_H + 8}
          onClose={() => onCollapse?.(id)}
        />
      )}
      {expanded && mimeType.startsWith("audio/") && (
        <AudioPlayer
          objId={id}
          label={label}
          top={CARD_H + 8}
          onClose={() => onCollapse?.(id)}
        />
      )}
      {expanded && (isVideo || isPdf) && (
        <MediaPreview
          objId={id}
          label={label}
          kind={isPdf ? "pdf" : "video"}
          top={CARD_H + 8}
          onClose={() => onCollapse?.(id)}
        />
      )}
      {expanded && !isImage && !mimeType.startsWith("audio/") && !isVideo && !isPdf && editPath && (
        <InlineTextEditor
          path={editPath}
          top={CARD_H + 8}
          width={panelW}
          onResizeEnd={(w) => onPanelResize?.(id, w)}
          onClose={() => onCollapse?.(id)}
          onSaved={() => onAfterSave?.()}
        />
      )}
    </div>
  );
}
