import { useRef, useState, useEffect, lazy, Suspense } from "react";
import { invoke } from "@tauri-apps/api/core";
import { InlineTextEditor } from "./InlineTextEditor";
import { ChatPanel } from "./ChatPanel";
import { T2iPanel } from "./T2iPanel";
import { ImagePreview } from "./ImagePreview";
import { AudioPlayer } from "./AudioPlayer";
import { MediaPreview } from "./MediaPreview";
// v1.3.11 #13: docx=mammoth / 表計算=SheetJS は重いので遅延読み込み（オフィスファイルを
// プレビューした時だけ取得＝メインバンドルを太らせない）。
const DocPreview = lazy(() =>
  import("./DocPreview").then((m) => ({ default: m.DocPreview }))
);
// v2.0 t23d Phase 2: three.js は重いので遅延読み込み（3Dプレビューを開いた時だけ取得）。
const GlbPreview = lazy(() =>
  import("./GlbPreview").then((m) => ({ default: m.GlbPreview }))
);
import { isTextMime, type RunAlert } from "../model";
import { VoiceStatus } from "./VoiceStatus";

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
  /** v1.7 P2 更新マーク: URLカードの「更新を確認」で内容の変化が検出された（強制反映なし） */
  webUpdate?: boolean;
  /** v1.8.7 カードの色でのお知らせ（前回の実行で失敗／後段で失敗）。null = 何もなし。 */
  runAlert?: RunAlert | null;
  /** v1.8.7 エラー印のクリックで実行ログを開く。 */
  onShowTrace?: (id: string, label: string) => void;
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
  /** v1.4.12: Ctrl+左ドラッグでこのカードの仮身（化身）をドロップ位置に作る。(id, clientX, clientY) */
  onCtrlClone?: (id: string, clientX: number, clientY: number) => void;
  /** v1.4.12: Alt+クリックで関係矢印の連結成分（つながり）をまるごと選択。 */
  onSelectConnected?: (id: string, additive?: boolean) => void;
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
  /** width of the text editor panel (v1.3.1, user-resizable)。v1.5.16 から各プレビューの幅も共用 */
  panelW?: number | null;
  /** height of resizable previews (v1.5.16, PDF/Doc のみ。画像・動画はアスペクト追従) */
  panelH?: number | null;
  onPanelResize?: (id: string, width: number, height?: number) => void;
  onCollapse?: (id: string) => void;
  onAfterSave?: () => void;
  /** v1.3.9: 読取専用テキスト閲覧から「編集」押下 → 実身化して編集（commit→edit）。 */
  onRequestEdit?: (id: string) => void;
  /** v1.3.9: フィルタ/コマンド/ジェネレータの ▶実行 ボタン（ダブルクリックは設定に変更）。 */
  onRunFlow?: (id: string) => void;
  /** v1.5.5: タイマ/ウォッチャの稼働/停止トグル（構成中の誤発火防止スイッチ）。 */
  onToggleOpPause?: (id: string) => void;
  onMoveEnd?: (id: string, x: number, y: number) => void;
  /** v1.4.17: 単体ドラッグ中の生位置（矢印の同時追従用。永続化はしない）。 */
  onDragLive?: (id: string, x: number, y: number) => void;
  /** Begin dragging a link from this card. (fromId, clientX, clientY) */
  onStartLink?: (fromId: string, clientX: number, clientY: number) => void;
  /** v1.8.1: 左端のつまみから OS へファイルを持ち出す（ネイティブ OLE ドラッグ）。 */
  onDragOut?: (id: string) => void;
  /** Double-click to open/edit this object. */
  onOpen?: (id: string) => void;
  /** v1.3.10: ラベルのダブルクリックでリネーム（実身=実ファイル改名／仮身=表示名）。 */
  onRenameTitle?: (id: string) => void;
  /** v1.4.29: カードの説明ツールチップ（ホバー時のmime/メモ/使い方・状態）を出すか。既定オン。 */
  showHints?: boolean;
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
  if (mime === "application/vnd.texos.op") return "⚡";
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
    text: "var(--card-filter-text, #7fd8b0)",
    badge: "⚙ フィルタ",
    run: "▶ 実行",
  },
  command: {
    bg: "var(--card-bg-filter-cmd, #142828)",
    border: "#3a7a7a",
    text: "var(--card-filter-cmd-text, #7fd8d8)",
    badge: "⌨ コマンド",
    run: "▶ 実行",
  },
  generator: {
    bg: "var(--card-bg-filter-gen, #2a2114)",
    border: "#7a5d3a",
    text: "var(--card-filter-gen-text, #e8c07f)",
    badge: "✨ ジェネレータ",
    run: "▶ 生成",
  },
  // v1.12 ④ コンストラクタ: 処理系の仲間（緑）だが、押すと「カードを並べる」ので名前とボタンで見分ける。
  construct: {
    bg: "var(--card-bg-filter, #14281e)",
    border: "#3a7a5a",
    text: "var(--card-filter-text, #7fd8b0)",
    badge: "🏗 コンストラクタ",
    run: "▶ 構築",
  },
  // シェルカード統合（2026-07-06）: cmd/PowerShell カード。従来は「⚙ フィルタ」表示で
  // 見分けがつかなかった。配色はコマンド系を流用（テーマ変数の追加を避ける）。
  shell: {
    bg: "var(--card-bg-filter-cmd, #142828)",
    border: "#3a7a7a",
    text: "var(--card-filter-cmd-text, #7fd8d8)",
    badge: "＞_ シェル",
    run: "▶ 実行",
  },
  // v1.5.4 WS-P2 オペレーション（kind='op'）: 琥珀系の亜種（ジェネレータより赤みを足して
  // 「発行する側」を見分ける）。▶ボタンはトリガのみ（run が空の種別はボタンなし）。
  op_trigger: {
    bg: "var(--card-bg-op, #2c1e10)",
    border: "#8a6230",
    text: "var(--card-op-text, #f0c070)",
    badge: "⚡ トリガ",
    run: "▶ 発行",
  },
  op_timer: {
    bg: "var(--card-bg-op, #2c1e10)",
    border: "#8a6230",
    text: "var(--card-op-text, #f0c070)",
    badge: "⚡ タイマ",
    run: "",
  },
  op_watch: {
    bg: "var(--card-bg-op, #2c1e10)",
    border: "#8a6230",
    text: "var(--card-op-text, #f0c070)",
    badge: "⚡ ウォッチャ",
    run: "",
  },
  // v1.10 音声反射カード: 稼働トグルのみ・▶なし。v1.12 で「声の窓口」（常駐して人と話すカード）に
  // なったので、発行系の琥珀から外して紫（どの種別も使っていない色）にする。
  op_voice: {
    bg: "var(--card-bg-live, #221a30)",
    border: "#7a5ab0",
    text: "var(--card-live-text, #c8a8f0)",
    badge: "🎙 音声",
    run: "",
  },
  // v1.5.6 WS-M M1（§9）: IN/OUT バーチャルカード＝データレーンの口（発行系ではないので
  // 琥珀ではなく青系＝ドロップポイントの親戚として見せる）。OUT だけ ▶（さかのぼり実行）。
  op_in: {
    bg: "var(--card-bg-drop, #102030)",
    border: "#4a90d9",
    text: "var(--card-drop-text, #7fb0d8)",
    badge: "⊳ IN（入口）",
    run: "",
  },
  op_out: {
    bg: "var(--card-bg-drop, #102030)",
    border: "#3a7a5a",
    text: "var(--card-filter-text, #7fd8b0)",
    badge: "⊲ OUT（出口）",
    run: "▶ 実行",
  },
} as const;

function filterTypeOf(ref: string | null | undefined): keyof typeof FILTER_TYPE_STYLES {
  if (!ref) return "filter";
  try {
    const s = JSON.parse(ref);
    // v1.5.4 WS-P2: op カード（トリガ/タイマ/ウォッチャ）は op キーで見分ける。
    // v1.5.6 WS-M M1: IN/OUT バーチャルカードも同じ器（op:"in"/"out"）。
    if (["trigger", "timer", "watch", "voice", "in", "out"].includes(s.op)) {
      return `op_${s.op}` as keyof typeof FILTER_TYPE_STYLES;
    }
    if (typeof s.shell === "string") return "shell";
    if (s.builtin === "construct") return "construct";
    if (!s.builtin && (s.type === "command" || s.type === "generator")) return s.type;
  } catch {
    // 壊れたJSONはフィルタ扱いで表示だけ続行（編集ダイアログ側で直せる）
  }
  return "filter";
}

// v1.8.7 カードの色でのお知らせ。
const ALERT_COLOR: Record<"error" | "warn", string> = { error: "#d9534f", warn: "#e0a040" };

// v1.8.7: 途中出力スイッチ（emit_mid）が入っているか（面に小さな印を出す）。
function emitMidOf(ref: string | null | undefined): boolean {
  try {
    return JSON.parse(ref ?? "{}").emit_mid === "on";
  } catch {
    return false;
  }
}

// v1.5.5: op カード（タイマ/ウォッチャ）の一時停止状態。キー無し=稼働（後方互換）。
function opPausedOf(ref: string | null | undefined): boolean {
  try {
    return JSON.parse(ref ?? "{}").paused === true;
  } catch {
    return false;
  }
}

// v1.10: 音声カードの聞き取りしきい値（レベルバーの目盛り。既定は voice.rs と同じ 0.012）。
function voiceThresholdOf(ref: string | null | undefined): number {
  try {
    const t = Number(JSON.parse(ref ?? "{}").threshold);
    return Number.isFinite(t) && t > 0 ? t : 0.012;
  } catch {
    return 0.012;
  }
}

// v1.4.1: AIフィルタの動作モード判定。"chat"=対話パネル / "transform"=一発変換。
function aiModeOf(ref: string | null | undefined): "chat" | "transform" | null {
  if (!ref) return null;
  try {
    const s = JSON.parse(ref);
    if (s.builtin === "ai") return s.mode === "chat" ? "chat" : "transform";
  } catch {
    /* 壊れたJSONは無視 */
  }
  return null;
}

// v1.4.17: t2i（画像生成）カード判定。ダブルクリック＝インライン生成パネル（AIチャットと同じ流儀）。
function isT2iOf(ref: string | null | undefined): boolean {
  if (!ref) return false;
  try {
    return JSON.parse(ref).builtin === "t2i";
  } catch {
    return false;
  }
}

// v1.4.6: AIカードの provider/endpoint（ChatPanel の Ollama セットアップ案内に渡す）。
function aiConnOf(ref: string | null | undefined): { provider?: string; endpoint?: string } {
  if (!ref) return {};
  try {
    const s = JSON.parse(ref);
    return { provider: s.provider, endpoint: s.endpoint };
  } catch {
    return {};
  }
}

export function ObjectCard({
  id, realId, label, mimeType, x, y,
  committed = false, active = false, lost = null,
  isProject = false, isUrl = false, webUpdate = false, isFolder = false, isFilter = false, isDrop = false,
  isLauncher = false, launcherPath = null,
  filterRef = null,
  ext = null, memo = null, expanded = false, editPath = null,
  panelW = null, panelH = null, onPanelResize,
  zIndex = 0, selected = false, onToggleSelect, onCtrlClone, onSelectConnected, onSelect, onGroupDragStart,
  onMoveEnd, onDragLive, onStartLink, onDragOut, onOpen, onRenameTitle, onCollapse, onAfterSave, onRequestEdit, onRunFlow,
  onToggleOpPause,
  runAlert = null,
  onShowTrace,
  showHints = true,
}: Props) {
  const ftype = filterTypeOf(filterRef);
  const fstyle = FILTER_TYPE_STYLES[ftype];
  // v1.5.5: タイマ/ウォッチャの稼働状態（面のトグルボタン）。トリガは手動▶のみなので対象外。
  const isOpSched = ftype === "op_timer" || ftype === "op_watch" || ftype === "op_voice";
  const opPaused = isOpSched && opPausedOf(filterRef);
  const isAiChat = aiModeOf(filterRef) === "chat"; // v1.4.1 対話パネルを持つAIカード
  const isT2i = isT2iOf(filterRef); // v1.4.17 インライン生成パネルを持つ画像生成カード
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
  // v2.0 t23d Phase 2: glb/gltf は three.js のインライン 3D ビューア（汎用・出所不問）。
  const isGlb = ["glb", "gltf"].includes(lowExt) || mimeType.startsWith("model/gltf");

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

  // v2.0 t23d Phase 2: thumbNonce = 3Dプレビューの「サムネイル更新」後にカード面を再取得する。
  const [thumbNonce, setThumbNonce] = useState(0);
  useEffect(() => {
    if (!isImage && !isGlb) return;
    invoke<string>("get_thumbnail", { id })
      .then(setThumbnail)
      .catch((e) => {
        // glb はサムネイル未作成が普通の状態（アイコン表示のまま）＝エラーにしない。
        if (isImage) console.error("get_thumbnail failed", id, e);
        setThumbnail(null);
      });
  }, [id, realId, isImage, isGlb, thumbNonce]);

  const handleMouseDown = (e: React.MouseEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    e.stopPropagation();
    e.preventDefault();

    // v1.4.12: Alt+クリック = 関係矢印でつながった連結成分（つながり）をまるごと選択。
    // v1.5.11 UIの気づき３-2②: Ctrl+Alt = 他の選択を解除せず、つながりを追加選択。
    if (e.altKey) {
      onSelectConnected?.(id, e.ctrlKey || e.metaKey);
      return;
    }

    // Ctrl+クリック = 選択に個別追加／解除。Ctrl+左ドラッグ = この場に仮身（化身）を作る。
    // クリックとドラッグは移動量で区別する（4px 超で化身、未満で選択トグル）。
    if (e.ctrlKey || e.metaKey) {
      const sx = e.clientX;
      const sy = e.clientY;
      // v1.4.29 ①: ドラッグ中は半透明のカードゴーストをポインタに追従表示する（「この場に
      // コピーしている」実感）。実カードを cloneNode して見た目そのままのゴーストにし、掴んだ
      // 相対位置（grab）とキャンバス倍率（scale）を保って画面上のサイズ・位置を一致させる。
      const cardEl = e.currentTarget as HTMLElement;
      const rect = cardEl.getBoundingClientRect();
      const grabX = sx - rect.left;
      const grabY = sy - rect.top;
      const layer = cardEl.closest<HTMLElement>("[data-canvas-layer]");
      const scale = parseFloat(layer?.style.getPropertyValue("--canvas-scale") || "1") || 1;
      let dragged = false;
      let ghost: HTMLElement | null = null;
      const moveGhost = (mx: number, my: number) => {
        if (ghost) ghost.style.transform = `translate(${mx - grabX}px, ${my - grabY}px) scale(${scale})`;
      };
      const removeGhost = () => {
        ghost?.remove();
        ghost = null;
      };
      const onCtrlMove = (me: globalThis.MouseEvent) => {
        if (!dragged && Math.hypot(me.clientX - sx, me.clientY - sy) > 4) {
          dragged = true;
          document.body.style.cursor = "copy";
          const g = cardEl.cloneNode(true) as HTMLElement;
          g.style.position = "fixed";
          g.style.left = "0";
          g.style.top = "0";
          g.style.margin = "0";
          g.style.transformOrigin = "0 0";
          g.style.opacity = "0.55";
          g.style.pointerEvents = "none";
          g.style.transition = "none";
          g.style.zIndex = "2147483647";
          document.body.appendChild(g);
          ghost = g;
          moveGhost(me.clientX, me.clientY);
        } else if (dragged) {
          moveGhost(me.clientX, me.clientY);
        }
      };
      const onCtrlUp = (me: globalThis.MouseEvent) => {
        window.removeEventListener("mousemove", onCtrlMove);
        window.removeEventListener("mouseup", onCtrlUp);
        document.body.style.cursor = "";
        removeGhost();
        if (dragged && onCtrlClone) onCtrlClone(id, me.clientX, me.clientY);
        else onToggleSelect?.(id);
      };
      window.addEventListener("mousemove", onCtrlMove);
      window.addEventListener("mouseup", onCtrlUp);
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
      onDragLive?.(id, curX, curY); // 矢印も同時に追従（v1.4.17）
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
          : isProject
          ? "プロジェクト（参照先プロジェクトが見つかりません・移動/削除/リネーム？ 右クリック「リンクし直す...」で繋ぎ直し）"
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

  // v1.8.1 ドラッグアウト: 左端のつまみを掴んで OS 側（エクスプローラ・DAW・他アプリ）へ
  // 実ファイルを手渡す。素ドラッグ＝カード移動 を壊さないための専用つまみで、Alt は
  // 既に「つながり選択」に取られているのでモディファイアには乗せない。
  // 4px 動いてから始めるのは Ctrl+ドラッグ（化身）と同じ流儀＝つまみを素クリックしただけで
  // 空のドラッグが走らないようにするため。開始後は OS のモーダルループがマウスを奪うので
  // mousemove/mouseup はこちらへ返ってこない（先に外しておく）。
  const handleExportDown = (e: React.MouseEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    e.stopPropagation();
    e.preventDefault();
    const sx = e.clientX;
    const sy = e.clientY;
    function onMove(me: globalThis.MouseEvent) {
      if (Math.hypot(me.clientX - sx, me.clientY - sy) <= 4) return;
      finish();
      onDragOut?.(id);
    }
    function finish() {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", finish);
    }
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", finish);
  };

  // 持ち出せる＝実体（ファイルかフォルダ）を持つカード。フィルタ／ドロップポイント／
  // ランチャは TexoS 内の道具、URL は実体が無いので、つまみ自体を出さない
  //（バックエンドの持ち出し規則＝resolve_export_paths と同じ線引き）。
  const exportable = !isFilter && !isDrop && !isLauncher && !isUrl;

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
        // v1.8.7: 前回の実行で失敗したカードは枠を赤（エラー）／琥珀（後段で失敗）に。
        border: runAlert
          ? `2px solid ${ALERT_COLOR[runAlert.level]}`
          : `1px ${isDrop ? "dashed" : "solid"} ${isUrl ? "#2f5a7a" : isProject ? "#5a4d7a" : isFolder ? "#7a6a3a" : isFilter ? fstyle.border : isDrop ? "#4a90d9" : isLauncher ? "#2f6a4a" : status.border}`,
        outline: selected ? "2px solid var(--accent, #4a90d9)" : "none",
        outlineOffset: 1,
        borderRadius: 10,
        display: "flex",
        flexDirection: "column",
        overflow: "visible",
        color: "var(--card-text, #d0d8e8)",
        fontSize: 11,
        fontFamily: "sans-serif",
        boxShadow: runAlert
          ? `0 0 14px ${ALERT_COLOR[runAlert.level]}88, var(--card-shadow, 0 2px 16px #00000080)`
          : "var(--card-shadow, 0 2px 16px #00000080)",
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
        // v1.4.29: 「カードのツールチップを表示」がオフのときは説明を出さない。
        !showHints
          ? undefined
          : isAiChat
          ? `🤖 AI対話\nダブルクリック / 💬チャット: パネルを開く\n右クリック → 設定: モデル・システム変更${memo ? `\n\n${memo}` : ""}`
          : isT2i
          ? `🎨 画像生成\nダブルクリック: 生成パネル（プロンプト編集＋実行） ／ ボタン: フローを実行\n右クリック → 設定: モデル・パラメータ変更${memo ? `\n\n${memo}` : ""}`
          : isFilter
          ? `${fstyle.badge}\nダブルクリック: 設定を開く${fstyle.run ? ` ／ ボタン: ${fstyle.run}` : ""}\n右クリック: メニュー${memo ? `\n\n${memo}` : ""}`
          : isDrop
          ? `ここにファイルをドロップ→下流を自動実行\nダブルクリック: 投入するファイルを選択${memo ? `\n\n${memo}` : ""}`
          : `${mimeType}\nid: ${id}${memo ? `\n\n${memo}` : ""}`
      }
    >
      {/* v1.8.7 カードの色でのお知らせ: 前回の実行で失敗したカードの上辺に印。ホバーで理由、
          クリックで実行ログ。次の実行で上書きされる（直して再実行すれば消える）。 */}
      {runAlert && (
        <div
          onMouseDown={(e) => e.stopPropagation()}
          onDoubleClick={(e) => e.stopPropagation()}
          onClick={(e) => {
            e.stopPropagation();
            onShowTrace?.(id, label);
          }}
          title={`${runAlert.level === "error" ? "前回の実行でエラー" : "前回の実行で注意"}\n${runAlert.message}\n\nクリックで実行ログを開く`}
          style={{
            position: "absolute",
            top: -11,
            left: "50%",
            transform: "translateX(-50%)",
            zIndex: 3,
            maxWidth: "92%",
            padding: "1px 8px",
            borderRadius: 9,
            background: runAlert.level === "error" ? "#5a1f1f" : "#5a4318",
            border: `1px solid ${ALERT_COLOR[runAlert.level]}`,
            color: runAlert.level === "error" ? "#ffb3b0" : "#ffd89a",
            fontSize: 9.5,
            whiteSpace: "nowrap",
            overflow: "hidden",
            textOverflow: "ellipsis",
            cursor: "pointer",
          }}
        >
          {runAlert.level === "error" ? "✗ " : "⚠ "}
          {runAlert.message.split("\n")[0]}
        </div>
      )}
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
              // v1.7 P2 更新マーク: 変化を検出したら控えめに知らせる（強制反映なし・
              // 実身にするかはユーザーが右クリック「再取得」で決める）。
              webUpdate ? (
                <span style={{ fontSize: 9, color: "#e08a8a" }}>リンク ▸ 更新あり</span>
              ) : (
                <span style={{ fontSize: 9, color: "#7fb0d8" }}>リンク ▸</span>
              )
            )}
            {isFolder && (
              <span style={{ fontSize: 9, color: "#d8c47f" }}>🔗 リンクフォルダ ▸</span>
            )}
            {isFilter && (
              <span style={{ fontSize: 9, color: fstyle.text }}>
                {isAiChat ? "🤖 AI対話" : fstyle.badge}
                {emitMidOf(filterRef) && (
                  <span title="途中の出力もキャンバスに出す（右クリックで切替）" style={{ marginLeft: 4, color: "#e0c068" }}>
                    ⤓出力
                  </span>
                )}
              </span>
            )}
            {/* v1.3.9: 実行は明示ボタンで（ダブルクリックは設定）。誤爆を防ぐ。
                v1.4.1: AI対話カードは「実行」ではなくチャットパネルを開く。
                v1.5.4: op カードの▶はトリガのみ（run が空のタイマ/ウォッチャはボタンなし）。 */}
            {isFilter && (isAiChat || fstyle.run) && (
              <button
                onMouseDown={(e) => e.stopPropagation()}
                onDoubleClick={(e) => e.stopPropagation()}
                onClick={(e) => {
                  e.stopPropagation();
                  if (isAiChat) onOpen?.(id);
                  else onRunFlow?.(id);
                }}
                title={isAiChat ? "チャットパネルを開きます" : "このフローを実行します"}
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
                {isAiChat ? "💬 チャット" : fstyle.run}
              </button>
            )}
            {/* v1.5.5: タイマ/ウォッチャの稼働/停止トグル（構成中の誤発火防止スイッチ）。
                停止中はスケジューラが発火せず、再開時に監視のベースラインを取り直す。 */}
            {isOpSched && (
              <button
                onMouseDown={(e) => e.stopPropagation()}
                onDoubleClick={(e) => e.stopPropagation()}
                onClick={(e) => {
                  e.stopPropagation();
                  onToggleOpPause?.(id);
                }}
                title={
                  opPaused
                    ? "一時停止中です。クリックで稼働を開始します（自動発火が始まります）"
                    : "稼働中です。クリックで一時停止します（構成中の誤発火を防げます）"
                }
                style={{
                  marginTop: 2,
                  padding: "2px 12px",
                  fontSize: 11,
                  fontWeight: 600,
                  color: "#fff",
                  background: opPaused ? "var(--card-op-paused, #5a5f66)" : fstyle.border,
                  border: "none",
                  borderRadius: 5,
                  cursor: "pointer",
                  fontFamily: "sans-serif",
                }}
              >
                {opPaused ? "▶ 稼働開始" : "⏸ 稼働中"}
              </button>
            )}
            {isDrop && (
              <span style={{ fontSize: 9, color: "var(--card-drop-text, #7fb0d8)" }}>📥 ここにドロップ→自動実行</span>
            )}
            {/* v1.3.11 ランチャー: 登録プログラムを起動。接続ファイルがあれば開く。 */}
            {isLauncher && (
              <span style={{ fontSize: 9, color: "var(--card-launcher-text, #7fb0d8)" }}>🚀 ランチャー</span>
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

      {/* v1.7 P2: URLカードの更新マーク（窓喪失の薄赤と同系の控えめなドット・v0.1 §2.3）。
          「更新を確認」で内容の変化を検出した状態。再取得すると消える。 */}
      {isUrl && webUpdate && (
        <div
          title={showHints ? "ページに更新があります（右クリック →「再取得」で新しい実身を作れます・前の実身は残ります）" : undefined}
          style={{
            position: "absolute",
            left: 7,
            top: 7,
            width: 9,
            height: 9,
            borderRadius: "50%",
            background: "#e08a8a",
            border: "1.5px solid #6b3a3a",
            boxShadow: "0 0 5px #e08a8aaa",
            zIndex: 3,
            pointerEvents: "none",
          }}
        />
      )}

      {/* status dot (top-left): 琥珀=仮身 / 緑=実身(今セッション変更) / グレー=実身 */}
      {/* v1.3.11 #12: 通常カードは常時。リンクフォルダ等は普段ドットを出さないが、
          リンク切れ（lost==="real"）のときだけ赤シグナルを表示する。 */}
      {((!isProject && !isUrl && !isFolder && !isFilter && !isLauncher) || lost === "real") && (
      <div
        title={showHints ? status.title : undefined}
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

      {/* v1.8.1 export grip on left edge — appears on hover.
          右端＝TexoS内へつなぐ／左端＝OSへ持ち出す、と両端で向きを対にしている。 */}
      {exportable && (
        <div
          onMouseDown={handleExportDown}
          title="ドラッグして OS へ持ち出す（コピー／未保存の仮身はここで実身になります）"
          style={{
            position: "absolute",
            left: -8,
            top: "50%",
            transform: "translateY(-50%)",
            width: 16,
            height: 16,
            borderRadius: 4,
            background: "#2a3242",
            border: "2px solid #8a93a6",
            boxShadow: "0 1px 4px #00000088",
            color: "#cfd6e4",
            fontSize: 9,
            lineHeight: "12px",
            textAlign: "center",
            cursor: "grab",
            opacity: hover ? 1 : 0,
            transition: "opacity 0.12s",
            zIndex: 2,
            userSelect: "none",
          }}
        >
          ↗
        </div>
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

      {/* v1.10 音声反射カード: 稼働中はカードの下に状態パネル（状態・入力レベル・直近の発話）。 */}
      {ftype === "op_voice" && !opPaused && (
        <VoiceStatus id={id} top={CARD_H + 8} threshold={voiceThresholdOf(filterRef)} />
      )}

      {/* expansion — anchored to this card, moves with it */}
      {expanded && isAiChat && (
        <ChatPanel
          objId={id}
          label={label}
          top={CARD_H + 8}
          onClose={() => onCollapse?.(id)}
          provider={aiConnOf(filterRef).provider}
          endpoint={aiConnOf(filterRef).endpoint}
        />
      )}
      {/* v1.4.17: 画像生成のインラインパネル（ダブルクリックでトグル・AIチャットと同じ流儀）。 */}
      {expanded && isT2i && (
        <T2iPanel
          objId={id}
          label={label}
          top={CARD_H + 8}
          filterRef={filterRef}
          onClose={() => onCollapse?.(id)}
          onRun={onRunFlow}
          onSaved={onAfterSave}
        />
      )}
      {expanded && !isAiChat && isImage && (
        <ImagePreview
          objId={id}
          label={label}
          top={CARD_H + 8}
          width={panelW}
          onResizeEnd={(w) => onPanelResize?.(id, w)}
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
          width={panelW}
          height={panelH}
          onResizeEnd={(w, h) => onPanelResize?.(id, w, h)}
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
            width={panelW}
            height={panelH}
            onResizeEnd={(w, h) => onPanelResize?.(id, w, h)}
            onClose={() => onCollapse?.(id)}
          />
        </Suspense>
      )}
      {/* v2.0 t23d Phase 2: glb/gltf のインライン 3D ビューア（遅延読込・three.js）。 */}
      {expanded && isGlb && (
        <Suspense fallback={null}>
          <GlbPreview
            objId={id}
            label={label}
            top={CARD_H + 8}
            width={panelW}
            height={panelH}
            onResizeEnd={(w, h) => onPanelResize?.(id, w, h)}
            onClose={() => onCollapse?.(id)}
            onThumbnailSaved={() => setThumbNonce((n) => n + 1)}
          />
        </Suspense>
      )}
      {/* v1.3.9: text panel. editPath present → editable (実身); otherwise
          read-only 閲覧 of the LIVE content, with an 「編集」 button that
          commits→edits via onRequestEdit. */}
      {expanded && !isImage && !mimeType.startsWith("audio/") && !isVideo && !isPdf && !isDoc && !isGlb &&
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
