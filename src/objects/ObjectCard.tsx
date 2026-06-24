import { useRef, useState, useEffect, lazy, Suspense } from "react";
import { invoke } from "@tauri-apps/api/core";
import { InlineTextEditor } from "./InlineTextEditor";
import { ImagePreview } from "./ImagePreview";
import { AudioPlayer } from "./AudioPlayer";
import { MediaPreview } from "./MediaPreview";
// v1.3.11 #13: docx=mammoth / 表計算=SheetJS は重いので遅延読み込み（オフィスファイルを
// プレビューした時だけ取得＝メインバンドルを太らせない）。
const DocPreview = lazy(() =>
  import("./DocPreview").then((m) => ({ default: m.DocPreview }))
);
import { isTextMime } from "../model";

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
   *  "cas" = showing the import-time CAS snapshot; "none" = nothing found.
   *  v1.3.9 問題点2: "real" = 実身カードだが実体ファイルが失われている（再リンク待ち）。 */
  lost?: "cas" | "none" | "real" | null;
  /** this card references another TexoS project (navigable) */
  isProject?: boolean;
  /** this card is a web URL link */
  isUrl?: boolean;
  /** this card is a link to an OS folder */
  isFolder?: boolean;
  /** v1.4 オートフロー: this card is a filter (pipeline processor) */
  isFilter?: boolean;
  /** filter_ref JSON — Type（フィルタ/コマンド/ジェネレータ）でバッジと配色を変える */
  filterRef?: string | null;
  /** v1.4 オートフロー: this card is a drop point (pipeline data inlet) */
  isDrop?: boolean;
  /** v1.3.11: this card is a launcher (registered program / terminal) */
  isLauncher?: boolean;
  /** v1.3.11: launcher が起動するプログラムの実パス（アイコン表示用。ターミナルは null） */
  launcherPath?: string | null;
  /** stacking order (computed by the parent; expanded cards come to front) */
  zIndex?: number;
  /** part of the current rubber-band selection */
  selected?: boolean;
  /** Ctrl+click: toggle this card in/out of the selection (v1.3 メモ２追加). */
  onToggleSelect?: (id: string) => void;
  /** 素クリック（未選択時）: このカードだけを選択（v1.3.8 マウスモデル刷新）。 */
  onSelect?: (id: string) => void;
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
  /** v1.3.9: 読取専用テキスト閲覧から「編集」押下 → 実身化して編集（commit→edit）。 */
  onRequestEdit?: (id: string) => void;
  /** v1.3.9: フィルタ/コマンド/ジェネレータの ▶実行 ボタン（ダブルクリックは設定に変更）。 */
  onRunFlow?: (id: string) => void;
  onMoveEnd?: (id: string, x: number, y: number) => void;
  /** Begin dragging a link from this card. (fromId, clientX, clientY) */
  onStartLink?: (fromId: string, clientX: number, clientY: number) => void;
  /** Double-click to open/edit this object. */
  onOpen?: (id: string) => void;
  /** v1.3.10: ラベルのダブルクリックでリネーム（実身=実ファイル改名／仮身=表示名）。 */
  onRenameTitle?: (id: string) => void;
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
  if (mime === "application/vnd.texos.filter") return "⚗️";
  if (mime === "application/vnd.texos.drop") return "📥";
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

// v1.5: フィルタカードの Type 別ビジュアル（一目で見分けるビジュアル系TexoS）。
// 内蔵フィルタ・旧形式はフィルタ扱い（緑）。
const FILTER_TYPE_STYLES = {
  // v1.3.9: カードは「名前／種類／実行ボタン」の3行で揃える。操作説明（ダブルクリック＝設定）
  // は常時表示せず、カードのホバー（title）と右クリックメニューに逃がす。
  filter: {
    bg: "var(--card-bg-filter, #14281e)",
    border: "#3a7a5a",
    text: "#7fd8b0",
    badge: "⚙ フィルタ",
    run: "▶ 実行",
  },
  command: {
    bg: "var(--card-bg-filter-cmd, #142828)",
    border: "#3a7a7a",
    text: "#7fd8d8",
    badge: "⌨ コマンド",
    run: "▶ 実行",
  },
  generator: {
    bg: "var(--card-bg-filter-gen, #2a2114)",
    border: "#7a5d3a",
    text: "#e8c07f",
    badge: "✨ ジェネレータ",
    run: "▶ 生成",
  },
} as const;

function filterTypeOf(ref: string | null | undefined): keyof typeof FILTER_TYPE_STYLES {
  if (!ref) return "filter";
  try {
    const s = JSON.parse(ref);
    if (!s.builtin && (s.type === "command" || s.type === "generator")) return s.type;
  } catch {
    // 壊れたJSONはフィルタ扱いで表示だけ続行（編集ダイアログ側で直せる）
  }
  return "filter";
}

export function ObjectCard({
  id, realId, label, mimeType, x, y,
  committed = false, active = false, lost = null,
  isProject = false, isUrl = false, isFolder = false, isFilter = false, isDrop = false,
  isLauncher = false, launcherPath = null,
  filterRef = null,
  ext = null, memo = null, expanded = false, editPath = null,
  panelW = null, onPanelResize,
  zIndex = 0, selected = false, onToggleSelect, onSelect, onGroupDragStart,
  onMoveEnd, onStartLink, onOpen, onRenameTitle, onCollapse, onAfterSave, onRequestEdit, onRunFlow,
}: Props) {
  const fstyle = FILTER_TYPE_STYLES[filterTypeOf(filterRef)];
  const posRef = useRef({ x, y });
  posRef.current = { x, y };
  const [thumbnail, setThumbnail] = useState<string | null>(null);
  const [osIcon, setOsIcon] = useState<string | null>(null);
  const [hover, setHover] = useState(false);
  const isImage = mimeType.startsWith("image/");
  const isVideo = mimeType.startsWith("video/");
  const isPdf = mimeType === "application/pdf" || (ext ?? "").toLowerCase() === "pdf";
  // v1.3.9: text cards render an inline panel — read-only 閲覧 when uncommitted,
  // editable when an editable real file (editPath) exists.
  const isText = isTextMime(mimeType, ext ?? null);
  // v1.3.11 #13: オフィス系のインラインプレビュー（docx=mammoth / 表計算=SheetJS）。
  // csv/tsv は SheetJS の表として見せたいのでテキスト編集より優先する。
  const lowExt = (ext ?? "").toLowerCase();
  // HTML化(mammoth/SheetJS) + LibreOffice→PDF(pptx等) のプレビュー対象。
  const isDoc = [
    "docx", "xlsx", "xls", "ods", "csv", "tsv",
    "pptx", "ppt", "doc", "odt", "odp",
  ].includes(lowExt);

  useEffect(() => {
    let alive = true;
    // v1.3.11: ランチャーは登録プログラムの実パスからアイコンを取得（exe/lnk 個別アイコン）。
    if (isLauncher) {
      if (launcherPath) {
        fetchOsIcon("exe", launcherPath).then((d) => { if (alive) setOsIcon(d); });
      }
      return () => { alive = false; };
    }
    if (isProject || isUrl || !ext) return;
    fetchOsIcon(ext, editPath).then((d) => { if (alive) setOsIcon(d); });
    return () => { alive = false; };
  }, [ext, editPath, isProject, isUrl, isLauncher, launcherPath]);

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

    // v1.3.8 マウスモデル: Shift = この1枚だけ移動（複数選択中でも単体）。
    const moveSingleOnly = e.shiftKey;

    // 素クリック（未選択）= このカードだけを選択（他は解除）。続けてドラッグすれば単体移動。
    if (!moveSingleOnly && !selected) {
      onSelect?.(id);
    }

    // 複数選択の1枚を素クリック/ドラッグ → 群移動（親がドラッグ or クリックで1枚に絞る）。
    if (!moveSingleOnly && selected && onGroupDragStart) {
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
      document.body.style.cursor = "grabbing"; // 手のひらはドラッグ開始後（G5）
      curX = startX + (me.clientX - startMouseX) / scale;
      curY = startY + (me.clientY - startMouseY) / scale;
      el.style.transform = `translate(${curX}px, ${curY}px)`;
    };
    const onUp = () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
      document.body.style.cursor = "";
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
  const status = lost === "real"
    ? {
        // v1.3.9 問題点2: 実身だが実体ファイルが喪失。再リンク／CAS復元を促す薄赤。
        // v1.3.11 #12: リンクフォルダは参照先フォルダのリンク切れを薄赤で示す。
        color: "#e08a8a",
        border: "#6b3a3a",
        filled: true,
        title: isFolder
          ? "リンクフォルダ（参照先フォルダが見つかりません・移動/削除/リネーム？）"
          : "実身（実体ファイルが見つかりません・リネーム/移動/削除？ 右クリックで再リンクかCAS復元）",
      }
    : committed
    ? active
      ? { color: "#3fae6a", border: "#2c4f3c", filled: true, title: "実身（このセッションで導入／変更）" }
      : { color: "#8a93a6", border: "#3a4254", filled: true, title: "実身（プロジェクトのもの）" }
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

  // v1.3.10: タイトル(ラベル)のダブルクリックでリネーム。普通のファイルカードのみ
  // （フィルタ/ドロップ/プロジェクト/URL/フォルダは別アクション）。カードのダブルクリック
  // ＝プレビューと衝突しないよう stopPropagation。
  const titleRenamable =
    !!onRenameTitle && !isFilter && !isDrop && !isProject && !isUrl && !isFolder && !isLauncher;
  // v1.3.10 補完: フィルタ/ドロップは「アイコン/名前/種類/▶実行」の4行で固定サイズ(115px)が
  // いっぱい。実行ログのメモは5行目になり下端からはみ出てクリップされるので、カード面からは
  // 外してホバー(title)に移し、余白を詰めて4行を確実に収める。
  const compactFilter = isFilter || isDrop || isLauncher;
  const onTitleDblClick = titleRenamable
    ? (e: React.MouseEvent) => {
        e.stopPropagation();
        onRenameTitle!(id);
      }
    : undefined;

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
          : isFilter
          ? fstyle.bg
          : isDrop
          ? "var(--card-bg-drop, #102030)"
          : isLauncher
          ? "var(--card-bg-launcher, #14241c)"
          : "var(--card-bg, #111120)",
        border: `1px ${isDrop ? "dashed" : "solid"} ${isUrl ? "#2f5a7a" : isProject ? "#5a4d7a" : isFolder ? "#7a6a3a" : isFilter ? fstyle.border : isDrop ? "#4a90d9" : isLauncher ? "#2f6a4a" : status.border}`,
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
        cursor: "default", // ホバーは矢印のまま。手のひらはドラッグ中だけ（G5）
        willChange: "transform",
      }}
      onMouseDown={handleMouseDown}
      onDoubleClick={(e) => {
        e.stopPropagation();
        onOpen?.(id);
      }}
      onMouseEnter={() => setHover(true)}
      onMouseLeave={() => setHover(false)}
      title={
        isFilter
          ? `${fstyle.badge}\nダブルクリック: 設定を開く ／ ボタン: ${fstyle.run}\n右クリック: メニュー${memo ? `\n\n${memo}` : ""}`
          : isDrop
          ? `ここにファイルをドロップ→下流を自動実行\nダブルクリック: 投入するファイルを選択${memo ? `\n\n${memo}` : ""}`
          : `${mimeType}\nid: ${id}`
      }
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
            <div
              onDoubleClick={onTitleDblClick}
              title={titleRenamable ? "ダブルクリックで表示名を変更" : undefined}
              style={{
                padding: "4px 8px",
                overflow: "hidden",
                textOverflow: "ellipsis",
                whiteSpace: "nowrap",
                background: "var(--card-label-bg, #00000055)",
                cursor: titleRenamable ? "text" : undefined,
              }}
            >
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
            gap: compactFilter ? 3 : 6,
            padding: compactFilter ? "5px 6px" : "8px 6px",
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
            <span
              onDoubleClick={onTitleDblClick}
              title={titleRenamable ? "ダブルクリックで表示名を変更" : undefined}
              style={{
                maxWidth: "100%",
                overflow: "hidden",
                textOverflow: "ellipsis",
                whiteSpace: "nowrap",
                textAlign: "center",
                cursor: titleRenamable ? "text" : undefined,
              }}
            >
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
            {isFilter && (
              <span style={{ fontSize: 9, color: fstyle.text }}>{fstyle.badge}</span>
            )}
            {/* v1.3.9: 実行は明示ボタンで（ダブルクリックは設定）。誤爆を防ぐ。 */}
            {isFilter && (
              <button
                onMouseDown={(e) => e.stopPropagation()}
                onDoubleClick={(e) => e.stopPropagation()}
                onClick={(e) => {
                  e.stopPropagation();
                  onRunFlow?.(id);
                }}
                title="このフローを実行します"
                style={{
                  marginTop: 2,
                  padding: "2px 12px",
                  fontSize: 11,
                  fontWeight: 600,
                  color: "#fff",
                  background: fstyle.border,
                  border: "none",
                  borderRadius: 5,
                  cursor: "pointer",
                  fontFamily: "sans-serif",
                }}
              >
                {fstyle.run}
              </button>
            )}
            {isDrop && (
              <span style={{ fontSize: 9, color: "#7fb0d8" }}>📥 ここにドロップ→自動実行</span>
            )}
            {/* v1.3.11 ランチャー: 登録プログラムを起動。接続ファイルがあれば開く。 */}
            {isLauncher && (
              <span style={{ fontSize: 9, color: "#7fb0d8" }}>🚀 ランチャー</span>
            )}
            {isLauncher && (
              <button
                onMouseDown={(e) => e.stopPropagation()}
                onDoubleClick={(e) => e.stopPropagation()}
                onClick={(e) => {
                  e.stopPropagation();
                  onRunFlow?.(id);
                }}
                title="このプログラムを起動します（接続したファイルがあれば開きます）"
                style={{
                  marginTop: 2,
                  padding: "2px 12px",
                  fontSize: 11,
                  fontWeight: 600,
                  color: "#fff",
                  background: "#2f6a4a",
                  border: "none",
                  borderRadius: 5,
                  cursor: "pointer",
                  fontFamily: "sans-serif",
                }}
              >
                ▶ 起動
              </button>
            )}
            {/* v1.3.10 補完: フィルタ/ドロップはメモ(実行ログ)を面に出さずホバーへ（4行に収める）。 */}
            {memo && !compactFilter && (
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
      {/* v1.3.11 #12: 通常カードは常時。リンクフォルダ等は普段ドットを出さないが、
          リンク切れ（lost==="real"）のときだけ赤シグナルを表示する。 */}
      {((!isProject && !isUrl && !isFolder && !isFilter && !isLauncher) || lost === "real") && (
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
      {/* v1.3.11 #13: オフィス系（docx/表計算）の読取専用インラインプレビュー（遅延読込）。 */}
      {expanded && isDoc && (
        <Suspense fallback={null}>
          <DocPreview
            objId={id}
            label={label}
            ext={lowExt}
            top={CARD_H + 8}
            onClose={() => onCollapse?.(id)}
          />
        </Suspense>
      )}
      {/* v1.3.9: text panel. editPath present → editable (実身); otherwise
          read-only 閲覧 of the LIVE content, with an 「編集」 button that
          commits→edits via onRequestEdit. */}
      {expanded && !isImage && !mimeType.startsWith("audio/") && !isVideo && !isPdf && !isDoc &&
        (editPath || isText) && (
        <InlineTextEditor
          path={editPath}
          objId={id}
          readOnly={!editPath}
          onRequestEdit={() => onRequestEdit?.(id)}
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
