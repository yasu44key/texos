// 右クリックメニューの構築（App.tsx から分離 — v1.3.2 リファクタリング）。
// 各ビルダーは「対象データ＋アクション群 → MenuItem[]」の純関数。
// ヒットテストと setMenu は App 側（handleContextMenu）に残す。
import { invoke } from "@tauri-apps/api/core";
import type { MenuItem } from "./ContextMenu";
import type { ShapeInfo } from "../objects/ShapeCard";
import type { NoteInfo, VirtualBodyInfo } from "../model";
import { SHAPE_COLORS, THEME_GROUPS, isTextMime, lostOf, lostRealOf, isInternalReal } from "../model";

// App のハンドラ群。メニュー項目はすべてこの窓口経由で実行する。
export interface MenuActions {
  // サブメニューを同位置に開き直す（既存メニューのクローズ後に）。
  openSubmenu: (items: MenuItem[]) => void;
  // 空きキャンバス
  contextPlace: (x: number, y: number) => void;
  // v1.3.11 #8/#9: 新規ファイルをプロジェクトフォルダに作成し実身として配置。
  newFile: (x: number, y: number) => void;
  addNote: (x: number, y: number) => void;
  paste: (x: number, y: number) => void;
  shapeAdd: (kind: string, x: number, y: number) => void;
  addUrl: (x: number, y: number) => void;
  // v1.4 オートフロー
  addFilter: (builtin: string, x: number, y: number) => void;
  runFlow: (id: string) => void;
  // v1.3.11 ランチャー（一般プログラム/cmd/PowerShell の登録・起動）
  addLauncher: (builtin: string, x: number, y: number) => void;
  runLauncher: (id: string) => void;
  // v1.3.11 ランチャーライブラリ（一度登録→リスト）
  addLibraryLauncher: (name: string, x: number, y: number) => void;
  saveLauncherToLibrary: (id: string) => void;
  deleteLibraryLauncher: (name: string) => void;
  editFilter: (id: string, current: string) => void;
  // v1.5 フィルタライブラリ（アプリ共通・プロジェクト横断）
  saveFilterToLibrary: (id: string) => void;
  addLibraryFilter: (name: string, x: number, y: number) => void;
  deleteLibraryFilter: (name: string) => void;
  feedDrop: (id: string) => void;
  runDrop: (id: string) => void;
  newSubproject: (sibling: boolean, x: number, y: number) => void;
  linkProject: (x: number, y: number) => void;
  autoLayout: () => void;
  commitAll: () => void;
  // v1.4.19 カードを検索: ライブ検索→フォーカス（Ctrl+F）。
  openSearch: () => void;
  // v1.4.19 ゴミ箱を開く: 削除済みの一覧・復活・完全削除（空にする）。
  openTrash: () => void;
  // v1.4.15 データの掃除A: UNDO/REDO履歴＋サムネイル・一時プレビューのキャッシュを掃除。
  cleanCaches: () => void;
  // v1.4.15 データの掃除B（正規化）: どのカードからも参照されない孤立CASデータを削除。
  gcCas: () => void;
  setTheme: (key: string) => void;
  // v1.4.16 遊び心: 画面全体の明るさ・コントラストのお手軽調整。
  uiAdjust: (kind: "brighter" | "darker" | "contrastUp" | "contrastDown" | "reset") => void;
  // v1.3.4: メニューの説明ツールチップ表示のオン/オフ。
  toggleMenuHints: () => void;
  // v1.4.29: カードの説明ツールチップ表示のオン/オフ（メニューとは別系統）。
  toggleCardHints: () => void;
  switchProject: () => void;
  // v1.4.12: キーボードショートカット一覧（? / F1 でも開閉）。
  showHelp: () => void;
  // 選択
  focusSelection: () => void;
  // v1.4.12: 関係矢印でつながった連結成分（つながり）をまるごと選択（Alt+クリックと同じ）。
  selectConnected: (id: string) => void;
  alignSelected: (mode: "row" | "col" | "grid" | "dist-h" | "dist-v") => void;
  bulkCommit: () => void;
  bulkUncommit: () => void;
  bulkDelete: () => void;
  // v1.4.14: 選択した部分グラフ（カード＋コメント＋内部の関係矢印）を別プロジェクトへ
  // コピー／移動する。送り先は最近のプロジェクト一覧から選ぶ（sendSelection が非同期で
  // 一覧を取得してサブメニューを開く）。
  sendSelection: (mode: "copy" | "move") => void;
  // v1.3.4 クリップボード持ち出し（選択カードをファイルとして）
  copyFiles: (ids: string[]) => void;
  /** v1.3.8 (G6): 画像カードをビットマップ(CF_DIB)としてクリップボードへ。 */
  copyImage: (id: string) => void;
  // カード
  // v1.3.9 ①: 閲覧（ダブルクリックと同じ非破壊ディスパッチ）。メニュー先頭に置く。
  view: (id: string) => void;
  openObject: (id: string) => void;
  openDefault: (id: string) => void;
  openWith: (id: string) => void;
  commitReveal: (id: string) => void;
  // v1.3.11 #1: 借用元（origin_path）が属するプロジェクトを開く（無ければフォルダ）。
  openOriginProject: (path: string) => void;
  // v1.3.11 #12: リンクフォルダの参照先を選び直す（リンク切れの繋ぎ直し）。
  relinkFolder: (id: string, currentPath: string) => void;
  // v1.3.9 問題点2: 実体喪失の実身を救う2経路。
  relink: (id: string) => void;
  restoreFromCas: (id: string) => void;
  editInCanvas: (id: string) => void;
  expandView: (id: string) => void;
  collapse: (id: string) => void;
  viewTemp: (id: string) => void;
  recommit: (id: string) => void;
  uncommit: (id: string) => void;
  duplicate: (id: string) => void;
  // v1.3.6 変更②: この仮身が指す内容を独立した内部実身にする（元ファイルから独立）。
  makeReal: (id: string) => void;
  renameObject: (id: string, current: string) => void;
  // v1.3.10: 実身の実ファイルを TexoS から改名（仮身は window_of で追従）。
  renameReal: (id: string, current: string) => void;
  setMemo: (id: string, current: string) => void;
  changeUrl: (id: string, current: string) => void;
  deleteObject: (id: string) => void;
  requestDelete: (id: string) => void;
  bringToFront: (id: string) => void;
  sendToBack: (id: string) => void;
  // コメント（付箋）
  noteStyle: (id: string, fontSize: number, bold: boolean) => void;
  // v1.4.16 付箋の充実: 色を設定（null=既定の黄に戻す）／自由なカラーピッカーを開く。
  noteColor: (id: string, color: string | null) => void;
  noteColorPick: (id: string) => void;
  // v1.4.16 付箋の充実: 先頭行を見出しにするフォントサイズ（null=見出しなし）。
  noteTitle: (id: string, titleSize: number | null) => void;
  // v1.3.5: コメント内容から独立したテキスト仮身カードを作成（実身化の後継）。
  noteToTextCard: (id: string) => void;
  deleteNote: (id: string) => void;
  // 図形
  shapeClickSelect: (id: string) => void;
  // v1.4.23 ③: 選択部（カード＋コメント）を囲む図形を新規作成して選択状態にする。
  shapeAroundSelection: () => void;
  // v1.4.25 ⑤: 図形グリッドスナップのオン/オフ切替と、全図形の強制スナップ。
  toggleShapeSnap: () => void;
  snapAllShapes: () => void;
  shapeStyle: (id: string, fill: string, opacity: number) => void;
  shapeRotate: (id: string, deg: number) => void;
  shapeFitToCards: (id: string) => void;
  shapeAlignGrid: (id: string) => void;
  shapeLabel: (id: string, current: string) => void;
  shapeZ: (id: string, front: boolean) => void;
  shapeDelete: (id: string) => void;
}

// v1.3.4: 区切り線アイテム。グループ間に挟む。
const DIV: MenuItem = { divider: true, label: "", onClick: () => {} };

// v1.3.4: 先頭・末尾・連続する区切り線を畳む（条件分岐で空グループになっても綺麗に）。
function compact(items: MenuItem[]): MenuItem[] {
  const out: MenuItem[] = [];
  for (const it of items) {
    if (it.divider) {
      if (out.length === 0 || out[out.length - 1].divider) continue;
    }
    out.push(it);
  }
  while (out.length && out[out.length - 1].divider) out.pop();
  return out;
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
  counts: { commitable: number; uncommitable: number; total: number; notes: number; ids: string[] },
  a: MenuActions
): MenuItem[] {
  return [
    {
      label: `まとめて実身化（${counts.commitable}件）`,
      hint: "選択した仮身をまとめてプロジェクトに保存し、実身（実体）にします。",
      disabled: !counts.commitable,
      onClick: () => a.bulkCommit(),
    },
    {
      label: `まとめて仮身に戻す（${counts.uncommitable}件）`,
      hint: "選択した実身をまとめて参照（仮身）に戻し、プロジェクト内の実体を消します。",
      disabled: !counts.uncommitable,
      onClick: () => a.bulkUncommit(),
    },
    {
      label: `まとめてクリップボードにコピー（${counts.total}件）`,
      hint: "選択カードをファイルとしてOSのクリップボードへ。未保存の仮身は自動で実身化されます。",
      shortcut: "Ctrl+C",
      disabled: !counts.total,
      onClick: () => a.copyFiles(counts.ids),
    },
    {
      label: "選択を別プロジェクトへ...",
      hint: "選択したカード・コメントと、その内部の関係矢印を別プロジェクトへコピー／移動します。フィルタ等の設定も引き継ぎます。",
      disabled: !counts.total,
      onClick: () =>
        a.openSubmenu([
          {
            label: "仮身化してコピー...",
            hint: "送り先には仮身として置きます（中身は元プロジェクトのファイルを指す窓＝元に依存）。元プロジェクトはそのまま残ります。",
            onClick: () => a.sendSelection("copy"),
          },
          {
            label: "実身化して移動...",
            hint: "送り先に独立した実身（実ファイル）として置き、元プロジェクトからは削除します。実体が移るため Ctrl+Z では復活できません（元にも残す場合は「仮身化してコピー」を）。",
            onClick: () => a.sendSelection("move"),
          },
        ]),
    },
    {
      // v1.4.30: 選択コメントも一緒に消えるようになったため件数に含める。
      label: `まとめて削除（${counts.total + counts.notes}件）`,
      danger: true,
      shortcut: "Delete",
      onClick: () => a.bulkDelete(),
    },
  ];
}

// v1.4.16 付箋の充実: テーマ（暗色）に馴染む控えめな付箋カラーのパレット。
// 値は付箋の背景色（hex）。文字色・枠は NoteCard が明度から自動決定する。
export const NOTE_COLORS: [string, string][] = [
  ["イエロー", "#3f3a1e"],
  ["オレンジ", "#402c1c"],
  ["グリーン", "#233c2b"],
  ["ティール", "#1f332f"],
  ["ブルー", "#223247"],
  ["パープル", "#322544"],
  ["ピンク", "#3f2330"],
  ["グレー", "#2f3542"],
];

export function buildNoteMenu(
  noteId: string,
  note: NoteInfo | undefined,
  a: MenuActions
): MenuItem[] {
  const cur = note ?? { font_size: 13, bold: false, title_size: null };
  const bodySizes: [string, number][] = [
    ["極小", 11],
    ["小", 13],
    ["中", 15],
    ["大", 18],
    ["特大", 24],
  ];
  const titleSizes: [string, number][] = [
    ["小", 18],
    ["中", 22],
    ["大", 28],
    ["特大", 36],
  ];
  return [
    { label: note?.file_ref ? "リンクテキスト" : "コメント", onClick: () => {}, disabled: true },
    {
      label: "文字サイズ ...",
      hint: "本文（見出し行以外）のフォントサイズを選びます。",
      onClick: () =>
        a.openSubmenu(
          bodySizes.map(([n, s]) => ({
            label: (Math.round(cur.font_size) === s ? "✓ " : "　") + `${n}（${s}）`,
            onClick: () => a.noteStyle(noteId, s, cur.bold),
          }))
        ),
    },
    {
      label: cur.bold ? "太字を解除" : "太字",
      onClick: () => a.noteStyle(noteId, cur.font_size, !cur.bold),
    },
    {
      label: "見出し行 ...",
      hint: "先頭行を見出しとして大きく太字で表示します（残りは本文サイズ）。マップの案内役に。",
      onClick: () =>
        a.openSubmenu([
          {
            label: (cur.title_size == null ? "✓ " : "　") + "オフ（見出しなし）",
            onClick: () => a.noteTitle(noteId, null),
          },
          ...titleSizes.map(([n, s]) => ({
            label: (cur.title_size === s ? "✓ " : "　") + `${n}（${s}）`,
            onClick: () => a.noteTitle(noteId, s),
          })),
        ]),
    },
    {
      label: "色 ...",
      hint: "付箋の色を変えます。マップの案内役として色分けできます。",
      onClick: () =>
        a.openSubmenu([
          {
            label: "標準（既定の黄）",
            swatch: "#3a3520",
            onClick: () => a.noteColor(noteId, null),
          },
          ...NOTE_COLORS.map(([name, hex]) => ({
            label: name,
            swatch: hex,
            onClick: () => a.noteColor(noteId, hex),
          })),
          { label: "その他の色...", onClick: () => a.noteColorPick(noteId) },
        ]),
    },
    // v1.3.5: コメントの内容から独立したテキスト仮身カードを作る（コメントは不変）。
    // コピペで作るテキスト仮身（create_text_object）と全く同じ。実身化（旧）は廃止。
    {
      label: "内容でテキストカードを作成",
      hint: "内容からテキスト仮身カードを作ります。コピペで作るテキストカードと同じで、独立した新カード（リンク矢印は引き継ぎません）。コメントはそのまま残ります。",
      onClick: () => a.noteToTextCard(noteId),
    },
    { label: "削除", danger: true, shortcut: "Delete", onClick: () => a.deleteNote(noteId) },
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
  // v1.4.x ③: 選択系（selItems）とz順（zOrderItems）を分離。通常カードでは前者を
  // 「選択」グループ、後者は「その他の機能」サブメニューに振り分けて項目を削減する。
  // 他種カード（プロジェクト/URL/フォルダ等）は従来どおり zItems をそのまま使う。
  const selItems: MenuItem[] = [
    ...(ctx.selected.has(targetId)
      ? [{ label: "選択範囲にフォーカス", onClick: () => a.focusSelection() }]
      : []),
    ...(ctx.selected.has(targetId) && ctx.selected.size >= 2
      ? [
          { label: "選択カードを整列 ...", onClick: () => a.openSubmenu(ctx.alignItems) },
          { label: "選択カードをまとめて ...", onClick: () => a.openSubmenu(ctx.bulkItems) },
        ]
      : []),
  ];
  const zOrderItems: MenuItem[] = [
    { label: "最前面へ", onClick: () => a.bringToFront(targetId) },
    { label: "最背面へ", onClick: () => a.sendToBack(targetId) },
  ];
  const zItems: MenuItem[] = [...selItems, ...zOrderItems];
  const multi = ctx.selected.has(targetId) && ctx.selected.size >= 2;
  // v1.3.7 C: 複数選択時はインラインの削除を出さず「選択カードをまとめて ...」サブ
  // メニューに一本化（重複排除。通常カードと同じ扱いをリンクカードにも横展開）。
  // 単独時は単体削除に Delete を併記。スプレッドで使う（multi のとき空配列）。
  const deleteItems = (single: MenuItem): MenuItem[] =>
    multi ? [] : [{ ...single, shortcut: "Delete" }];
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
      ...deleteItems({
        label: "削除（参照のみ・フォルダは残る）",
        danger: true,
        onClick: () => a.deleteObject(targetId),
      }),
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
      { label: obj.memo ? "コメントを編集..." : "コメントを追加...", onClick: () => a.setMemo(targetId, obj.memo ?? "") },
      { label: "表示名を変更...", onClick: () => a.renameObject(targetId, obj.label) },
      ...deleteItems({ label: "削除", danger: true, onClick: () => a.deleteObject(targetId) }),
    ];
  }
  // v1.4 オートフロー: ドロップポイント（パイプラインのデータ入口）
  if (obj && obj.kind === "drop") {
    return [
      { label: `📥「${obj.label}」`, onClick: () => {}, disabled: true },
      { label: "ファイルを選んで投入（→自動実行）...", onClick: () => a.feedDrop(targetId) },
      { label: "▶ 下流のフローを実行", onClick: () => a.runDrop(targetId) },
      { label: "つながりを選択", hint: "関係矢印でつながったカード全体を選択します（Alt+クリックと同じ）。", onClick: () => a.selectConnected(targetId) },
      ...zItems,
      { label: obj.memo ? "コメントを編集..." : "コメントを追加...", onClick: () => a.setMemo(targetId, obj.memo ?? "") },
      { label: "表示名を変更...", onClick: () => a.renameObject(targetId, obj.label) },
      ...deleteItems({ label: "削除", danger: true, onClick: () => a.deleteObject(targetId) }),
    ];
  }
  // v1.3.11 ランチャーカード（一般プログラム/ターミナルの起動）。filter_ref を持つため
  // フィルタ判定より先に分岐する。
  if (obj && obj.kind === "launcher") {
    return [
      { label: `🚀「${obj.label}」`, onClick: () => {}, disabled: true },
      { label: "▶ 起動", onClick: () => a.runLauncher(targetId) },
      { label: "つながりを選択", hint: "関係矢印でつながったカード全体を選択します（Alt+クリックと同じ）。", onClick: () => a.selectConnected(targetId) },
      {
        label: "設定を編集...",
        hint: "登録プログラムのパスや引数（JSON）を編集します。",
        onClick: () => a.editFilter(targetId, obj.filter_ref ?? "{}"),
      },
      { label: "ライブラリに登録...", onClick: () => a.saveLauncherToLibrary(targetId) },
      ...zItems,
      { label: obj.memo ? "コメントを編集..." : "コメントを追加...", onClick: () => a.setMemo(targetId, obj.memo ?? "") },
      { label: "表示名を変更...", onClick: () => a.renameObject(targetId, obj.label) },
      ...deleteItems({ label: "削除", danger: true, onClick: () => a.deleteObject(targetId) }),
    ];
  }
  // v1.4 オートフロー: フィルタカード（パイプラインの処理ノード）
  if (obj && (obj.kind === "filter" || obj.filter_ref)) {
    return [
      { label: `⚗「${obj.label}」`, onClick: () => {}, disabled: true },
      { label: "▶ フローを実行", onClick: () => a.runFlow(targetId) },
      { label: "つながりを選択", hint: "関係矢印でつながったカード全体を選択します（Alt+クリックと同じ）。", onClick: () => a.selectConnected(targetId) },
      {
        // 外部プログラムは設定ダイアログ、内蔵フィルタはJSON編集（App側で分岐）
        label: "フィルタ設定を編集...",
        onClick: () => a.editFilter(targetId, obj.filter_ref ?? "{}"),
      },
      // v1.5: 設定済みフィルタに名前を付けてプロジェクト横断で再利用
      { label: "ライブラリに登録...", onClick: () => a.saveFilterToLibrary(targetId) },
      ...zItems,
      { label: obj.memo ? "コメントを編集..." : "コメントを追加...", onClick: () => a.setMemo(targetId, obj.memo ?? "") },
      { label: "表示名を変更...", onClick: () => a.renameObject(targetId, obj.label) },
      ...deleteItems({ label: "削除", danger: true, onClick: () => a.deleteObject(targetId) }),
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
      // v1.3.11 #12: リンク切れ（リネーム/移動）したフォルダを繋ぎ直す。
      {
        label: "リンクし直す...",
        hint: "参照先フォルダが見つからない（リネーム・移動）ときに、正しいフォルダを選んで繋ぎ直します。元のリンク先の親フォルダが既定で開きます。",
        onClick: () => a.relinkFolder(targetId, obj.folder_ref!),
      },
      ...zItems,
      { label: obj.memo ? "コメントを編集..." : "コメントを追加...", onClick: () => a.setMemo(targetId, obj.memo ?? "") },
      { label: "表示名を変更...", onClick: () => a.renameObject(targetId, obj.label) },
      ...deleteItems({
        label: "削除（参照のみ・フォルダは残る）",
        danger: true,
        onClick: () => a.deleteObject(targetId),
      }),
    ];
  }
  const committed = !!obj?.committed_path;
  // v1.3.6: 内部実身（外部の戻り先が無い実身）。仮身に戻す／実身化し直すは出さない
  // （戻り先が無く、実行すると編集を捨てるだけ＝危険）。その場取り込みもこれに含む。
  const internalReal = !!obj && isInternalReal(obj);
  // v1.3.6 変更③: 危険度の高い操作（仮身に戻す・実身化し直す・クリップボードにコピー）は
  // 「特別 ...」サブメニューへ退避して優先度を下げる（誤操作防止）。「仮身に戻す」は origin を
  // 指す実身（外部取り込み／フォーク由来）だけに出す＝編集破棄の戻し操作。
  const specialItems: MenuItem[] = compact([
    ...(committed && !internalReal
      ? [
          {
            label: "実身化し直す",
            hint: "現在の編集を破棄し、取り込み時点の内容で実身化し直します。",
            onClick: () => a.recommit(targetId),
          },
          {
            label: "仮身に戻す",
            hint: "プロジェクト内の実体を消し、元ファイルへの参照（仮身）に戻します。編集内容は破棄されます。",
            danger: true,
            onClick: () => a.uncommit(targetId),
          },
        ]
      : []),
    ...(!multi
      ? [
          {
            label: "クリップボードにコピー",
            hint: "このカードをファイルとしてOSのクリップボードへ。Explorerや他アプリに貼り付けられます。未保存の仮身は自動で実身化されます。",
            shortcut: "Ctrl+C",
            onClick: () => a.copyFiles([targetId]),
          },
        ]
      : []),
  ]);
  // v1.4.x ③: 安全だが低頻度の操作を「その他の機能 ...」へ集約してメニューを短くする
  // （よく使う「開く・編集」「実身⇔仮身」「表示名」「削除」をトップに残す）。
  const otherItems: MenuItem[] = compact([
    {
      label: "アプリを選択して開く／編集...",
      onClick: () => a.openWith(targetId),
    },
    ...(obj?.mime_type.startsWith("image/") && !multi
      ? [
          {
            label: "クリップボードに画像をコピー",
            hint: "この画像をビットマップとしてOSのクリップボードへ。ペイント等の画像アプリに直接貼り付けられます（Ctrl+C はファイルとしてのコピー）。",
            onClick: () => a.copyImage(targetId),
          },
        ]
      : []),
    {
      label: "一時的に閲覧",
      hint: "プロジェクトに保存せず、一時コピーで中身を確認します。閉じるとコピーは破棄されます。",
      onClick: () => a.viewTemp(targetId),
    },
    ...zOrderItems,
    ...(!committed && obj?.origin_path
      ? [
          {
            // v1.3.11 #1: 借用元がTexoSプロジェクト内なら、そのプロジェクトを開く。
            label: "借用元のプロジェクトを開く",
            hint: "借用元のファイルがTexoSプロジェクト内にあれば、そのプロジェクトに切り替えます。プロジェクトでなければ借用元フォルダを開きます。",
            onClick: () => a.openOriginProject(obj.origin_path!),
          },
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
        ]
      : []),
    ...(!committed
      ? [
          {
            label: "実身化（フォルダで表示）",
            hint: "このカードの内容をプロジェクトに保存して実身（実体）にし、保存先フォルダを開きます。",
            onClick: () => a.commitReveal(targetId),
          },
        ]
      : []),
    ...(committed
      ? [
          {
            label: "実ファイル名の変更...",
            hint: "このカードの実体ファイルを改名します（連番 (2) を消す等）。この実身を指す仮身は自動で追従します。",
            onClick: () => a.renameReal(targetId, obj?.label ?? ""),
          },
        ]
      : []),
    // v1.4.12: 任意カードにコメント（メモ）。ホバー時にツールチップで表示される。
    ...(!multi
      ? [
          {
            label: obj?.memo ? "コメントを編集..." : "コメントを追加...",
            hint: "このカードに自由なメモを付けます。カードにマウスを乗せるとツールチップとして表示されます。",
            onClick: () => a.setMemo(targetId, obj?.memo ?? ""),
          },
        ]
      : []),
  ]);
  // v1.3.4: 用語を実身/仮身に統一し、グループを区切り線で分ける。
  // 説明は label から外してツールチップ(hint)へ。compact() で空グループの線を畳む。
  return compact([
    { label: `「${obj?.label ?? ""}」`, onClick: () => {}, disabled: true },
    // v1.3.9 ①: 閲覧をメニュー先頭に（＝ダブルクリックと同じ非破壊の覗き）。
    {
      label: "閲覧",
      hint: "中身を読み取り専用で表示します（実身化しません）。インラインで表示できない種別は一時コピーで開き、閉じると破棄されます。",
      onClick: () => a.view(targetId),
    },
    // v1.3.1: 窓の先を喪失した仮身 → CASのスナップショットから復活。
    ...(obj && lostOf(obj) === "cas"
      ? [
          {
            label: "取り込み時点の内容を復活",
            hint: "失われた窓の先を、CASに残るスナップショットから実身化して表示します。",
            onClick: () => a.commitReveal(targetId),
          },
        ]
      : []),
    // v1.3.9 問題点2: 実体が失われた実身 → 正しいファイルを選び直す／無ければCAS復元。
    ...(obj && committed && lostRealOf(obj)
      ? [
          {
            label: "実ファイルを再リンク...",
            hint: "この実身の実体ファイルが見つかりません（リネーム・フォルダ移動など）。移動先の正しいファイルを選んで繋ぎ直します。",
            onClick: () => a.relink(targetId),
          },
          {
            label: "CASから復元",
            hint: "正しいファイルが見つからない場合に、取り込み時点のスナップショット（CAS）から実体を書き出して復元します。",
            onClick: () => a.restoreFromCas(targetId),
          },
        ]
      : []),
    DIV,
    // ── 開く・編集系（よく使う）──
    {
      label: "既定のアプリで開く／編集",
      onClick: () => a.openDefault(targetId),
    },
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
    // v1.3.11 #13: オフィス系（docx/表計算/プレゼン）の読取専用プレビュー。
    ...(obj &&
    ["docx", "xlsx", "xls", "ods", "csv", "tsv", "pptx", "ppt", "doc", "odt", "odp"].includes(
      (obj.ext ?? "").toLowerCase()
    )
      ? [
          {
            label: ctx.expanded.has(targetId)
              ? "プレビューを閉じる"
              : "キャンバスでプレビュー",
            onClick: () =>
              ctx.expanded.has(targetId)
                ? a.collapse(targetId)
                : a.expandView(targetId),
          },
        ]
      : []),
    DIV,
    // ── 選択系 ──
    // v1.4.12: つながり（関係矢印の連結成分）をまるごと選択。Alt+クリックと同じ。
    { label: "つながりを選択", hint: "このカードと関係矢印でつながったカード全体を選択します（Alt+クリックと同じ）。", onClick: () => a.selectConnected(targetId) },
    // ── 一括（選択があるときだけ）──
    ...selItems,
    DIV,
    // ── 実身⇔仮身系 ──
    // v1.3.6 変更②: 仮身 →「実身にする」。指す内容を独立した内部実身にする
    // （元ファイルから独立。以後は仮身に戻す対象外）。「実身化（フォルダで表示）」は
    // v1.4.x ③ で「その他の機能」へ移動。
    ...(!committed
      ? [
          {
            label: "実身にする",
            hint: "この仮身が指す内容のコピーをプロジェクト内に確定し、独立した実身にします。元ファイルとの縁は切れ、以後この実身を編集できます。",
            onClick: () => a.makeReal(targetId),
          },
        ]
      : []),
    // v1.3.10「仮身を作成」: 常に「このカード自身」を指す窓を作る（window_of=このカードのid）。
    //   実身 → その実身を指す仮身。 仮身 → その仮身を指す仮身（仮身の仮身・チェーン）。
    //   実身を編集・リネームすると追従する（生きた窓）。「特別→仮身を複製」は廃止（窓対象を
    //   コピーして元を指す②の動作は混乱するため）。
    {
      label: "仮身を作成",
      hint: committed
        ? "この実身を指す仮身を作ります。実身を編集・リネームすると、この仮身にも反映されます（生きた窓）。"
        : "この仮身を指す仮身（仮身の仮身）を作ります。元の仮身が指す実身に追従します。",
      onClick: () => a.duplicate(targetId),
    },
    // v1.3.6 変更③: 危険・低頻度の操作はまとめて「特別 ...」へ退避。
    ...(specialItems.length
      ? [{ label: "特別 ...", onClick: () => a.openSubmenu(specialItems) }]
      : []),
    DIV,
    // ── メタ操作 ──
    // v1.3.10 補完: 表示名はトップに残す（実ファイル名の変更は v1.4.x ③ で「その他の機能」へ）。
    {
      label: "表示名を変更...",
      hint: committed
        ? "カードの表示名だけを変えます（実ファイル名は変わりません）。"
        : undefined,
      onClick: () => a.renameObject(targetId, obj?.label ?? ""),
    },
    // v1.4.x ③: 安全だが低頻度の操作をまとめて「その他の機能 ...」へ退避（項目削減）。
    ...(otherItems.length
      ? [{ label: "その他の機能 ...", onClick: () => a.openSubmenu(otherItems) }]
      : []),
    // 複数選択時の削除も「まとめて ...」サブメニュー側に集約（重複排除）。
    ...(!multi
      ? [{ label: "削除", danger: true, shortcut: "Delete", onClick: () => a.requestDelete(targetId) }]
      : []),
  ]);
}

// v1.3-②③: shape menu (shapes sit behind cards)
export function buildShapeMenu(
  shapeId: string,
  sh: ShapeInfo | undefined,
  // v1.3.7: 図形の上に「まだ選択されていない」カード/コメントがあるか。
  // 普通クリックは図形＋上のカードをまとめて選択するので、その流れで開いた
  // メニューでは全選択済み → 本項目は冗長になる。Shift+クリックで図形だけ選んだ
  // 等、未選択カードが残る時だけ「上のカードをまとめて選択」を出す。
  hasUnselectedAbove: boolean,
  // v1.4.x: 図形に乗っているカード/コメントが2枚以上あるか（格子整列が意味を持つ条件）。
  canArrangeAbove: boolean,
  a: MenuActions
): MenuItem[] {
  const kindName = sh?.kind === "ellipse" ? "楕円" : sh?.kind === "arrow" ? "矢印" : "角丸四角";
  return [
    { label: `図形「${sh?.label || kindName}」`, onClick: () => {}, disabled: true },
    ...(hasUnselectedAbove
      ? [{ label: "上のカードをまとめて選択", onClick: () => a.shapeClickSelect(shapeId) }]
      : []),
    ...(canArrangeAbove
      ? [{ label: "上のカードを格子状に整列", onClick: () => a.shapeAlignGrid(shapeId) }]
      : []),
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
    // v1.4.29 ③: 拡縮コマンド（拡大/縮小）は廃止し、「選択範囲にフィット」に一本化。
    // 選択中のカード・コメントを囲む大きさに図形を合わせる（「選択を図形で囲む」と同じ
    // 範囲計算）。ハンドルがカードの下に隠れて掴めなくても、メニューから畳み直せる。
    {
      label: "選択範囲にフィット",
      hint: "選択中のカード・コメントを囲む大きさに図形を合わせます（選択が無ければ図形の上のカードに合わせます）。",
      onClick: () => a.shapeFitToCards(shapeId),
    },
    { label: "名前を変更...", onClick: () => a.shapeLabel(shapeId, sh?.label ?? "") },
    { label: "最前面へ（図形内）", onClick: () => a.shapeZ(shapeId, true) },
    { label: "最背面へ（図形内）", onClick: () => a.shapeZ(shapeId, false) },
    { label: "図形を削除", danger: true, onClick: () => a.shapeDelete(shapeId) },
  ];
}

interface CanvasMenuCtx {
  selectionCount: number;
  /// v1.3.5: 選択中のコメント(付箋)数。整列・フォーカスはカード＋コメントで判定。
  noteCount: number;
  theme: string;
  alignItems: MenuItem[];
  bulkItems: MenuItem[];
  /// v1.5: ライブラリ登録済みフィルタ名（「ユーザーのフィルタ」として列挙）
  library: string[];
  /// v1.3.11: ライブラリ登録済みランチャー名（「ユーザーのランチャー」として列挙）
  launcherLibrary: string[];
  /// v1.4.9: dshow 音声デバイス名（「マイク録音」プリセットの選択肢）。
  audioDevices: string[];
  /// v1.3.4: メニュー説明ツールチップの現在のオン/オフ（トグル表示用）。
  menuHints: boolean;
  /// v1.4.29: カード説明ツールチップの現在のオン/オフ（トグル表示用）。
  cardHints: boolean;
  /// v1.4.25 ⑤: 図形グリッドスナップの現在のオン/オフ（トグル表示用）。
  shapeSnap: boolean;
}

// v1.4.16: UIカラーの連続比較用サブメニュー。選んでもメニューは閉じず（keepOpen）、
// ✓ を選択中テーマへ付け替えて開き直す。末尾の OK で閉じる。
function themeSubmenu(a: MenuActions, sel: string): MenuItem[] {
  // v1.4.30: グループごとに区切り線を入れて段落表示（基本/中間/配色/パステル/季節/アクセント）。
  return [
    ...THEME_GROUPS.flatMap((group, gi) => [
      ...(gi > 0 ? [DIV] : []),
      ...group.map(([key, name]) => ({
        label: (sel === key ? "✓ " : "　") + name,
        keepOpen: true,
        onClick: () => {
          a.setTheme(key);
          a.openSubmenu(themeSubmenu(a, key));
        },
      })),
    ]),
    DIV,
    { label: "OK（閉じる）", onClick: () => {} },
  ];
}

// v1.4.16: 明るさ・コントラストの連続操作用サブメニュー（keepOpen）。OK で閉じる。
function adjustSubmenu(a: MenuActions): MenuItem[] {
  const item = (
    label: string,
    kind: "brighter" | "darker" | "contrastUp" | "contrastDown" | "reset"
  ): MenuItem => ({ label, keepOpen: true, onClick: () => a.uiAdjust(kind) });
  return [
    item("明るく", "brighter"),
    item("暗く", "darker"),
    item("コントラストを強く", "contrastUp"),
    item("コントラストを弱く", "contrastDown"),
    item("調整をリセット", "reset"),
    DIV,
    { label: "OK（閉じる）", onClick: () => {} },
  ];
}

export function buildCanvasMenu(
  pos: { canvasX: number; canvasY: number },
  ctx: CanvasMenuCtx,
  a: MenuActions
): MenuItem[] {
  // v1.3.4: カテゴリ別に区切り線で分ける。選択系は選択があるときだけ先頭に出る。
  // v1.3.5: フォーカス・整列はカード＋コメントの合計で判定（コメントも整列対象）。
  //         一括操作（まとめて…）はカード専用なのでカード数だけで判定。
  const selTotal = ctx.selectionCount + ctx.noteCount;
  // v1.4.x ③: 「自動化カードを追加」のサブメニュー。1段階のフラットなリストにし、
  // ランチャー → フィルタ・処理カード → ユーザーのカード をセクション見出しで区切る。
  const automationAddSub: MenuItem[] = [
    // ── ランチャー（プログラム起動）──
    { label: "── ランチャー ──", onClick: () => {}, disabled: true },
    { label: "インストール済みアプリから選択...", onClick: () => a.addLauncher("installed", pos.canvasX, pos.canvasY) },
    { label: "一般プログラム（.exe を選択）...", onClick: () => a.addLauncher("program", pos.canvasX, pos.canvasY) },
    // シェルカード統合（2026-07-06）: 旧「コマンドプロンプト/PowerShell」ランチャー2項目と
    // ビルトインフィルタ内の「対話: …」2項目をこの1項目に統合（入口はここと矢印引き出しの2か所）。
    // シェル種別・コマンド・ウィンドウ（開いたまま/閉じる/出さない）は設定ダイアログで選ぶ。
    {
      label: "コマンドプロンプト / PowerShell ...",
      hint: "シェルでコマンドを実行するカードを追加します。ウィンドウを開いたまま対話・実行して閉じる・ウィンドウなしで結果をパネル表示、を設定で選べます。",
      onClick: () => a.addFilter("shell", pos.canvasX, pos.canvasY),
    },
    // ── フィルタ・処理カード（データ→フィルタ→結果のパイプ）──
    { label: "── フィルタ・処理カード ──", onClick: () => {}, disabled: true },
    { label: "ドロップポイント（データ入口・自動実行）", onClick: () => a.addFilter("drop_point", pos.canvasX, pos.canvasY) },
    // v1.3.11 #3: 内蔵フィルタは数が多いので「ビルトインフィルタ ...」サブメニューに残す。
    {
      label: "ビルトインフィルタ ...",
      onClick: () =>
        a.openSubmenu([
          // v1.4.1 AI（Ollama）。矢印引き出しメニューと項目を揃える。
          { label: "AI: 対話（チャットパネル）...", onClick: () => a.addFilter("ai_dialog", pos.canvasX, pos.canvasY) },
          { label: "AI: 変換（一発・MD出力）...", onClick: () => a.addFilter("ai_chat", pos.canvasX, pos.canvasY) },
          { label: "AI後段: コードブロック抽出（MD→ソース）...", onClick: () => a.addFilter("md_codeblock", pos.canvasX, pos.canvasY) },
          { label: "画像生成: Text-To-Image（テキスト→画像）...", onClick: () => a.addFilter("t2i", pos.canvasX, pos.canvasY) },
          { label: "テキスト: grep（正規表現で行を抽出）...", onClick: () => a.addFilter("text_grep", pos.canvasX, pos.canvasY) },
          { label: "テキスト: 置換（正規表現）...", onClick: () => a.addFilter("text_replace", pos.canvasX, pos.canvasY) },
          { label: "画像: リサイズ（幅指定）...", onClick: () => a.addFilter("img_resize", pos.canvasX, pos.canvasY) },
          { label: "画像: 形式変換...", onClick: () => a.addFilter("img_convert", pos.canvasX, pos.canvasY) },
          // v1.3.11 #4/#5: ffmpeg を使う音声/動画の簡易編集（要 ffmpeg）。
          { label: "音声: 長さでクリップ...", onClick: () => a.addFilter("audio_clip", pos.canvasX, pos.canvasY) },
          { label: "音声: ノーマライズ", onClick: () => a.addFilter("audio_norm", pos.canvasX, pos.canvasY) },
          // v1.4.9: マイク録音（録音→「停止」で確定。デバイスはサブメニューで選ぶ）。
          {
            label: "音声: マイク録音（録音→停止で確定）...",
            onClick: () =>
              a.openSubmenu(
                ctx.audioDevices.length
                  ? ctx.audioDevices.map((d) => ({
                      label: `🎙 ${d}`,
                      onClick: () => a.addFilter(`mic_record:${d}`, pos.canvasX, pos.canvasY),
                    }))
                  : [
                      {
                        label: "（マイクが見つかりません／ffmpeg 未検出）",
                        onClick: () => {},
                        disabled: true,
                      },
                    ]
              ),
          },
          // v1.4.8 ①: Whisper の入力アダプタ（m4a/動画/ステレオ → 16kHz mono WAV）と本体。
          { label: "音声: Whisper用WAVに変換（16kHz mono）", onClick: () => a.addFilter("to_wav16k", pos.canvasX, pos.canvasY) },
          { label: "音声: Whisper 文字起こし（→テキスト）...", onClick: () => a.addFilter("whisper", pos.canvasX, pos.canvasY) },
          { label: "動画: 長さでクリップ...", onClick: () => a.addFilter("video_clip", pos.canvasX, pos.canvasY) },
          // シェルカード統合（2026-07-06）: 旧「対話: コマンドプロンプト/PowerShell」2項目は
          // ランチャー節の「コマンドプロンプト / PowerShell ...」1項目に統合した。
        ]),
    },
    // v1.5: フィルタ/コマンド/ジェネレータの別は設定ダイアログの Type で選ぶ
    { label: "外部プログラム...", onClick: () => a.addFilter("external", pos.canvasX, pos.canvasY) },
    // ── ユーザーのカード（ライブラリ登録済み・登録があるときだけ）──
    ...(ctx.launcherLibrary.length || ctx.library.length
      ? [
          { label: "── ユーザーのカード ──", onClick: () => {}, disabled: true },
          ...ctx.launcherLibrary.map((name) => ({
            label: `🚀 ${name}`,
            onClick: () => a.addLibraryLauncher(name, pos.canvasX, pos.canvasY),
          })),
          ...ctx.library.map((name) => ({
            label: `⚗ ${name}`,
            onClick: () => a.addLibraryFilter(name, pos.canvasX, pos.canvasY),
          })),
          {
            label: "ライブラリから削除 ...",
            danger: true,
            onClick: () =>
              a.openSubmenu([
                ...ctx.launcherLibrary.map((name) => ({
                  label: `🚀 ${name} を削除`,
                  danger: true,
                  onClick: () => a.deleteLibraryLauncher(name),
                })),
                ...ctx.library.map((name) => ({
                  label: `⚗ ${name} を削除`,
                  danger: true,
                  onClick: () => a.deleteLibraryFilter(name),
                })),
              ]),
          },
        ]
      : []),
  ];
  // v1.4.29 ②: メニュー再整理。トップは「よく使う作成・配置＋探す・並べる」に絞り、低頻度の
  // 保守系（実身化・ゴミ箱・掃除）／プロジェクト作成／表示・設定を各1段のサブメニューへ畳む。
  // openSubmenu は「戻る」導線の無いその場差し替えなので、既に深い項目（自動化＝フィルタは
  // 2〜3段）はトップに残し、畳むのは中身が浅い（1段のリーフ）グループに限る。
  const cleanupSub: MenuItem[] = [
    {
      label: "すべて実身化（持ち出し）...",
      hint: "プロジェクト内のすべての仮身を実身化し、データをプロジェクトフォルダに書き出します。",
      onClick: () => a.commitAll(),
    },
    {
      label: "ゴミ箱を開く ...",
      hint: "削除したカード・コメント・図形を一覧して、復活または完全削除します。UNDO履歴を掃除した後でも、ここからならいつでも復活できます。",
      onClick: () => a.openTrash(),
    },
    {
      label: "履歴・キャッシュを掃除...",
      hint: "UNDO/REDO履歴と、サムネイル・一時プレビューのキャッシュを掃除します。キャッシュは必要時に自動再生成されます（ゴミ箱の中身は残り、「ゴミ箱を開く」から復活できます）。",
      onClick: () => a.cleanCaches(),
    },
    {
      label: "孤立データを削除（正規化）...",
      hint: "どのカードからも参照されなくなった実身データ（CAS）を削除して容量を回収します。まず件数・サイズを確認してから実行します（生きたカード・ゴミ箱・履歴は残ります）。先に「ゴミ箱を空にする」と回収量が増えます。",
      onClick: () => a.gcCas(),
    },
  ];
  const newProjectSub: MenuItem[] = [
    {
      label: "新規プロジェクトを内包...",
      hint: "新しいサブプロジェクトを作成し、このプロジェクトの中に実身として内包します。",
      onClick: () => a.newSubproject(false, pos.canvasX, pos.canvasY),
    },
    {
      label: "新規プロジェクトを分岐（フォーク）...",
      hint: "このプロジェクトをフォーク（複製）して、兄弟プロジェクトを新規作成します。元のプロジェクトはそのまま残ります。",
      onClick: () => a.newSubproject(true, pos.canvasX, pos.canvasY),
    },
    {
      label: "既存プロジェクトへのリンクを配置...",
      hint: "既にあるTexoSプロジェクトのフォルダを選び、そこへ移動できるリンクカードをこの場所に置きます。",
      onClick: () => a.linkProject(pos.canvasX, pos.canvasY),
    },
  ];
  const settingsSub: MenuItem[] = [
    {
      label: "UIカラー ...",
      hint: "配色テーマを選びます。選ぶたびにその場でプレビューされ、OKで確定します。",
      // v1.4.16: 連続で見比べられるよう、選んでもメニューは開いたまま（keepOpen）。
      onClick: () => a.openSubmenu(themeSubmenu(a, ctx.theme)),
    },
    {
      label: "明るさ・コントラスト ...",
      hint: "画面全体をワンタッチで明るく／暗く、コントラストを強く／弱く調整します（テーマはそのまま）。連続で操作でき、OKで閉じます。",
      onClick: () => a.openSubmenu(adjustSubmenu(a)),
    },
    {
      // v1.3.4: メニュー項目の説明ツールチップを出すか。慣れたらオフにできる。
      label: (ctx.menuHints ? "✓ " : "　") + "メニューのツールチップを表示",
      hint: "各メニュー項目にカーソルを合わせたとき、このような説明を表示します。オフにすると非表示になります。",
      onClick: () => a.toggleMenuHints(),
    },
    {
      // v1.4.29: カードのホバー説明（種別・メモ・使い方・状態）を出すか。メニューとは別系統。
      label: (ctx.cardHints ? "✓ " : "　") + "カードのツールチップを表示",
      hint: "カードにカーソルを合わせたときの説明（種別・メモ・使い方や仮身/実身の状態）を表示します。オフにすると非表示になります。",
      onClick: () => a.toggleCardHints(),
    },
    {
      label: "ショートカット一覧（?）",
      hint: "キーボードショートカットの一覧を表示します（? または F1 キーでも開閉できます）。",
      onClick: () => a.showHelp(),
    },
  ];
  return compact([
    // ── 選択系（選択があるときだけ）──
    ...(selTotal
      ? [{
          label: "選択範囲にフォーカス",
          shortcut: "F",
          hint: "選択したカード・コメントが画面いっぱいに収まるよう、ズームと位置を合わせます（キーボードの F でも実行できます）。",
          onClick: () => a.focusSelection(),
        }]
      : []),
    // v1.4.23 ③: 選択部を囲む図形（角丸四角）を作って選択状態で渡す。囲んでから
    // 図形ドラッグ＝分離モードで、混在する密集地から選択だけを新グループにできる。
    ...(selTotal
      ? [{
          label: "選択を図形で囲む",
          hint: "選択したカード・コメントを囲む角丸四角の図形を作り、選択状態のまま渡します。図形をドラッグすると中身ごと動くので、密集した場所から思考のまとまりを切り出せます。",
          onClick: () => a.shapeAroundSelection(),
        }]
      : []),
    ...(selTotal >= 2
      ? [{
          label: "選択を整列 ...",
          hint: "選択したカード・コメントを、横一列／縦一列／格子状／均等配置に並べ替えます。",
          onClick: () => a.openSubmenu(ctx.alignItems),
        }]
      : []),
    ...(ctx.selectionCount >= 2
      ? [{
          label: "選択カードをまとめて ...",
          hint: "選択したカードをまとめて実身化・仮身化・クリップボードにコピー・別プロジェクトへ送付・削除します。",
          onClick: () => a.openSubmenu(ctx.bulkItems),
        }]
      : []),
    DIV,
    // ── 作成・配置系（よく使う。トップに残す）──
    {
      label: "クリップボードから貼り付け",
      hint: "OSのクリップボードの内容（画像・テキスト・URL）をカードとして取り込みます。",
      shortcut: "Ctrl+V",
      onClick: () => a.paste(pos.canvasX, pos.canvasY),
    },
    {
      label: "ファイルを配置...",
      hint: "PC上のファイルを選び、仮身（元ファイルを指す参照カード）としてこの場所に置きます。ドラッグ＆ドロップでも配置できます。",
      onClick: () => a.contextPlace(pos.canvasX, pos.canvasY),
    },
    {
      label: "コメントを追加...",
      hint: "自由に書ける付箋（コメントカード）をこの場所に作ります。色・文字サイズ・見出し行は右クリックから変えられます。",
      onClick: () => a.addNote(pos.canvasX, pos.canvasY),
    },
    {
      // v1.3.11 #8/#9: プロジェクトフォルダに新規ファイルを作って実身として置く。
      label: "新規ファイルを作成...",
      hint: "プロジェクトフォルダに新しいファイル（拡張子つき）を作り、実身としてキャンバスに置きます。開くアプリはOSの関連付けに従います。",
      onClick: () => a.newFile(pos.canvasX, pos.canvasY),
    },
    {
      label: "URLリンクを追加...",
      hint: "Webページなどのアドレス（URL）を開けるリンクカードをこの場所に作ります。",
      onClick: () => a.addUrl(pos.canvasX, pos.canvasY),
    },
    {
      label: "図形を追加 ...",
      hint: "角丸四角・楕円・太矢印の図形をカードの背面に置きます。思考のグループ分けや、選択範囲を囲む土台に使います。",
      onClick: () =>
        a.openSubmenu([
          { label: "角丸四角", hint: "角丸四角の図形を背面に追加します（思考のまとまりを囲む土台に）。", onClick: () => a.shapeAdd("rect", pos.canvasX, pos.canvasY) },
          { label: "楕円", hint: "楕円の図形を背面に追加します。", onClick: () => a.shapeAdd("ellipse", pos.canvasX, pos.canvasY) },
          { label: "太矢印", hint: "面のある太い矢印を背面に追加します（向きは追加後、右クリック→向きを変更）。", onClick: () => a.shapeAdd("arrow", pos.canvasX, pos.canvasY) },
          DIV,
          // v1.4.25 ⑤: 見えない格子（カード≒横4×縦3単位の38px正方）への吸着。
          // オンの間、図形の作成・移動・リサイズの対角が格子に乗る（カードは自由なまま）。
          {
            label: (ctx.shapeSnap ? "✓ " : "　") + "グリッドにスナップ",
            hint: "オンの間、図形の作成・移動・サイズ変更が見えない格子（38px）に吸着します。カードの配置は自由なままです。",
            onClick: () => a.toggleShapeSnap(),
          },
          {
            label: "全図形を今すぐグリッドへ",
            hint: "このプロジェクトの全図形の位置とサイズを格子に揃えます（Ctrl+Z で一括で戻せます）。スナップがオフでも使えます。",
            onClick: () => a.snapAllShapes(),
          },
        ]),
    },
    // v1.4.x ③: 自動化系（ランチャー／フィルタ／ユーザーのカード）は既に2〜3段深いので
    // トップに残す（追加サブへ畳むと最悪パスが4段になり「戻る」不在では使いにくい）。
    {
      label: "自動化カードを追加 ...",
      hint: "プログラムを起動するランチャーカードや、データを処理するフィルタカードを追加します。",
      onClick: () => a.openSubmenu(automationAddSub),
    },
    DIV,
    // ── 探す・並べる（よく使う）＋畳んだ低頻度グループ ──
    {
      label: "カードを検索 ...",
      shortcut: "Ctrl+F",
      hint: "ラベル・コメント本文・メモ・拡張子でカードを検索し、クリックでフォーカスします（複数一致はまとめて選択も可）。",
      onClick: () => a.openSearch(),
    },
    {
      label: "自動レイアウト（重なり解消）",
      hint: "重なり合ったカードを自動でずらして、重なりを解消します（Ctrl+Z で元に戻せます）。",
      onClick: () => a.autoLayout(),
    },
    {
      label: "整理・書き出し ...",
      hint: "すべて実身化（持ち出し）、ゴミ箱、履歴・キャッシュの掃除、孤立データの削除などの保守操作。",
      onClick: () => a.openSubmenu(cleanupSub),
    },
    {
      label: "プロジェクトを作成 ...",
      hint: "サブプロジェクトの内包・分岐（フォーク）や、既存プロジェクトへのリンク配置。",
      onClick: () => a.openSubmenu(newProjectSub),
    },
    {
      label: "プロジェクトを切り替え...",
      hint: "別のTexoSプロジェクトを開きます（現在のプロジェクトは自動で保存されます）。",
      onClick: () => a.switchProject(),
    },
    DIV,
    // ── 表示・設定（低頻度・畳む）──
    {
      label: "表示・設定 ...",
      hint: "UIカラー、明るさ・コントラスト、メニューの説明表示、ショートカット一覧。",
      onClick: () => a.openSubmenu(settingsSub),
    },
  ]);
}
