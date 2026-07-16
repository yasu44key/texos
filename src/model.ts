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
  /** v1.5.16: リサイズ可能プレビューの高さ（PDF/Doc。画像・動画はアスペクト追従で幅のみ） */
  panel_h: number | null;
  /** カード変種（v1.4 カード統一モデル §1）: file|url|project|folder|filter */
  kind: string | null;
  /** kind='filter' のフィルタ定義 JSON（v1.4 オートフロー⑤） */
  filter_ref: string | null;
  /** v1.3.10 窓モデル: 内部仮身が指す実身カードの id（外部=origin_path とは別レーン） */
  window_of: string | null;
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
  // v1.4.16 付箋の充実: 色（null=既定の黄）と見出し行フォントサイズ（null=見出しなし）。
  color: string | null;
  title_size: number | null;
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
  // v1.3.10: 内部仮身(window_of)も外部仮身(origin_path)も、生きた窓に届かず CAS に
  // 落ちていれば薄赤。内部仮身は指し先実身がリネーム/喪失で見失われた状態を表す。
  if (
    (o.origin_path || o.window_of) &&
    (o.view_source === "cas" || o.view_source === "fork-cas")
  )
    return "cas";
  return null;
}

// v1.3.9 問題点2: 実身カードなのに実体ファイルが失われている状態（リネーム/移動/削除）。
// 判定は view_source ではなく「自分の committed_path が実在するか」(cur_mtime)で行う。
// view_source 依存だと、空ファイル(0バイト)等で real_id を共有する無関係なカードに
// 兄弟解決が引っ張られ view_source="sibling" となって喪失を見逃すため（実バグで判明）。
// 自分が所有する実ファイルが消えていれば、兄弟の有無に関わらず喪失＝薄赤で警告する。
export function lostRealOf(o: VirtualBodyInfo): boolean {
  if (!o.committed_path) return false; // 仮身は lostOf 側
  if (o.project_ref || o.url_ref || o.folder_ref) return false;
  return o.cur_mtime == null; // committed_path があるのに実ファイルが無い
}

// v1.3.11 #12: リンクフォルダカードの参照先フォルダが存在しない（リンク切れ）。
// list_objects は folder_ref が実在すれば cur_mtime を埋めるので、null＝喪失＝薄赤。
export function lostFolderOf(o: VirtualBodyInfo): boolean {
  return !!o.folder_ref && o.cur_mtime == null;
}

// v1.3.11 ランチャー: 登録プログラムの実パス（アイコン表示用）。ターミナル/未設定は null。
export function launcherPathOf(o: VirtualBodyInfo): string | null {
  if (o.kind !== "launcher") return null;
  try {
    const s = JSON.parse(o.filter_ref ?? "{}");
    return typeof s.path === "string" && s.path ? s.path : null;
  } catch {
    return null;
  }
}

// v1.3.6: 内部実身 — プロジェクト内に実体があり、外部の戻り先を持たない実身。
// 「実身にする」で外部から独立させた実身や、その場取り込み（origin===committed）が該当。
// これらには「仮身に戻す」「実身化し直す」を出してはいけない（戻り先が無く、
// 実行すると編集内容を捨てるだけになる＝危険）。
export function isInternalReal(o: VirtualBodyInfo): boolean {
  if (!o.committed_path) return false;
  if (!o.origin_path) return true;
  return o.origin_path === o.committed_path;
}

// v1.3-①: UIカラープリセット（App.cssの[data-texos-theme]と対応）。
// v1.4.30: グループ化（テーマメニューは区切り線で段落表示）＋中間トーンの配色シリーズ8色を追加。
// 並び = 基本 → 中間（実用）→ 中間（配色）→ パステル → 季節（春夏秋冬を連続に）→ アクセント。
// ※ 従来はビビッドが季節の間（冬と秋の間）に挟まっていた違和感を解消。
export const THEME_GROUPS: [string, string][][] = [
  [
    ["midnight", "ミッドナイト（標準）"],
    ["light", "視認性重視（ライト）"],
  ],
  [
    ["dim", "中間・明るめ（フォグ）"],
    ["slate", "中間・暗め（スレート）"],
    ["warm", "色調重視（セピア）"],
    ["calm", "疲労防止（低刺激）"],
  ],
  // v1.4.30 中間トーンの配色シリーズ（まぶしくなく・暗くなく・華やか）
  [
    ["modern", "モダン（グレージュ×シグナルレッド）"],
    ["britone", "ブライトーン（オートミール×クリアオレンジ）"],
    ["nordic", "ノルディック（白樺×ダスティブルー）"],
    ["japonisme", "ジャポニズム（藍×朱）"],
    ["roman", "ローマン（大理石×テラコッタ）"],
    ["mediterranean", "地中海（白壁×コバルト）"],
    ["venice", "ベニス（漆喰×緑青）"],
    ["cafe", "カフェ（ラテ×エスプレッソ）"],
  ],
  [
    ["pastel-cool", "パステル（クール）"],
    ["pastel-warm", "パステル（ウォーム）"],
  ],
  [
    ["spring", "スプリング（春）"],
    ["summer", "サマー（夏）"],
    ["autumn", "オータム（秋）"],
    ["winter", "ウィンター（冬）"],
  ],
  [["vivid", "ビビッド"]],
];
export const THEMES: [string, string][] = THEME_GROUPS.flat();

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

// v1.4 オートフロー⑤: 接続時型チェック。バックエンド flow.rs の mime_matches と
// 同じ規則を保つこと: 空・"*/*" は全受け入れ / "text/*" は text/ 系＋JSON /
// その他は前方一致 or 完全一致。
export function mimeMatches(accept: string, mime: string): boolean {
  if (!accept || accept === "*/*") return true;
  if (accept.endsWith("/*")) {
    const prefix = accept.slice(0, -2);
    if (prefix === "text" && mime === "application/json") return true;
    return mime.startsWith(prefix + "/");
  }
  return accept === mime;
}

// v1.4 連鎖: フィルタ→フィルタ接続の型チェック用に、フィルタの出力MIMEを静的に推定。
// null = 不明（チェックしない。外部コマンドの出力は実行時にバックエンドが判定）。
export function filterOutputMime(filterRef: string | null): string | null {
  try {
    const s = JSON.parse(filterRef ?? "{}");
    if (s.builtin === "text_grep" || s.builtin === "text_replace") return "text/plain";
    // v1.5.12 P1: Unix標準テキストフィルタ群も常に text/plain を吐く
    // v1.5.17 P2: 集約系（concat/diff/paste）も text/plain（diff の実体拡張子は .diff）
    if (["text_head", "text_tail", "text_sort", "text_count", "text_cut", "text_convert",
         "text_concat", "text_diff", "text_paste"].includes(s.builtin))
      return "text/plain";
    if (s.builtin === "ai") return "text/markdown"; // v1.4.0 AIフィルタは常にMDを吐く
    // v1.7 P1 web_fetch: 通常は text/markdown だが、HTML 以外の直リンクは raw のまま
    // 実身化する（画像/csv/json…）ため静的には不明＝null（チェックしない）。
    if (s.builtin === "web_fetch") return null;
    if (s.builtin === "t2i") return "image/png"; // v1.5 Text-To-Image は常にPNGを吐く
    if (s.builtin === "md_codeblock") return "text/plain"; // v1.4.0 コード抽出（→gcc等）
    if (s.builtin === "img_resize") return "image/png";
    if (s.builtin === "img_convert") {
      const f = String(s.format ?? "png").toLowerCase();
      return f === "jpg" || f === "jpeg" ? "image/jpeg" : `image/${f}`;
    }
  } catch {
    /* 壊れた定義は不明扱い */
  }
  return null;
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
