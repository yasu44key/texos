// 右クリックメニューの構築（App.tsx から分離 — v1.3.2 リファクタリング）。
// 各ビルダーは「対象データ＋アクション群 → MenuItem[]」の純関数。
// ヒットテストと setMenu は App 側（handleContextMenu）に残す。
import { invoke } from "@tauri-apps/api/core";
import type { MenuItem } from "./ContextMenu";
import type { ShapeInfo } from "../objects/ShapeCard";
import type { NoteInfo, VirtualBodyInfo } from "../model";
import { SHAPE_COLORS, THEMES, isTextMime, lostOf } from "../model";

// App のハンドラ群。メニュー項目はすべてこの窓口経由で実行する。
export interface MenuActions {
  // サブメニューを同位置に開き直す（既存メニューのクローズ後に）。
  openSubmenu: (items: MenuItem[]) => void;
  // 空きキャンバス
  contextPlace: (x: number, y: number) => void;
  addNote: (x: number, y: number) => void;
  paste: (x: number, y: number) => void;
  shapeAdd: (kind: string, x: number, y: number) => void;
  addUrl: (x: number, y: number) => void;
  // v1.4 オートフロー
  addFilter: (builtin: string, x: number, y: number) => void;
  runFlow: (id: string) => void;
  editFilter: (id: string, current: string) => void;
  newSubproject: (sibling: boolean, x: number, y: number) => void;
  linkProject: (x: number, y: number) => void;
  autoLayout: () => void;
  commitAll: () => void;
  setTheme: (key: string) => void;
  switchProject: () => void;
  // 選択
  focusSelection: () => void;
  alignSelected: (mode: "row" | "col" | "grid" | "dist-h" | "dist-v") => void;
  bulkCommit: () => void;
  bulkUncommit: () => void;
  bulkDelete: () => void;
  // カード
  openObject: (id: string) => void;
  openDefault: (id: string) => void;
  openWith: (id: string) => void;
  commitReveal: (id: string) => void;
  editInCanvas: (id: string) => void;
  expandView: (id: string) => void;
  collapse: (id: string) => void;
  viewTemp: (id: string) => void;
  recommit: (id: string) => void;
  uncommit: (id: string) => void;
  duplicate: (id: string) => void;
  renameObject: (id: string, current: string) => void;
  setMemo: (id: string, current: string) => void;
  changeUrl: (id: string, current: string) => void;
  deleteObject: (id: string) => void;
  requestDelete: (id: string) => void;
  bringToFront: (id: string) => void;
  sendToBack: (id: string) => void;
  // コメント（付箋）
  noteStyle: (id: string, fontSize: number, bold: boolean) => void;
  commitNoteToFile: (id: string) => void;
  deleteNote: (id: string) => void;
  // 図形
  shapeClickSelect: (id: string) => void;
  shapeStyle: (id: string, fill: string, opacity: number) => void;
  shapeRotate: (id: string, deg: number) => void;
  shapeScale: (id: string, factor: number) => void;
  shapeFitToCards: (id: string) => void;
  shapeLabel: (id: string, current: string) => void;
  shapeZ: (id: string, front: boolean) => void;
  shapeDelete: (id: string) => void;
}

// v1.3 メモ⑦: 選択カードの整列（2枚以上選択時のみメニューに出す）。
export function buildAlignItems(a: MenuActions): MenuItem[] {
  return [
    { label: "横一列に整列（水平）", onClick: () => a.alignSelected("row") },
    { label: "縦一列に整列（垂直）", onClick: () => a.alignSelected("col") },
    { label: "格子状に整列", onClick: () => a.alignSelected("grid") },
    { label: "横方向に均等配置", onClick: () => a.alignSelected("dist-h") },
    { label: "縦方向に均等配置", onClick: () => a.alignSelected("dist-v") },
  ];
}

// v1.3 メモ２追加④: 選択カードへの一括操作（対象件数つき）。
export function buildBulkItems(
  counts: { commitable: number; uncommitable: number; total: number },
  a: MenuActions
): MenuItem[] {
  return [
    {
      label: `まとめて実身化（コミット・${counts.commitable}件）`,
      disabled: !counts.commitable,
      onClick: () => a.bulkCommit(),
    },
    {
      label: `まとめて仮身化（コミット取り消し・${counts.uncommitable}件）`,
      disabled: !counts.uncommitable,
      onClick: () => a.bulkUncommit(),
    },
    {
      label: `まとめて削除（${counts.total}件）`,
      danger: true,
      onClick: () => a.bulkDelete(),
    },
  ];
}

export function buildNoteMenu(
  noteId: string,
  note: NoteInfo | undefined,
  a: MenuActions
): MenuItem[] {
  const cur = note ?? { font_size: 13, bold: false };
  return [
    { label: note?.file_ref ? "リンクテキスト" : "コメント", onClick: () => {}, disabled: true },
    { label: "見出し（大）", onClick: () => a.noteStyle(noteId, 24, true) },
    { label: "小見出し（中）", onClick: () => a.noteStyle(noteId, 17, true) },
    { label: "本文（小）", onClick: () => a.noteStyle(noteId, 13, false) },
    {
      label: cur.bold ? "太字を解除" : "太字",
      onClick: () => a.noteStyle(noteId, cur.font_size, !cur.bold),
    },
    // v1.3-④: コメントを実ファイル化（以後はファイルリンク付箋として編集が書き戻る）
    ...(!note?.file_ref
      ? [
          {
            label: "コミットしてテキストファイルとして保存",
            onClick: () => a.commitNoteToFile(noteId),
          },
        ]
      : []),
    { label: "削除", danger: true, onClick: () => a.deleteNote(noteId) },
  ];
}

interface ObjectMenuCtx {
  selected: Set<string>;
  expanded: Set<string>;
  alignItems: MenuItem[];
  bulkItems: MenuItem[];
}

export function buildObjectMenu(
  targetId: string,
  obj: VirtualBodyInfo | undefined,
  ctx: ObjectMenuCtx,
  a: MenuActions
): MenuItem[] {
  const zItems: MenuItem[] = [
    ...(ctx.selected.has(targetId)
      ? [{ label: "選択範囲にフォーカス", onClick: () => a.focusSelection() }]
      : []),
    ...(ctx.selected.has(targetId) && ctx.selected.size >= 2
      ? [
          { label: "選択カードを整列 ...", onClick: () => a.openSubmenu(ctx.alignItems) },
          { label: "選択カードをまとめて ...", onClick: () => a.openSubmenu(ctx.bulkItems) },
        ]
      : []),
    { label: "最前面へ", onClick: () => a.bringToFront(targetId) },
    { label: "最背面へ", onClick: () => a.sendToBack(targetId) },
  ];
  // Project object: navigate / rename / remove reference.
  if (obj?.project_ref) {
    return [
      { label: `「${obj.label}」`, onClick: () => {}, disabled: true },
      { label: "開く（プロジェクトへ移動）", onClick: () => a.openObject(targetId) },
      ...zItems,
      {
        label: "表示名を変更...",
        onClick: () => a.renameObject(targetId, obj.label),
      },
      {
        label: "削除（参照のみ・フォルダは残る）",
        danger: true,
        onClick: () => a.deleteObject(targetId),
      },
    ];
  }
  // URL link object
  if (obj?.url_ref) {
    return [
      { label: `「${obj.label}」`, onClick: () => {}, disabled: true },
      {
        label: "リンクを開く",
        onClick: () =>
          invoke("open_with_default", { path: obj.url_ref! }).catch(() => {}),
      },
      {
        label: "アプリを選択して開く...",
        onClick: () =>
          invoke("open_url_with_chooser", { url: obj.url_ref! }).catch(() => {}),
      },
      { label: "URLを変更...", onClick: () => a.changeUrl(targetId, obj.url_ref!) },
      ...zItems,
      { label: "コメントを編集...", onClick: () => a.setMemo(targetId, obj.memo ?? "") },
      { label: "表示名を変更...", onClick: () => a.renameObject(targetId, obj.label) },
      { label: "削除", danger: true, onClick: () => a.deleteObject(targetId) },
    ];
  }
  // v1.4 オートフロー: フィルタカード（パイプラインの処理ノード）
  if (obj && (obj.kind === "filter" || obj.filter_ref)) {
    return [
      { label: `⚗「${obj.label}」`, onClick: () => {}, disabled: true },
      { label: "▶ フローを実行", onClick: () => a.runFlow(targetId) },
      {
        label: "フィルタ設定を編集（JSON）...",
        onClick: () => a.editFilter(targetId, obj.filter_ref ?? "{}"),
      },
      ...zItems,
      { label: "表示名を変更...", onClick: () => a.renameObject(targetId, obj.label) },
      { label: "削除", danger: true, onClick: () => a.deleteObject(targetId) },
    ];
  }
  // Folder link object (リンクフォルダ)
  if (obj?.folder_ref) {
    return [
      { label: `「${obj.label}」`, onClick: () => {}, disabled: true },
      {
        label: "フォルダを開く",
        onClick: () =>
          invoke("open_with_default", { path: obj.folder_ref! }).catch(() => {}),
      },
      {
        label: "エクスプローラーで表示",
        onClick: () =>
          invoke("reveal_in_dir", { path: obj.folder_ref! }).catch(() => {}),
      },
      ...zItems,
      { label: "コメントを編集...", onClick: () => a.setMemo(targetId, obj.memo ?? "") },
      { label: "表示名を変更...", onClick: () => a.renameObject(targetId, obj.label) },
      {
        label: "削除（参照のみ・フォルダは残る）",
        danger: true,
        onClick: () => a.deleteObject(targetId),
      },
    ];
  }
  const committed = !!obj?.committed_path;
  // 取り込み元の実ファイルがそのままコミット先（プロジェクト内取り込み）。
  // 仮身に戻す＝元ファイル削除になってしまうため、コマンドを出さない。
  const isOriginalFile =
    !!obj?.origin_path && !!obj?.committed_path && obj.origin_path === obj.committed_path;
  return [
    { label: `「${obj?.label ?? ""}」`, onClick: () => {}, disabled: true },
    // v1.3.1: 窓の先を喪失した仮身 → CASのスナップショットから復活。
    ...(obj && lostOf(obj) === "cas"
      ? [
          {
            label: "取り込み時点の内容を復活（コミットして表示）",
            onClick: () => a.commitReveal(targetId),
          },
        ]
      : []),
    {
      label: "既定のアプリで開く／編集",
      onClick: () => a.openDefault(targetId),
    },
    {
      label: "アプリを選択して開く／編集...",
      onClick: () => a.openWith(targetId),
    },
    // 第3の選択肢: 実身化してExplorerでフォーカス（そこから先はWindows任せ）
    ...(!committed
      ? [
          {
            label: "コミットしてプロジェクトフォルダで表示",
            onClick: () => a.commitReveal(targetId),
          },
        ]
      : []),
    ...(obj && isTextMime(obj.mime_type, obj.ext)
      ? [
          {
            label: "キャンバスで編集（テキスト）",
            onClick: () => a.editInCanvas(targetId),
          },
        ]
      : []),
    ...(obj?.mime_type.startsWith("image/")
      ? [
          {
            label: ctx.expanded.has(targetId) ? "プレビューを閉じる" : "キャンバスでプレビュー",
            onClick: () =>
              ctx.expanded.has(targetId)
                ? a.collapse(targetId)
                : a.expandView(targetId),
          },
        ]
      : []),
    ...(obj?.mime_type.startsWith("audio/")
      ? [
          {
            label: ctx.expanded.has(targetId) ? "再生を閉じる" : "キャンバスで再生",
            onClick: () =>
              ctx.expanded.has(targetId)
                ? a.collapse(targetId)
                : a.expandView(targetId),
          },
        ]
      : []),
    ...(obj &&
    (obj.mime_type.startsWith("video/") ||
      obj.mime_type === "application/pdf" ||
      (obj.ext ?? "").toLowerCase() === "pdf")
      ? [
          {
            label: ctx.expanded.has(targetId)
              ? "プレビューを閉じる"
              : obj.mime_type.startsWith("video/")
              ? "キャンバスで再生（動画）"
              : "キャンバスでプレビュー（PDF）",
            onClick: () =>
              ctx.expanded.has(targetId)
                ? a.collapse(targetId)
                : a.expandView(targetId),
          },
        ]
      : []),
    {
      label: "一時的に閲覧（コピーを破棄）",
      onClick: () => a.viewTemp(targetId),
    },
    ...zItems,
    ...(!committed && obj?.origin_path
      ? [
          {
            label: "借用元のフォルダで表示",
            onClick: () => {
              invoke("reveal_in_dir", { path: obj.origin_path! }).catch(() => {});
            },
          },
        ]
      : []),
    ...(committed
      ? [
          {
            label: "フォルダで表示",
            onClick: () => {
              if (obj?.committed_path)
                invoke("reveal_in_dir", { path: obj.committed_path });
            },
          },
          {
            label: "編集を破棄して再コミット",
            onClick: () => a.recommit(targetId),
          },
          ...(!isOriginalFile
            ? [
                {
                  label: "コミットを取り消す（仮身に戻す）",
                  onClick: () => a.uncommit(targetId),
                },
              ]
            : []),
        ]
      : []),
    {
      label: "元のファイルを呼ぶ（仮身を複製）",
      onClick: () => a.duplicate(targetId),
    },
    {
      label: "表示名を変更...",
      onClick: () => a.renameObject(targetId, obj?.label ?? ""),
    },
    { label: "削除", danger: true, onClick: () => a.requestDelete(targetId) },
  ];
}

// v1.3-②③: shape menu (shapes sit behind cards)
export function buildShapeMenu(
  shapeId: string,
  sh: ShapeInfo | undefined,
  a: MenuActions
): MenuItem[] {
  const kindName = sh?.kind === "ellipse" ? "楕円" : sh?.kind === "arrow" ? "矢印" : "角丸四角";
  return [
    { label: `図形「${sh?.label || kindName}」`, onClick: () => {}, disabled: true },
    { label: "上のカードをまとめて選択", onClick: () => a.shapeClickSelect(shapeId) },
    {
      label: "色を変更 ...",
      onClick: () =>
        a.openSubmenu(
          SHAPE_COLORS.map(([name, hex]) => ({
            label: name,
            onClick: () => a.shapeStyle(shapeId, hex, sh?.opacity ?? 0.2),
          }))
        ),
    },
    {
      label: "濃さを変更 ...",
      onClick: () =>
        a.openSubmenu(
          [
            ["薄く", 0.12],
            ["標準", 0.2],
            ["濃く", 0.38],
          ].map(([name, op]) => ({
            label: name as string,
            onClick: () => a.shapeStyle(shapeId, sh?.fill ?? "#4a90d9", op as number),
          }))
        ),
    },
    ...(sh?.kind === "arrow"
      ? [
          {
            label: "向きを変更 ...",
            onClick: () =>
              a.openSubmenu(
                [0, 45, 90, 135, 180, 225, 270, 315].map((deg) => ({
                  label: `${deg}°`,
                  onClick: () => a.shapeRotate(shapeId, deg),
                }))
              ),
          },
        ]
      : []),
    // v1.3 メモ⑥: ハンドルが隠れていてもメニューから拡縮できる。
    {
      label: "図形の拡縮 ...",
      onClick: () =>
        a.openSubmenu([
          { label: "上のカードを含む大きさに", onClick: () => a.shapeFitToCards(shapeId) },
          { label: "拡大（1.25倍）", onClick: () => a.shapeScale(shapeId, 1.25) },
          { label: "縮小（0.8倍）", onClick: () => a.shapeScale(shapeId, 0.8) },
        ]),
    },
    { label: "名前を変更...", onClick: () => a.shapeLabel(shapeId, sh?.label ?? "") },
    { label: "最前面へ（図形内）", onClick: () => a.shapeZ(shapeId, true) },
    { label: "最背面へ（図形内）", onClick: () => a.shapeZ(shapeId, false) },
    { label: "図形を削除", danger: true, onClick: () => a.shapeDelete(shapeId) },
  ];
}

interface CanvasMenuCtx {
  selectionCount: number;
  theme: string;
  alignItems: MenuItem[];
  bulkItems: MenuItem[];
}

export function buildCanvasMenu(
  pos: { canvasX: number; canvasY: number },
  ctx: CanvasMenuCtx,
  a: MenuActions
): MenuItem[] {
  return [
    ...(ctx.selectionCount
      ? [{ label: "選択範囲にフォーカス", onClick: () => a.focusSelection() }]
      : []),
    ...(ctx.selectionCount >= 2
      ? [
          { label: "選択カードを整列 ...", onClick: () => a.openSubmenu(ctx.alignItems) },
          { label: "選択カードをまとめて ...", onClick: () => a.openSubmenu(ctx.bulkItems) },
        ]
      : []),
    {
      label: "ファイルを配置...",
      onClick: () => a.contextPlace(pos.canvasX, pos.canvasY),
    },
    {
      label: "コメントを追加...",
      onClick: () => a.addNote(pos.canvasX, pos.canvasY),
    },
    {
      label: "クリップボードから貼り付け",
      onClick: () => a.paste(pos.canvasX, pos.canvasY),
    },
    {
      label: "図形を追加 ...",
      onClick: () =>
        a.openSubmenu([
          { label: "角丸四角", onClick: () => a.shapeAdd("rect", pos.canvasX, pos.canvasY) },
          { label: "楕円", onClick: () => a.shapeAdd("ellipse", pos.canvasX, pos.canvasY) },
          { label: "太矢印", onClick: () => a.shapeAdd("arrow", pos.canvasX, pos.canvasY) },
        ]),
    },
    {
      label: "URLリンクを追加...",
      onClick: () => a.addUrl(pos.canvasX, pos.canvasY),
    },
    // v1.4 オートフロー⑤: フィルタカード（データ→フィルタ→結果のパイプ）
    {
      label: "フィルタカードを追加 ...",
      onClick: () =>
        a.openSubmenu([
          { label: "テキスト: grep（正規表現で行を抽出）...", onClick: () => a.addFilter("text_grep", pos.canvasX, pos.canvasY) },
          { label: "テキスト: 置換（正規表現）...", onClick: () => a.addFilter("text_replace", pos.canvasX, pos.canvasY) },
          { label: "画像: リサイズ（幅指定）...", onClick: () => a.addFilter("img_resize", pos.canvasX, pos.canvasY) },
          { label: "画像: 形式変換...", onClick: () => a.addFilter("img_convert", pos.canvasX, pos.canvasY) },
          { label: "外部コマンド（{in} {out}）...", onClick: () => a.addFilter("external", pos.canvasX, pos.canvasY) },
        ]),
    },
    {
      label: "新規プロジェクトを内包（コミット）...",
      onClick: () => a.newSubproject(false, pos.canvasX, pos.canvasY),
    },
    {
      label: "新規プロジェクトを分岐（フォーク）...",
      onClick: () => a.newSubproject(true, pos.canvasX, pos.canvasY),
    },
    {
      label: "既存プロジェクトへのリンクを配置...",
      onClick: () => a.linkProject(pos.canvasX, pos.canvasY),
    },
    {
      label: "自動レイアウト（重なり解消）",
      onClick: () => a.autoLayout(),
    },
    {
      label: "全コミット（データ持ち出し）...",
      onClick: () => a.commitAll(),
    },
    {
      label: "UIカラー ...",
      onClick: () =>
        a.openSubmenu(
          THEMES.map(([key, name]) => ({
            label: (ctx.theme === key ? "✓ " : "　") + name,
            onClick: () => a.setTheme(key),
          }))
        ),
    },
    {
      label: "プロジェクトを切り替え...",
      onClick: () => a.switchProject(),
    },
  ];
}
