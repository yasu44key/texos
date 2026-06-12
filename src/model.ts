// 共有データモデルと純粋ヘルパー（App.tsx から分離 — v1.3.2 リファクタリング）。
// バックエンド（list_objects / list_notes）の行型と、型に依存する判定関数を置く。
import type { ShapeInfo } from "./objects/ShapeCard";

export interface VirtualBodyInfo {
  id: string;
  real_id: string;
  label: string;
  mime_type: string;
  pos_x: number;
  pos_y: number;
  committed_path: string | null;
  committed_modified: boolean;
  project_ref: string | null;
  ext: string | null;
  expanded: boolean;
  url_ref: string | null;
  memo: string | null;
  z_order: number;
  folder_ref: string | null;
  origin_path: string | null;
  cur_mtime: number | null;
  view_source: string | null;
  panel_w: number | null;
}

export interface NoteInfo {
  id: string;
  text: string;
  pos_x: number;
  pos_y: number;
  width: number;
  font_size: number;
  bold: boolean;
  file_ref: string | null;
}

export const PROJECT_MIME = "application/vnd.texos.project";

export const TEXT_EXTS = new Set(["txt", "md", "csv", "json", "xml", "py", "js", "ts", "rs", "c", "cpp", "h", "css", "html", "yaml", "yml", "toml", "ini", "log"]);
// v1.3-⑧: known binary containers must NEVER be treated as text, even when an
// old DB row carries a wrong text-ish MIME (e.g. .docx imported pre-v1.3).
export const BINARY_EXTS = new Set(["doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp", "rtf", "pdf", "zip", "7z", "rar", "gz", "tar", "exe", "msi", "dll", "lnk", "blend", "psd", "ai", "epub"]);
export function isTextMime(mime: string, ext?: string | null): boolean {
  if (ext && BINARY_EXTS.has(ext.toLowerCase())) return false;
  if (mime.startsWith("text/")) return true;
  if (mime === "application/json") return true;
  if (ext && TEXT_EXTS.has(ext.toLowerCase())) return true;
  return false;
}

// v1.3-⑩ 薄赤判定: 仮身の窓が生きたファイルに届いていない状態。
export function lostOf(o: VirtualBodyInfo): "cas" | "none" | null {
  if (o.committed_path || o.project_ref || o.url_ref || o.folder_ref) return null;
  if (o.view_source === "none") return "none";
  if (o.origin_path && (o.view_source === "cas" || o.view_source === "fork-cas")) return "cas";
  return null;
}

// v1.3-①: UIカラープリセット（App.cssの[data-texos-theme]と対応）
export const THEMES: [string, string][] = [
  ["midnight", "ミッドナイト（標準）"],
  ["light", "視認性重視（ライト）"],
  ["warm", "色調重視（セピア）"],
  ["calm", "疲労防止（低刺激）"],
];

// v1.3-②: 図形のカラーパレット
export const SHAPE_COLORS: [string, string][] = [
  ["青", "#4a90d9"],
  ["緑", "#3fae6a"],
  ["琥珀", "#e8b84a"],
  ["赤", "#d96a6a"],
  ["紫", "#9a7bd9"],
  ["グレー", "#8a93a6"],
];

// v1.3 メモ⑤: 図形の階層化 — 中心が親図形の上に乗っているかで「従う」関係を判定。
export function shapeCenterOn(child: ShapeInfo, parent: ShapeInfo): boolean {
  const cx = child.pos_x + child.width / 2;
  const cy = child.pos_y + child.height / 2;
  return (
    cx >= parent.pos_x && cx <= parent.pos_x + parent.width &&
    cy >= parent.pos_y && cy <= parent.pos_y + parent.height
  );
}

// Estimated footprint of a note (used for links, selection, layout).
// 実測値（ResizeObserver）が取れるまでのフォールバック。
export function noteSize(n: NoteInfo): { w: number; h: number } {
  const w = n.width || 220;
  const charW = n.font_size * 0.95; // CJK-leaning estimate
  let lines = 0;
  for (const para of (n.text || " ").split("\n")) {
    lines += Math.max(1, Math.ceil((para.length * charW) / Math.max(80, w - 24)));
  }
  return { w, h: Math.max(40, lines * n.font_size * 1.5 + 20) };
}
