import { useEffect, useState, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { getVersion } from "@tauri-apps/api/app";
import { listen } from "@tauri-apps/api/event";
import { Canvas, screenToCanvas, type Transform, type CanvasApi } from "./canvas/Canvas";
import { LinkLayer, center as nodeCenter, edgePoint as nodeEdgePoint, type NodeBox, type RelationInfo } from "./canvas/LinkLayer";
import { ObjectCard, CARD_W, CARD_H } from "./objects/ObjectCard";
import { NoteCard } from "./objects/NoteCard";
import { ShapeCard, type ShapeInfo } from "./objects/ShapeCard";
import { ContextMenu, type MenuItem } from "./ui/ContextMenu";
import { PromptDialog } from "./ui/PromptDialog";
import { FilterDialog } from "./ui/FilterDialog";
import { BuiltinFilterDialog, hasBuiltinSchema } from "./ui/BuiltinFilterDialog";
import { ShellSettingsDialog } from "./ui/ShellSettingsDialog";
import { ShellOutputDialog } from "./ui/ShellOutputDialog";
import { TraceDialog, type CardTrace } from "./ui/TraceDialog";
import { AiSettingsDialog } from "./ui/AiSettingsDialog";
import { WhisperSetupDialog, type WhisperStatus } from "./ui/WhisperSetupDialog";
import { T2iSettingsDialog } from "./ui/T2iSettingsDialog";
import { T2iSetupDialog, type T2iStatus } from "./ui/T2iSetupDialog";
import { SearxngSetupDialog, type SearxngStatus } from "./ui/SearxngSetupDialog";

// v2.0 t23d: t23d_status（runtime/t23d.rs）の返却形。専用セットアップ画面は持たず、
// エンジン未設定時のパス入力（プロンプト）だけで足りるためここに置く。
interface T23dStatus {
  engine_set: boolean;
  engine_ok: boolean;
  engine_path: string;
  engine_error: string;
  server_running: boolean;
  gpu: boolean;
  models: string[];
  known: { name: string; label: string }[];
  current_model: string;
}
import { JobsPanel, type JobRow } from "./ui/JobsPanel";
import { InputOrderDialog } from "./ui/InputOrderDialog";

// v1.4.8 ①: Whisper 文字起こしカードの spec（外部コマンドフィルタ）。
// whisper-cli は app_data 共有・絶対パス参照。-of {out_noext} が <base>.txt を書き {out} と一致。
// 言語は日本語既定（-l ja）。前段に「Whisper用WAVに変換」を繋げば m4a/動画も扱える。
/// v1.8: 生 JSON を読める形（2スペース整形）にする。壊れた JSON はそのまま返す
/// —— 直しに来た人から元の文字列を取り上げないため。
function prettyJson(s: string): string {
  try {
    return JSON.stringify(JSON.parse(s), null, 2);
  } catch {
    return s;
  }
}

function whisperSpec(st: WhisperStatus) {
  return {
    name: "whisper-cli.exe",
    path: st.cli_path,
    type: "command",
    cmd: `-m "${st.model_path}" -f {in} -otxt -of {out_noext} -l ja`,
    output_ext: "txt",
    accept_mime: "audio/*",
    timeout_secs: 1800,
    note: `Whisper 文字起こし（モデル: ${st.current_model}）。前段に「Whisper用WAVに変換」を繋ぐと m4a・動画も扱えます。`,
  };
}
import { AppPicker } from "./ui/AppPicker";
import { ConfirmDialog } from "./ui/ConfirmDialog";
import { TrashDialog } from "./ui/TrashDialog";
import { SearchDialog } from "./ui/SearchDialog";
import { ShortcutHelp } from "./ui/ShortcutHelp";
import { ProjectChooser, type ProjectInfo } from "./ui/ProjectChooser";
import {
  PROJECT_MIME,
  filterOutputMime,
  isTextMime,
  lostOf,
  lostRealOf,
  lostFolderOf,
  lostProjectOf,
  type InnerFolder,
  launcherPathOf,
  mimeMatches,
  noteSize,
  shapeCenterOn,
  webUpdateOf,
  runAlertOf,
  type NoteInfo,
  type VirtualBodyInfo,
} from "./model";
import { useUndo, type UndoAction } from "./hooks/useUndo";
import { useSelection } from "./hooks/useSelection";
import {
  buildAlignItems,
  buildAutomationAddSub,
  buildBulkItems,
  buildCanvasMenu,
  buildNewFileSub,
  buildNoteMenu,
  buildObjectMenu,
  buildShapeMenu,
  type MenuActions,
} from "./ui/menuBuilder";
import "./App.css";

interface CommitAllWarning {
  label: string;
  kind: string; // "url" | "external_project" | "program"
  detail: string;
}

interface CommitAllReport {
  committed: string[];
  already: number;
  warnings: CommitAllWarning[];
  errors: string[];
}

interface LinkDrag {
  fromId: string;
  x1: number;
  y1: number;
  x2: number;
  y2: number;
}

// v1.4.25 ⑤: 図形グリッドスナップの格子（px）。カード(150×115)がほぼ正確に
// 「横4×縦3単位」に収まる正方グリッド（150/4=37.5, 115/3≈38.3 → 38）。
// 図形の対角（左上・右下）をこの格子に吸着させる。カードの配置は自由なまま。
export const SHAPE_GRID = 38;

function App() {
  const [project, setProject] = useState<ProjectInfo | null>(null);
  const currentProjectRef = useRef<ProjectInfo | null>(null);
  currentProjectRef.current = project;
  const [projectLoaded, setProjectLoaded] = useState(false);
  const [showChooser, setShowChooser] = useState(false);
  const [objects, setObjects] = useState<VirtualBodyInfo[]>([]);
  const [notes, setNotes] = useState<NoteInfo[]>([]);
  const [shapes, setShapes] = useState<ShapeInfo[]>([]);
  // v1.3 メモ①: カンバス左下に表示するアプリバージョン。
  const [appVersion, setAppVersion] = useState("");
  const [theme, setTheme] = useState<string>(() => localStorage.getItem("texos-theme") ?? "midnight");
  // v1.4.16 遊び心: 画面全体の明るさ・コントラストのお手軽調整（filter方式・段階値）。
  const [uiAdjust, setUiAdjust] = useState<{ bright: number; contrast: number }>(() => {
    try {
      const s = JSON.parse(localStorage.getItem("texos-ui-adjust") ?? "");
      if (s && typeof s.bright === "number" && typeof s.contrast === "number") return s;
    } catch {
      /* ignore */
    }
    return { bright: 0, contrast: 0 };
  });
  // v1.3.4: メニュー項目の説明ツールチップを出すか（既定オン、設定で切替・永続化）。
  const [showMenuHints, setShowMenuHints] = useState<boolean>(
    () => localStorage.getItem("texos-menu-hints") !== "0"
  );
  // v1.4.29: カードのホバー説明ツールチップ（mime/メモ/使い方・状態）を出すか（既定オン・永続化）。
  const [showCardHints, setShowCardHints] = useState<boolean>(
    () => localStorage.getItem("texos-card-hints") !== "0"
  );
  // v1.4.25 ⑤: 図形のグリッドスナップ（既定オフ、切替・永続化）。オンの間、図形の
  // 作成・移動・リサイズが SHAPE_GRID の見えない格子に吸着する（カードは対象外＝自由）。
  const [shapeSnap, setShapeSnap] = useState<boolean>(
    () => localStorage.getItem("texos-shape-snap") === "1"
  );
  // v1.5.4 WS-P2: リクエスト矢印（req・opカードの発行線）の表示（既定オン・永続化）。
  // 非表示でも発行は動き続ける（見た目のトグルだけ）。
  const [showReqArrows, setShowReqArrows] = useState<boolean>(
    () => localStorage.getItem("texos-show-req") !== "0"
  );
  // v1.8.7 実行トレース: デバッグモード（実行ログに入出力の中身・stderr まで記録。既定オフ・永続化）。
  // バックエンドはジョブ開始時点の値を使うので、変更のたびに送っておく。
  const [debugTrace, setDebugTrace] = useState<boolean>(
    () => localStorage.getItem("texos-debug-trace") === "1"
  );
  useEffect(() => {
    void invoke("set_trace_detail", { on: debugTrace }).catch(() => {});
  }, [debugTrace]);

  // v1.8.7: カードの実行ログダイアログ（trace=null は「まだ記録なし」）。
  const [traceDlg, setTraceDlg] = useState<{ label: string; trace: CardTrace | null } | null>(null);
  const [newNoteId, setNewNoteId] = useState<string | null>(null);
  const [relations, setRelations] = useState<RelationInfo[]>([]);
  const [linkDrag, setLinkDrag] = useState<LinkDrag | null>(null);
  const [menu, setMenu] = useState<{ x: number; y: number; items: MenuItem[] } | null>(null);
  const [prompt, setPrompt] = useState<{
    title: string;
    initial: string;
    allowEmpty?: boolean;
    // メニュー再編（2026-07-12）: 2入力欄プロンプト（新規ファイル「その他の拡張子」= 名前＋拡張子）。
    label?: string;
    field2?: { label: string; initial?: string };
    /// v1.8: 複数行入力（生JSONの編集など）。Enter は改行・Ctrl+Enter で確定。
    multiline?: boolean;
    onConfirm: (v: string, v2?: string) => void;
  } | null>(null);
  // v1.5.2 WS-C 背景ジョブパネル: 実行中ジョブの一覧（job:started/progress/done/failed で
  // 更新・マウント時に list_jobs で復元）。旧「フロー実行中…」1行パネルの置き換え。
  const [jobs, setJobs] = useState<JobRow[]>([]);
  // v1.4.10 二度押し抑止の WS-C 版: 同一カードの▶連打を同期的に弾く（並行解禁後は
  // カード単位。バックエンド register_flow も同一終端カードを拒否するが、ここで先に
  // 止めてエラーダイアログを出さない）。
  const runningCardsRef = useRef<Set<string>>(new Set());
  // v1.5.2 WS-C: 実行完了時に「起動時と同じプロジェクトか」を判定するためのミラー
  // （切替後に完了したフローの結果は起動元プロジェクトにあり、現キャンバスには無い）。
  const projectPathRef = useRef<string | null>(null);
  // v1.5.3 WS-P1: 入力の順序ダイアログ（カード右クリック「入力の順序 ...」）。
  const [inputOrderDlg, setInputOrderDlg] = useState<{ id: string; label: string } | null>(null);
  // v1.5 外部プログラムフィルタの設定ダイアログ（作成 or 既存カードの編集）
  const [filterDlg, setFilterDlg] = useState<{
    x: number;
    y: number;
    edit?: { id: string; label: string; spec: string };
  } | null>(null);
  // v1.4.29 ⑤: ビルトインフィルタ専用の設定ダイアログ（生JSON編集の置き換え）。
  // 種別ごとのスキーマからフォームを自動生成する（text_grep / text_replace /
  // img_resize / img_convert / md_codeblock）。id あり=既存編集。
  // v1.5.4 WS-P2: op カード（op_trigger/op_timer/op_watch）もこのダイアログで作成・編集
  // する（id あり=既存編集 / create あり=新規作成）。
  const [builtinDlg, setBuiltinDlg] = useState<{
    id: string | null;
    builtin: string;
    label: string;
    spec?: string;
    create?: { x: number; y: number; linkFrom?: string };
  } | null>(null);
  // シェルカード統合（2026-07-06）: コマンドプロンプト/PowerShell カード専用の設定
  // ダイアログ（作成・編集兼用）。id あり=既存編集 / create あり=新規作成。
  const [shellDlg, setShellDlg] = useState<{
    id: string | null;
    label: string;
    spec?: string;
    create?: { x: number; y: number; linkFrom?: string };
  } | null>(null);
  // シェルカード統合: window:"hidden" 実行の結果パネル（stdout/stderr＋終了コード）。
  const [shellOut, setShellOut] = useState<{
    label: string;
    exitCode: number | null;
    output: string;
    truncated: boolean;
  } | null>(null);
  // v1.4.1 AIカード専用の設定ダイアログ（生JSON編集の置き換え／新規作成も兼ねる）。
  // id あり=既存編集 / create あり=新規作成（保存時に create_filter＋矢印接続）。
  const [aiDlg, setAiDlg] = useState<{
    id: string | null;
    label: string;
    spec: string;
    create?: { x: number; y: number; linkFrom?: string };
  } | null>(null);
  // v1.4.8 ①: Whisper セットアップダイアログ（未導入時に開く。作成位置を保持）。
  const [whisperDlg, setWhisperDlg] = useState<{
    x: number;
    y: number;
    linkFrom?: string;
  } | null>(null);
  // v1.5 Text-To-Image: パラメータ設定ダイアログ（新規作成 create / 既存編集 id）。
  const [t2iDlg, setT2iDlg] = useState<{
    id: string | null;
    label: string;
    spec: string;
    models: string[];
    known: string[];
    // モディファイア用アセット（LoRA/埋め込み/VAE のセレクト候補）。
    assets: { loras: string[]; embeddings: string[]; vae: string[] };
    create?: { x: number; y: number; linkFrom?: string };
  } | null>(null);
  // v1.5 Text-To-Image: セットアップ（依存確認・モデルDL・サーバー起動）。完了で設定へ。
  const [t2iSetup, setT2iSetup] = useState<{
    x: number;
    y: number;
    linkFrom?: string;
  } | null>(null);
  // Web検索: 検索エンジン（SearXNG）の導入・管理ダイアログ。manage=false はカード作成の途中
  // （未導入だった）＝導入が済んだらそのまま web_search カード作成へ進む。
  const [searxngSetup, setSearxngSetup] = useState<{
    x: number;
    y: number;
    linkFrom?: string;
    manage?: boolean;
  } | null>(null);
  // v1.4.9: 録音プリセット用の dshow 音声デバイス名（メニューは同期構築なので state に持つ）。
  const [audioDevices, setAudioDevices] = useState<string[]>([]);
  const audioDevicesRef = useRef<string[]>([]);
  audioDevicesRef.current = audioDevices;
  useEffect(() => {
    invoke<string[]>("list_audio_devices")
      .then(setAudioDevices)
      .catch(() => setAudioDevices([]));
  }, []);
  // v1.5 フィルタライブラリ（アプリ共通・プロジェクト横断）。メニュー構築が同期なので state に持つ。
  const [filterLibrary, setFilterLibrary] = useState<{ name: string; spec: string }[]>([]);
  const filterLibraryRef = useRef<{ name: string; spec: string }[]>([]);
  filterLibraryRef.current = filterLibrary;
  const reloadFilterLibrary = useCallback(async () => {
    try {
      setFilterLibrary(await invoke<{ name: string; spec: string }[]>("list_filter_library"));
    } catch {
      setFilterLibrary([]);
    }
  }, []);
  useEffect(() => {
    void reloadFilterLibrary();
  }, [reloadFilterLibrary]);
  // v1.3.11 ランチャーライブラリ（一度登録→リスト）
  const [launcherLibrary, setLauncherLibrary] = useState<{ name: string; spec: string }[]>([]);
  const launcherLibraryRef = useRef<{ name: string; spec: string }[]>([]);
  launcherLibraryRef.current = launcherLibrary;
  const reloadLauncherLibrary = useCallback(async () => {
    try {
      setLauncherLibrary(await invoke<{ name: string; spec: string }[]>("list_launcher_library"));
    } catch {
      setLauncherLibrary([]);
    }
  }, []);
  useEffect(() => {
    void reloadLauncherLibrary();
  }, [reloadLauncherLibrary]);
  // v1.5.8 WS-M M2: モジュールライブラリ（AppData/modules・共有とモジュール設計 §10）。
  const [moduleLibrary, setModuleLibrary] = useState<
    { name: string; created_at: number; cards: number; total_blob_bytes: number }[]
  >([]);
  // メニュー再編（2026-07-12）: 矢印引き出しメニュー（handleStartLink 内の同期構築）からも
  // 「自動化カードを追加」を出すため ref でも持つ（filterLibraryRef 等と同じ流儀）。
  const moduleLibraryRef = useRef<{ name: string }[]>([]);
  moduleLibraryRef.current = moduleLibrary;
  const reloadModuleLibrary = useCallback(async () => {
    try {
      setModuleLibrary(
        await invoke<{ name: string; created_at: number; cards: number; total_blob_bytes: number }[]>(
          "list_module_library"
        )
      );
    } catch {
      setModuleLibrary([]);
    }
  }, []);
  useEffect(() => {
    void reloadModuleLibrary();
  }, [reloadModuleLibrary]);
  // v1.3.11 #A: インストール済みアプリのピッカー（Start Menu 走査）。開いている間だけ
  // 配置先座標と linkFrom を保持する。
  const [appPicker, setAppPicker] = useState<{ x: number; y: number; linkFrom?: string } | null>(null);
  // v1.4.12: キーボードショートカットのヘルプ・オーバーレイ（? / F1 で開閉）。
  const [helpOpen, setHelpOpen] = useState(false);
  // v1.4.19: ゴミ箱を開く（削除済みの一覧・復活・完全削除）。
  const [trashOpen, setTrashOpen] = useState(false);
  // v1.4.19: カードを検索（Ctrl+F・右側フローティング）。
  const [searchOpen, setSearchOpen] = useState(false);
  const [confirm, setConfirm] = useState<{
    title: string;
    message?: string;
    warning?: string;
    okLabel?: string;
    cancelLabel?: string;
    onConfirm: () => void;
  } | null>(null);
  // Objects newly committed / changed during this session (緑シグナル).
  // 閲覧のみでは入れない — 仮身は琥珀のまま、実身はグレーのまま (v1.2 仕様).
  const [touched, setTouched] = useState<Set<string>>(new Set());
  // Objects whose inline text editor is currently expanded.
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const expandedRef = useRef(expanded);
  expandedRef.current = expanded;
  // Saved canvas view (pan/zoom) for the current project, applied atomically.
  const [view, setView] = useState<{ key: string; transform: Transform } | null>(null);
  const saveTimer = useRef<number | null>(null);

  const transformRef = useRef<Transform>({ x: 0, y: 0, scale: 1 });
  const objectsRef = useRef<VirtualBodyInfo[]>([]);
  objectsRef.current = objects;
  // v1.3.11 ランチャー: 起動後に戻ったとき新規保存ファイルを回収する位置（1回だけ）。
  const pendingLauncherScanRef = useRef<{ x: number; y: number } | null>(null);
  const shapesRef = useRef<ShapeInfo[]>([]);
  shapesRef.current = shapes;
  const notesRef = useRef<NoteInfo[]>([]);
  notesRef.current = notes;
  const relationsRef = useRef<RelationInfo[]>([]);
  relationsRef.current = relations;
  // Actual rendered note sizes (ResizeObserver) — arrows/selection use these;
  // noteSize() stays as the pre-measurement fallback only.
  const [noteSizes, setNoteSizes] = useState<Map<string, { w: number; h: number }>>(new Map());
  const noteSizesRef = useRef(noteSizes);
  noteSizesRef.current = noteSizes;
  const measuredNoteSize = useCallback(
    (n: NoteInfo) => noteSizesRef.current.get(n.id) ?? noteSize(n),
    []
  );
  const handleNoteMeasure = useCallback((id: string, w: number, h: number) => {
    setNoteSizes((prev) => {
      const cur = prev.get(id);
      if (cur && Math.abs(cur.w - w) < 1 && Math.abs(cur.h - h) < 1) return prev;
      const next = new Map(prev);
      next.set(id, { w, h });
      return next;
    });
  }, []);
  // Committed-file mtimes at first sight this session; a later increase means
  // the file was changed during this session → green dot.
  const mtimeBaseRef = useRef<Map<string, number>>(new Map());
  // Canvas imperative API (fitTo) — 選択範囲にフォーカス等で使う。
  const canvasApi = useRef<CanvasApi | null>(null);
  // v1.4.22 ②: プロジェクト間コピー/移動の控え。送り先で新規採番されたIDを覚えておき、
  // このセッション中に送り先プロジェクトを開いたら選択状態で迎える（重なりの分離が容易に）。
  const pendingSelectRef = useRef<{
    path: string;
    cards: string[];
    notes: string[];
    shapes: string[];
  } | null>(null);
  // v1.4 ドロップポイント: 先に定義される handleDropFiles から、後方で定義される
  // パイプライン実行関数を呼ぶための間接参照（TDZ回避。refミラーの既存イディオム）。
  const runDownstreamFlowsRef = useRef<(startId: string) => Promise<void>>(async () => {});

  // 選択状態（カード・コメント・図形）— useSelection に分離（v1.3.2 リファクタ）。
  const {
    selected,
    setSelected,
    selectedRef,
    selectedNotes,
    setSelectedNotes,
    selectedNotesRef,
    selectedShapes,
    setSelectedShapes,
    selectedShapesRef,
    handleSelectRect,
    handleCardToggleSelect,
    handleNoteToggleSelect,
    handleShapeToggleSelect,
    handleClearSelection,
    handleCardSelectOnly,
    handleNoteSelectOnly,
  } = useSelection({ objectsRef, notesRef, shapesRef, measuredNoteSize });

  const refresh = useCallback(async () => {
    try {
      const [items, rels, ns, shps] = await Promise.all([
        invoke<VirtualBodyInfo[]>("list_objects"),
        invoke<RelationInfo[]>("list_relations"),
        invoke<NoteInfo[]>("list_notes"),
        invoke<ShapeInfo[]>("list_shapes"),
      ]);
      // Session-change detection against the first-seen mtime.
      const changed: string[] = [];
      for (const o of items) {
        if (o.cur_mtime != null) {
          const base = mtimeBaseRef.current.get(o.id);
          if (base == null) mtimeBaseRef.current.set(o.id, o.cur_mtime);
          else if (o.cur_mtime > base) changed.push(o.id);
        } else {
          mtimeBaseRef.current.delete(o.id); // uncommitted again (仮身に戻った)
        }
      }
      if (changed.length) {
        setTouched((prev) => {
          const next = new Set(prev);
          changed.forEach((id) => next.add(id));
          return next;
        });
      }
      setObjects(items);
      setRelations(rels);
      setNotes(ns);
      setShapes(shps);
      // Restore expansion state from the saved per-object flags.
      setExpanded(new Set(items.filter((o) => o.expanded).map((o) => o.id)));
    } catch (e) {
      console.error("refresh failed", e);
    }
  }, []);

  // v1.4.1 チャット1ターン完了でキャンバスを更新（MD実身ログカードの誕生/更新を反映）。
  useEffect(() => {
    let alive = true;
    // v1.8.9 抜粋の書き出し（新しい MD カード）も同じく反映する。
    const uns: (() => void)[] = [];
    for (const ev of ["ai-chat-done", "ai-chat-exported"]) {
      listen(ev, () => {
        void refresh();
      }).then((u) => {
        if (alive) uns.push(u);
        else u();
      });
    }
    return () => {
      alive = false;
      uns.forEach((u) => u());
    };
  }, [refresh]);

  // v1.5.2 WS-C 背景ジョブパネル: job:* イベントで一覧を維持する。started で行を追加
  // （flow は card から録音対応/停止コマンドを spec で引く）、progress で note/percent 更新、
  // done/failed で行を除去。マウント時に list_jobs で復元（イベント欠落・リロード保険）。
  useEffect(() => {
    let alive = true;
    const uns: (() => void)[] = [];
    const recordOf = (card?: string) => {
      const obj = card ? objectsRef.current.find((o) => o.id === card) : undefined;
      if (!obj?.filter_ref) return { record: false, stopCmd: "q" };
      try {
        const s = JSON.parse(obj.filter_ref);
        return {
          record: !!s.record,
          stopCmd: typeof s.stop_cmd === "string" && s.stop_cmd ? s.stop_cmd : "q",
        };
      } catch {
        return { record: false, stopCmd: "q" };
      }
    };
    void listen<{ job_id: number; kind: string; label: string; card?: string }>(
      "job:started",
      (e) => {
        const { record, stopCmd } = recordOf(e.payload.card);
        setJobs((prev) => [
          ...prev.filter((j) => j.id !== e.payload.job_id),
          {
            id: e.payload.job_id,
            kind: e.payload.kind,
            label: e.payload.label,
            startedAt: Date.now(),
            record,
            stopCmd,
          },
        ]);
      }
    ).then((u) => (alive ? uns.push(u) : u()));
    void listen<{ job_id: number; note?: string; percent?: number }>("job:progress", (e) => {
      setJobs((prev) =>
        prev.map((j) =>
          j.id === e.payload.job_id
            ? { ...j, note: e.payload.note ?? j.note, percent: e.payload.percent ?? j.percent }
            : j
        )
      );
    }).then((u) => (alive ? uns.push(u) : u()));
    for (const ev of ["job:done", "job:failed"]) {
      void listen<{ job_id: number }>(ev, (e) => {
        setJobs((prev) => prev.filter((j) => j.id !== e.payload.job_id));
        // v1.5.4 WS-P2: タイマ/ウォッチャ発の背景フローは完了ダイアログを出さないので、
        // 完了時にキャンバスを更新して結果カードを見せる（手動実行の refresh と重なっても無害）。
        // v1.8.7: 失敗（job:failed）でも読み直す — 終端カードに付いたエラーの色を見せるため。
        void refresh();
      }).then((u) => (alive ? uns.push(u) : u()));
    }
    // 復元（リロード時など。started_at はバックエンドの unix ms をそのまま使う）。
    void invoke<{ id: number; kind: string; label: string; started_at: number; card?: string }[]>(
      "list_jobs"
    )
      .then((rows) => {
        if (!alive || rows.length === 0) return;
        setJobs((prev) => {
          const known = new Set(prev.map((j) => j.id));
          const add = rows
            .filter((r) => !known.has(r.id))
            .map((r) => {
              const { record, stopCmd } = recordOf(r.card ?? undefined);
              return {
                id: r.id,
                kind: r.kind,
                label: r.label,
                startedAt: r.started_at,
                record,
                stopCmd,
              };
            });
          return add.length ? [...prev, ...add] : prev;
        });
      })
      .catch(() => {});
    return () => {
      alive = false;
      for (const u of uns) u();
    };
  }, []);

  // v1.5.2 WS-C: 現在のプロジェクトパスのミラー（実行完了時の一致判定用）。
  useEffect(() => {
    projectPathRef.current = project?.path ?? null;
  }, [project]);

  // v1.3 メモ② UNDO/REDO — useUndo に分離（v1.3.2 リファクタ）。
  // Ctrl+Z / Ctrl+Shift+Z / Ctrl+Y のキーバインドもフック内。
  const {
    pushUndo,
    applyObjectMoves,
    applyNoteMoves,
    applyShapeGeoms,
    resetStacks,
    loadStacks,
    clearHistory,
  } = useUndo({ objectsRef, notesRef, shapesRef, setObjects, setNotes, setShapes, refresh });

  // Windows: swap in the custom OLE drop target (files + URL drops).
  useEffect(() => {
    invoke("install_drop_target").catch((e) => console.error("install_drop_target failed", e));
  }, []);

  // v1.3 メモ①: tauri.conf.json のバージョンをそのまま表示する。
  useEffect(() => {
    getVersion().then(setAppVersion).catch(() => {});
  }, []);

  // On launch, restore the last project (backend reopens it automatically).
  useEffect(() => {
    invoke<ProjectInfo | null>("get_current_project")
      .then((p) => setProject(p))
      .catch(() => setProject(null))
      .finally(() => setProjectLoaded(true));
  }, []);

  // Whenever the open project changes, reload its contents and saved view.
  useEffect(() => {
    // アンドゥはプロジェクト単位。前プロジェクトの履歴（と保存予約）を破棄し、
    // このプロジェクトのDBに保存されている履歴を読み込む（再起動後も続く）。
    resetStacks();
    if (project) {
      loadStacks();
      setTouched(new Set());
      setSelected(new Set());
      setSelectedNotes(new Set());
      mtimeBaseRef.current = new Map();
      refresh();
      invoke<Transform | null>("get_view_transform")
        .then((t) =>
          setView({ key: project.path, transform: t ?? { x: 0, y: 0, scale: 1 } })
        )
        .catch(() => setView({ key: project.path, transform: { x: 0, y: 0, scale: 1 } }));
    } else {
      setObjects([]);
      setRelations([]);
      setNotes([]);
      setShapes([]);
      setView(null);
    }
  }, [project, refresh, resetStacks, loadStacks]);

  // v1.4.22 ②: プロジェクト間コピー/移動の後、送り先プロジェクトを開いたら送った物を
  // 選択状態で迎えてビューを寄せる。元の座標のまま既存カードと重なっていても、選択
  // ドラッグ（まとめ移動）でそのまま空きキャンバスへ分離できる。
  // 保存ビューの復元（view）とデータ読込（objects等）の両方が揃ってから発火する。
  useEffect(() => {
    const pend = pendingSelectRef.current;
    if (!pend || !project || !view || view.key !== project.path) return;
    const norm = (s: string) => s.replace(/\\/g, "/").toLowerCase();
    if (norm(pend.path) !== norm(project.path)) return;
    const cards = objects.filter((o) => pend.cards.includes(o.id));
    const noteHits = notes.filter((n) => pend.notes.includes(n.id));
    const shapeHits = shapes.filter((s) => pend.shapes.includes(s.id));
    if (!cards.length && !noteHits.length && !shapeHits.length) return; // データ読込前
    pendingSelectRef.current = null;
    setSelected(new Set(cards.map((o) => o.id)));
    setSelectedNotes(new Set(noteHits.map((n) => n.id)));
    setSelectedShapes(new Set(shapeHits.map((s) => s.id)));
    // 送った物の外接矩形へビューを寄せる（fitTo が余白を付ける）。
    const boxes = [
      ...cards.map((o) => ({ x: o.pos_x, y: o.pos_y, w: CARD_W, h: CARD_H })),
      ...noteHits.map((n) => {
        const sz = measuredNoteSize(n);
        return { x: n.pos_x, y: n.pos_y, w: sz.w, h: sz.h };
      }),
      ...shapeHits.map((s) => ({ x: s.pos_x, y: s.pos_y, w: s.width, h: s.height })),
    ];
    const minX = Math.min(...boxes.map((b) => b.x));
    const minY = Math.min(...boxes.map((b) => b.y));
    const maxX = Math.max(...boxes.map((b) => b.x + b.w));
    const maxY = Math.max(...boxes.map((b) => b.y + b.h));
    canvasApi.current?.fitTo({ x: minX, y: minY, w: maxX - minX, h: maxY - minY });
  }, [project, view, objects, notes, shapes, measuredNoteSize, setSelected, setSelectedNotes, setSelectedShapes]);

  // v1.3-①: apply / persist the UI color preset.
  useEffect(() => {
    if (theme === "midnight") delete document.documentElement.dataset.texosTheme;
    else document.documentElement.dataset.texosTheme = theme;
    localStorage.setItem("texos-theme", theme);
  }, [theme]);

  // v1.4.16 遊び心: 明るさ・コントラストのお手軽調整。html に filter をかける
  // （html＝ビューポートなので fixed 要素＝メニュー/ダイアログの位置はズレない）。
  useEffect(() => {
    const { bright, contrast } = uiAdjust;
    const f =
      bright === 0 && contrast === 0
        ? ""
        : `brightness(${(1 + bright * 0.06).toFixed(3)}) contrast(${(1 + contrast * 0.08).toFixed(3)})`;
    document.documentElement.style.filter = f;
    localStorage.setItem("texos-ui-adjust", JSON.stringify(uiAdjust));
  }, [uiAdjust]);

  const handleUiAdjust = useCallback(
    (kind: "brighter" | "darker" | "contrastUp" | "contrastDown" | "reset") => {
      setUiAdjust((prev) => {
        const clamp = (n: number) => Math.max(-4, Math.min(4, n));
        switch (kind) {
          case "brighter":
            return { ...prev, bright: clamp(prev.bright + 1) };
          case "darker":
            return { ...prev, bright: clamp(prev.bright - 1) };
          case "contrastUp":
            return { ...prev, contrast: clamp(prev.contrast + 1) };
          case "contrastDown":
            return { ...prev, contrast: clamp(prev.contrast - 1) };
          case "reset":
            return { bright: 0, contrast: 0 };
        }
      });
    },
    []
  );

  // Re-check edit state when returning to the window (e.g. after editing externally)
  useEffect(() => {
    const onFocus = async () => {
      if (!project) return;
      // v1.3.11 ランチャー: 起動したアプリが新規保存したファイルを実身として回収する
      // （戻ってきた1回だけ）。位置はランチャーカードの右隣から並べる。
      const scan = pendingLauncherScanRef.current;
      if (scan) {
        pendingLauncherScanRef.current = null;
        try {
          const created = await invoke<string[]>("import_new_project_files", {
            x: scan.x,
            y: scan.y,
          });
          if (created.length) {
            setTouched((prev) => new Set([...prev, ...created]));
            pushUndo([{ kind: "delete", ids: created }]);
          }
        } catch (e) {
          console.error("import_new_project_files failed", e);
        }
      }
      refresh();
    };
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [project, refresh, pushUndo]);

  const handleDropFiles = useCallback(
    async (paths: string[], canvasX: number, canvasY: number) => {
      // v1.4: ドロップポイントの上に落とされたら取り込みではなく「投入」。
      // 入口の窓を入れ替え、下流のパイプラインを末端まで自動実行する。
      const dropCard = objectsRef.current.find(
        (o) =>
          o.kind === "drop" &&
          canvasX >= o.pos_x &&
          canvasX <= o.pos_x + CARD_W &&
          canvasY >= o.pos_y &&
          canvasY <= o.pos_y + CARD_H
      );
      if (dropCard) {
        for (const path of paths) {
          try {
            await invoke("drop_into_point", { id: dropCard.id, path });
            await refresh();
            await runDownstreamFlowsRef.current(dropCard.id);
          } catch (e) {
            console.error("drop_into_point failed", path, e);
          }
        }
        await refresh();
        return;
      }
      const offset = 170;
      const imported: string[] = [];
      for (let i = 0; i < paths.length; i++) {
        const x = canvasX + (i % 4) * offset;
        const y = canvasY + Math.floor(i / 4) * offset;
        try {
          imported.push(await invoke<string>("import_file", { path: paths[i], label: "", x, y }));
        } catch (e) {
          console.error("import_file failed", paths[i], e);
        }
      }
      // 新規導入 → green (only visible on cards that start committed, i.e.
      // files already inside the project folder; 仮身 stays amber anyway).
      if (imported.length) {
        setTouched((prev) => new Set([...prev, ...imported]));
        // 作成のUNDO = ソフト削除（1回のドロップ分をまとめて戻す）。
        pushUndo([{ kind: "delete", ids: imported }]);
      }
      await refresh();
    },
    [refresh, pushUndo]
  );

  // URL (or text) dropped from a browser → URL link card at the drop point.
  const handleDropText = useCallback(
    async (text: string, canvasX: number, canvasY: number) => {
      const lines = text.trim().split(/\r?\n/).map((s) => s.trim()).filter(Boolean);
      const url = lines[0] ?? "";
      if (!/^https?:\/\//i.test(url)) return;
      const title = lines.length > 1 ? lines.slice(1).join(" ") : "";
      try {
        const vid = await invoke<string>("create_url", { title, url, x: canvasX, y: canvasY });
        pushUndo([{ kind: "delete", ids: [vid] }]);
        await refresh();
      } catch (e) {
        console.error("create_url (drop) failed", e);
      }
    },
    [refresh, pushUndo]
  );

  // v1.3.10: 新しく作ったカード/コメントを fromId から矢印で繋ぐ（「その場に作って繋ぐ」共通処理）。
  const linkNewNode = useCallback(
    async (fromId: string, newId: string) => {
      try {
        const rid = await invoke<number | null>("create_relation", { fromId, toId: newId });
        if (rid != null) pushUndo([{ kind: "deleteRelations", ids: [rid] }]);
        setRelations(await invoke<RelationInfo[]>("list_relations"));
      } catch (e) {
        console.error("link new node failed", e);
      }
    },
    [pushUndo]
  );

  const handleContextPlace = useCallback(
    async (canvasX: number, canvasY: number, linkFrom?: string) => {
      const selected = await open({ multiple: true, title: "配置するファイルを選択" });
      if (!selected) return;
      const paths = Array.isArray(selected) ? selected : [selected];
      const offset = 170;
      const imported: string[] = [];
      for (let i = 0; i < paths.length; i++) {
        const x = canvasX + (i % 4) * offset;
        const y = canvasY + Math.floor(i / 4) * offset;
        try {
          imported.push(await invoke<string>("import_file", { path: paths[i], label: "", x, y }));
        } catch (e) {
          console.error("import_file failed", paths[i], e);
        }
      }
      if (imported.length) {
        setTouched((prev) => new Set([...prev, ...imported]));
        pushUndo([{ kind: "delete", ids: imported }]);
      }
      await refresh();
      if (linkFrom) for (const nid of imported) await linkNewNode(linkFrom, nid);
    },
    [refresh, pushUndo, linkNewNode]
  );

  // v1.5.6 WS-M M1（§8.1）: ロック中図形のメンバー判定（cardsOnShape と同じ中心点包含）。
  const cardCenterInShape = useCallback((x: number, y: number, s: ShapeInfo) => {
    const cx = x + CARD_W / 2;
    const cy = y + CARD_H / 2;
    return cx >= s.pos_x && cx <= s.pos_x + s.width && cy >= s.pos_y && cy <= s.pos_y + s.height;
  }, []);
  // 移動がロックを破る（メンバーの中心がロック図形の外で終わる）なら、その図形を返す。
  // v1.5.6 実機FB: 対象はそのカードが**所属する**ロック図形だけ（幾何の偶然の重なりは無関係）。
  // 既に外にいるメンバー（リサイズ等の帰結）も「中へ戻す移動」以外は弾く＝迷子を増やさない。
  const lockViolation = useCallback(
    (cardId: string, newX: number, newY: number): ShapeInfo | null => {
      for (const s of shapesRef.current) {
        if (!s.locked || !s.members.includes(cardId)) continue;
        if (!cardCenterInShape(newX, newY, s)) return s;
      }
      return null;
    },
    [cardCenterInShape]
  );
  // v1.5.9: コメント（付箋）版のドラッグアウト判定。付箋も members に入るように
  // なったので（要件1）、カードと同じく所属ロック図形の外へ出す移動を弾く。中心は
  // 付箋の実測サイズで測る（notesOnShape と同じ流儀）。
  const noteLockViolation = useCallback(
    (noteId: string, newX: number, newY: number): ShapeInfo | null => {
      const n = notesRef.current.find((it) => it.id === noteId);
      if (!n) return null;
      const sz = measuredNoteSize(n);
      const cx = newX + sz.w / 2;
      const cy = newY + sz.h / 2;
      for (const s of shapesRef.current) {
        if (!s.locked || !s.members.includes(noteId)) continue;
        if (!(cx >= s.pos_x && cx <= s.pos_x + s.width && cy >= s.pos_y && cy <= s.pos_y + s.height))
          return s;
      }
      return null;
    },
    [measuredNoteSize]
  );
  // カードがロック中図形のメンバーなら、その図形を返す（削除ガード・§8.1 メンバー保全）。
  // v1.5.6 実機FB: 判定は**所属（members の id）**であって幾何ではない — 図形をリサイズ
  // したり Shift 移動しても、ロック時に取ったスナップショットのメンバーは守られ続ける。
  const lockedMemberShape = useCallback(
    (cardId: string): ShapeInfo | null =>
      shapesRef.current.find((s) => s.locked && s.members.includes(cardId)) ?? null,
    []
  );
  // redrawArrowsFor は後方（矢印まわり）で定義されるため、前方のハンドラからは ref 経由で呼ぶ
  //（TDZ 回避。レンダーごとに最新を差し込む）。
  const redrawArrowsForRef = useRef<(moved: Map<string, { x: number; y: number }>) => void>(
    () => {}
  );
  // ロック図形へ入る移動（イン警告＝許可し、**メンバーに追加**する・§8.1）。
  // 非メンバーのカードの中心がロック図形の矩形に入ったとき、その図形を返す。
  const lockEntry = useCallback(
    (cardId: string, oldX: number, oldY: number, newX: number, newY: number): ShapeInfo | null => {
      for (const s of shapesRef.current) {
        if (!s.locked || s.members.includes(cardId)) continue;
        if (!cardCenterInShape(oldX, oldY, s) && cardCenterInShape(newX, newY, s)) return s;
      }
      return null;
    },
    [cardCenterInShape]
  );
  // イン確定: バックエンドへ追加し、ローカルの members も同期する。
  const addShapeMember = useCallback(async (shapeId: string, cardId: string) => {
    try {
      await invoke("add_shape_member", { shapeId, cardId });
      setShapes((prev) =>
        prev.map((s) =>
          s.id === shapeId && !s.members.includes(cardId)
            ? { ...s, members: [...s.members, cardId] }
            : s
        )
      );
    } catch (e) {
      console.error("add_shape_member failed", e);
    }
  }, []);

  const handleMoveEnd = useCallback(async (id: string, x: number, y: number) => {
    const prev = objectsRef.current.find((o) => o.id === id);
    // v1.5.6 WS-M M1（§8.1）: ロック中図形のメンバーのドラッグアウトは弾く（位置を戻す）。
    if (prev) {
      const viol = lockViolation(id, x, y);
      if (viol) {
        // React の VDOM 差分は「座標が変わっていない」ため DOM を書き戻さない — ドラッグ中に
        // 直接書かれた transform を**命令的に**戻す（矢印も同様＝redrawArrowsFor の流儀）。
        const el = document.querySelector<HTMLElement>(
          `[data-object-id="${CSS.escape(id)}"]`
        );
        if (el) el.style.transform = `translate(${prev.pos_x}px, ${prev.pos_y}px)`;
        redrawArrowsForRef.current(new Map([[id, { x: prev.pos_x, y: prev.pos_y }]]));
        setConfirm({
          title: "モジュールがロックされています",
          message: `このカードは 🔒「${viol.label || "図形"}」のメンバーです。外へ出すには、図形メニューの「メンバーをロック」を解除してください。`,
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
        return;
      }
      const entry = lockEntry(id, prev.pos_x, prev.pos_y, x, y);
      if (entry) {
        // イン警告（§8.1）: 入れるのは自由。**所属（members）にも追加**＝以後はロックで守られる。
        void addShapeMember(entry.id, id);
        setConfirm({
          title: "ロック中のモジュールに追加されました",
          message: `カードは 🔒「${entry.label || "図形"}」のメンバーになりました（ロック解除するまで外へ出せません・削除もできません）。`,
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
      }
    }
    try {
      await invoke("move_object", { id, x, y });
      if (prev && (prev.pos_x !== x || prev.pos_y !== y)) {
        pushUndo([{ kind: "objects", moves: [{ id, x: prev.pos_x, y: prev.pos_y }] }]);
      }
      setObjects((prev) =>
        prev.map((o) => (o.id === id ? { ...o, pos_x: x, pos_y: y } : o))
      );
    } catch (e) {
      console.error("move_object failed", e);
    }
  }, [pushUndo, lockViolation, lockEntry, addShapeMember]);

  const handleTransformChange = useCallback((t: Transform) => {
    transformRef.current = t;
    // Debounced persist of pan/zoom for the current project.
    if (saveTimer.current) window.clearTimeout(saveTimer.current);
    saveTimer.current = window.setTimeout(() => {
      invoke("set_view_transform", { x: t.x, y: t.y, scale: t.scale }).catch(() => {});
    }, 400);
  }, []);

  // v1.3.1: arrows attach to object cards AND notes (コメントもリンク対象).
  const nodeBoxOf = useCallback((id: string): { x: number; y: number; w: number; h: number } | null => {
    const o = objectsRef.current.find((it) => it.id === id);
    if (o) return { x: o.pos_x, y: o.pos_y, w: CARD_W, h: CARD_H };
    const n = notesRef.current.find((it) => it.id === id);
    if (n) {
      const s = measuredNoteSize(n);
      return { x: n.pos_x, y: n.pos_y, w: s.w, h: s.h };
    }
    return null;
  }, []);

  // 後方で定義する各種「配置」ハンドラ（linkFrom 付き）を、先に定義する handleStartLink
  // から呼べるように ref で渡す（定義順の都合・TDZ 回避）。毎レンダーで最新を代入する。
  const linkActionsRef = useRef<{
    file: (x: number, y: number, linkFrom: string) => void;
    newFile: (x: number, y: number, linkFrom: string) => void;
    createEmpty: (ext: string, x: number, y: number, linkFrom: string) => void;
    createEmptyOther: (x: number, y: number, linkFrom: string) => void;
    paste: (x: number, y: number, linkFrom: string) => void;
    url: (x: number, y: number, linkFrom: string) => void;
    filter: (builtin: string, x: number, y: number, linkFrom: string) => void;
    launcher: (builtin: string, x: number, y: number, linkFrom: string) => void;
    note: (x: number, y: number, linkFrom: string) => void;
    libLauncher: (name: string, x: number, y: number, linkFrom: string) => void;
    libFilter: (name: string, x: number, y: number, linkFrom: string) => void;
    libModule: (name: string, x: number, y: number) => void;
  }>({
    file: () => {},
    newFile: () => {},
    createEmpty: () => {},
    createEmptyOther: () => {},
    paste: () => {},
    url: () => {},
    filter: () => {},
    launcher: () => {},
    note: () => {},
    libLauncher: () => {},
    libFilter: () => {},
    libModule: () => {},
  });

  const handleStartLink = useCallback(
    (fromId: string, clientX: number, clientY: number) => {
      const from = nodeBoxOf(fromId);
      if (!from) return;
      const x1 = from.x + from.w / 2;
      const y1 = from.y + from.h / 2;
      const start = screenToCanvas(transformRef.current, clientX, clientY);
      setLinkDrag({ fromId, x1, y1, x2: start.x, y2: start.y });

      const onMove = (me: globalThis.MouseEvent) => {
        const p = screenToCanvas(transformRef.current, me.clientX, me.clientY);
        setLinkDrag((prev) => (prev ? { ...prev, x2: p.x, y2: p.y } : prev));
      };
      const onUp = async (me: globalThis.MouseEvent) => {
        window.removeEventListener("mousemove", onMove);
        window.removeEventListener("mouseup", onUp);
        setLinkDrag(null);
        const el = document.elementFromPoint(me.clientX, me.clientY);
        const targetEl = el?.closest<HTMLElement>("[data-object-id], [data-note-id]");
        const toId = targetEl?.dataset.objectId ?? targetEl?.dataset.noteId;
        if (toId && toId !== fromId) {
          try {
            // 戻り値は新規作成時のみ非null（既存リンクへの重ね引きはnull）。
            const rid = await invoke<number | null>("create_relation", { fromId, toId });
            if (rid != null) pushUndo([{ kind: "deleteRelations", ids: [rid] }]);
            const rels = await invoke<RelationInfo[]>("list_relations");
            setRelations(rels);
          } catch (e) {
            console.error("create_relation failed", e);
          }
          return;
        }
        // v1.3.10: 既存ノードでなく空きキャンバスに落としたら、その場にノードを
        // 作って繋ぐメニューを出す（自分自身への落下は除く）。カードにできるものは
        // 全部その場で生やせる（図形は対象外）。
        // メニュー再編（2026-07-12）: キャンバスメニューの「作成・配置」節と同じ名称・並び・
        // 構造に対称化（図形と「このカードの仮身を配置」「既存プロジェクトへのリンク」は
        // 廃止 — 後者はキャンバスメニュー「プロジェクトを作成 ...」内にある）。
        if (!toId) {
          const cp = screenToCanvas(transformRef.current, me.clientX, me.clientY);
          const la = linkActionsRef.current;
          const cx = me.clientX;
          const cy = me.clientY;
          // サブメニューはメニューのクリック閉じの後に開き直す（openSubmenu と同じ定石）。
          const openSub = (items: MenuItem[]) =>
            window.setTimeout(() => setMenu({ x: cx, y: cy, items }), 0);
          // 「自動化カードを追加」= キャンバスメニューと同じ共有ビルダー。位置と引き出し元
          // （linkFrom）をここで束縛。ライブラリ管理（名前編集・削除）は作ることに特化する
          // このメニューでは出さない。モジュール展開は複数カードが生まれ繋ぎ先が一意に
          // 定まらないため、配置のみで矢印は張らない。
          const automationSub = buildAutomationAddSub(
            {
              library: filterLibraryRef.current.map((e) => e.name),
              launcherLibrary: launcherLibraryRef.current.map((e) => e.name),
              moduleLibrary: moduleLibraryRef.current.map((e) => e.name),
              audioDevices: audioDevicesRef.current,
            },
            {
              openSubmenu: openSub,
              addLauncher: (builtin) => la.launcher(builtin, cp.x, cp.y, fromId),
              addFilter: (builtin) => la.filter(builtin, cp.x, cp.y, fromId),
              addLibraryLauncher: (name) => la.libLauncher(name, cp.x, cp.y, fromId),
              addLibraryFilter: (name) => la.libFilter(name, cp.x, cp.y, fromId),
              addLibraryModule: (name) => la.libModule(name, cp.x, cp.y),
            }
          );
          setMenu({
            x: cx,
            y: cy,
            items: [
              { label: "ここに作って矢印で繋ぐ", onClick: () => {}, disabled: true },
              { label: "クリップボードから貼り付け", onClick: () => la.paste(cp.x, cp.y, fromId) },
              { label: "ファイルを配置...", onClick: () => la.file(cp.x, cp.y, fromId) },
              { label: "コメントを追加...", onClick: () => la.note(cp.x, cp.y, fromId) },
              {
                label: "新規ファイルを作成 ...",
                hint: "ファイルの種類（拡張子）を選んで、空のカードをこの場所に作り矢印で繋ぎます。",
                onClick: () =>
                  openSub(
                    buildNewFileSub({
                      pick: (ext) => la.createEmpty(ext, cp.x, cp.y, fromId),
                      otherExt: () => la.createEmptyOther(cp.x, cp.y, fromId),
                      osDialog: () => la.newFile(cp.x, cp.y, fromId),
                    })
                  ),
              },
              { label: "URLリンクを追加...", onClick: () => la.url(cp.x, cp.y, fromId) },
              { label: "自動化カードを追加 ...", onClick: () => openSub(automationSub) },
            ],
          });
        }
      };
      window.addEventListener("mousemove", onMove);
      window.addEventListener("mouseup", onUp);
    },
    [nodeBoxOf, pushUndo]
  );

  const handleDeleteRelation = useCallback(async (id: number) => {
    try {
      await invoke("delete_relation", { id });
      pushUndo([{ kind: "restoreRelations", ids: [id] }]);
      setRelations((prev) => prev.filter((r) => r.id !== id));
    } catch (e) {
      console.error("delete_relation failed", e);
    }
  }, [pushUndo]);

  // v1.5.3 WS-P1（オペ層 §5.4.2）: flow 矢印のクリック＝条件メニュー（削除は項目に降格）。
  // when=通過条件（成功時/失敗時/常に）・carry=運ぶもの。設定は relations.attrs へ
  // マージ書き込み（NULL=従来）。リンク矢印（kind='link'）は従来どおりクリック=削除。
  const handleFlowRelationClick = useCallback(
    (r: RelationInfo, clientX: number, clientY: number) => {
      let attrs: { when?: string; carry?: string; role?: string } = {};
      try {
        attrs = JSON.parse(r.attrs ?? "{}");
      } catch {
        /* 壊れた JSON は既定扱い */
      }
      const when = attrs.when ?? "always";
      const carry = attrs.carry ?? "";
      const role = attrs.role ?? "";
      const setAttrs = (patch: { when?: string; carry?: string; role?: string }) => {
        void invoke("set_relation_attrs", { id: r.id, ...patch })
          .then(async () => setRelations(await invoke<RelationInfo[]>("list_relations")))
          .catch((e) => console.error("set_relation_attrs failed", e));
      };
      const check = (on: boolean, label: string) => (on ? `✓ ${label}` : `　 ${label}`);
      setMenu({
        x: clientX,
        y: clientY,
        items: [
          { label: "フロー矢印の通過条件", onClick: () => {}, disabled: true },
          { label: check(when === "always", "常に（既定）"), onClick: () => setAttrs({ when: "always" }) },
          {
            label: check(when === "success", "成功したときだけ"),
            hint: "上流がエラーなく完走したときだけデータを通します。失敗は経路選択の情報になり、フローのエラーにしません。",
            onClick: () => setAttrs({ when: "success" }),
          },
          {
            label: check(when === "failure", "失敗したときだけ"),
            hint: "上流が失敗したときだけ通します（既定はエラーメッセージを運ぶ＝AI に読ませて分岐できます）。",
            onClick: () => setAttrs({ when: "failure" }),
          },
          { label: "", onClick: () => {}, divider: true },
          { label: "運ぶもの", onClick: () => {}, disabled: true },
          {
            label: check(carry === "", "既定（成功=出力／失敗=エラーメッセージ）"),
            onClick: () => setAttrs({ carry: "" }),
          },
          {
            label: check(carry === "stderr", "エラーメッセージ"),
            onClick: () => setAttrs({ carry: "stderr" }),
          },
          {
            label: check(carry === "input", "失敗時の入力（リトライ材料）"),
            onClick: () => setAttrs({ carry: "input" }),
          },
          {
            label: check(carry === "none", "なし（信号のみ＝ゲート）"),
            onClick: () => setAttrs({ carry: "none" }),
          },
          { label: "", onClick: () => {}, divider: true },
          { label: "この矢印が運ぶもの（役割）", onClick: () => {}, disabled: true },
          {
            label: check(role === "", "入力（既定）"),
            onClick: () => setAttrs({ role: "" }),
          },
          {
            label: check(role === "system", "役割＝システムプロンプト"),
            hint:
              "上流の中身を、AI カードの「役割」（システムプロンプト）として渡します。" +
              "付箋を繋いで役割にすれば、誰がどんな担当なのかがキャンバスから読めて、" +
              "その場で書き換えられます。\n" +
              "この矢印は入力に数えません（AI カードが入力ごとに何度も走るのを避けるため）。",
            onClick: () => setAttrs({ role: "system" }),
          },
          {
            label: check(role === "previous", "役割＝前周の結果（反復の戻り）"),
            hint:
              "反復カードから上流へ引き戻す矢印に付けます。実行中は「1周前の出力」がここを" +
              "流れ、1周目だけは空です。\n" +
              "この矢印をたどって上流を評価することはありません（絵は輪に見えますが、" +
              "実行は行き止まりのない一方向のままです）。",
            onClick: () => setAttrs({ role: "previous" }),
          },
          {
            label: check(role === "answer", "役割＝答え（問い合わせカードへ）"),
            hint:
              "付箋を「答えの箱」として問い合わせカードに繋ぎます。付箋に何か書いてあれば" +
              "それが答えとして下流へ流れ、空なら実行はそこで止まります。\n" +
              "「>」で始まる行は質問の控えとして扱い、答えには含めません。",
            onClick: () => setAttrs({ role: "answer" }),
          },
          { label: "", onClick: () => {}, divider: true },
          {
            label: "矢印を削除",
            danger: true,
            onClick: () => void handleDeleteRelation(r.id),
          },
        ],
      });
    },
    [handleDeleteRelation]
  );

  // --- v1.4 オートフロー⑤ ----------------------------------------------------

  // フィルタカードを作成。引数はプロンプトの連鎖で聞く（フォーム自動生成はフェーズ2）。
  const handleAddFilter = useCallback(
    (builtin: string, canvasX: number, canvasY: number, linkFrom?: string) => {
      // ドロップポイント＝パイプラインのデータ入口カード（投入で下流が自動実行）
      if (builtin === "drop_point") {
        (async () => {
          try {
            const vid = await invoke<string>("create_drop_point", { x: canvasX, y: canvasY });
            pushUndo([{ kind: "delete", ids: [vid] }]);
            await refresh();
            if (linkFrom) await linkNewNode(linkFrom, vid);
          } catch (e) {
            console.error("create_drop_point failed", e);
          }
        })();
        return;
      }
      const create = async (label: string, spec: Record<string, unknown>) => {
        try {
          const vid = await invoke<string>("create_filter", {
            label,
            spec: JSON.stringify(spec),
            x: canvasX,
            y: canvasY,
          });
          pushUndo([{ kind: "delete", ids: [vid] }]);
          await refresh();
          if (linkFrom) await linkNewNode(linkFrom, vid);
        } catch (e) {
          console.error("create_filter failed", e);
          setConfirm({
            title: "フィルタを作成できませんでした",
            message: String(e),
            okLabel: "OK",
            onConfirm: () => setConfirm(null),
          });
        }
      };
      // v1.5.4 WS-P2 オペレーション: トリガは即作成（設定項目なし）、タイマ/ウォッチャは
      // スキーマフォーム（BuiltinFilterDialog）で作成。spec の op キーを見て backend の
      // create_filter が kind='op' として作る。
      if (builtin === "op_trigger") {
        create("トリガ", { op: "trigger" });
        return;
      }
      // v1.5.6 WS-M M1（§9.1 手動設置）: IN/OUT バーチャルカード＝即作成（設定項目なし）。
      if (builtin === "op_in") {
        create("IN", { op: "in" });
        return;
      }
      if (builtin === "op_out") {
        create("OUT", { op: "out" });
        return;
      }
      if (builtin === "op_timer" || builtin === "op_watch") {
        setBuiltinDlg({
          id: null,
          builtin,
          label: builtin === "op_timer" ? "タイマ" : "ウォッチャ",
          // v1.5.5: 停止状態で生成（構成中の誤発火防止・実機FB）。配線後に「▶ 稼働開始」。
          // paused はフォーム外キーなのでダイアログの base として温存される。
          spec: '{"paused":true}',
          create: { x: canvasX, y: canvasY, linkFrom },
        });
        return;
      }
      if (builtin === "ai_dialog" || builtin === "ai_chat") {
        // v1.4.1 AIカードの作成は専用設定ダイアログで（モデル/システム/温度を一括設定）。
        // 作成・編集で同じダイアログ＝システムプロンプトも複数行で書ける。
        const chat = builtin === "ai_dialog";
        const spec = {
          builtin: "ai",
          mode: chat ? "chat" : "transform",
          provider: "ollama",
          model: "gemma3:latest",
          system: chat
            ? "あなたは親切で簡潔なアシスタントです。"
            : "あなたは有能なアシスタントです。簡潔なMarkdownで答えてください。",
          accept_mime: chat ? "*/*" : "text/*",
          output_ext: "md",
        };
        setAiDlg({
          id: null,
          label: chat ? "AI対話" : "AI変換",
          spec: JSON.stringify(spec),
          create: { x: canvasX, y: canvasY, linkFrom },
        });
        return;
      }
      if (builtin === "t2i") {
        // v1.5 Text-To-Image: 依存＋モデルが揃っていれば設定ダイアログ、未整備ならセットアップへ。
        void (async () => {
          try {
            const s = await invoke<T2iStatus>("t2i_status");
            if (s.deps_ok && s.models.length > 0) {
              setT2iDlg({
                id: null,
                label: "画像生成",
                spec: JSON.stringify({
                  builtin: "t2i",
                  model: s.current_model || s.models[0],
                  steps: 25,
                  guidance: 7.5,
                  width: 512,
                  height: 512,
                  accept_mime: "*/*",
                  output_ext: "png",
                }),
                models: s.models,
                known: s.known.map((k) => k.name),
                assets: { loras: s.loras, embeddings: s.embeddings, vae: s.vae },
                create: { x: canvasX, y: canvasY, linkFrom },
              });
            } else {
              setT2iSetup({ x: canvasX, y: canvasY, linkFrom });
            }
          } catch (e) {
            console.error("t2i_status failed", e);
            setT2iSetup({ x: canvasX, y: canvasY, linkFrom });
          }
        })();
        return;
      }
      if (builtin === "searxng_manage") {
        setSearxngSetup({ x: canvasX, y: canvasY, linkFrom, manage: true });
        return;
      }
      if (builtin === "web_search") {
        // Web検索 P2: 検索エンジンが未導入なら先に導入ダイアログ（済めばここへ戻ってくる）。
        // 導入済みなら検索語を聞いて作成（空欄=検索語を書いたテキストカードを接続する運用）。
        (async () => {
          let installed = false;
          try {
            installed = (await invoke<SearxngStatus>("searxng_status")).installed;
          } catch (e) {
            console.error("searxng_status failed", e);
          }
          if (!installed) {
            setSearxngSetup({ x: canvasX, y: canvasY, linkFrom });
            return;
          }
          setPrompt({
            title: "Web検索: 検索語（空欄=検索語を書いたテキストカードを接続）",
            initial: "",
            allowEmpty: true,
            onConfirm: (q) => {
              const query = q.trim();
              create(query ? `Web検索: ${query.slice(0, 24)}` : "Web検索", {
                builtin: "web_search",
                ...(query ? { query } : {}),
                accept_mime: "text/*",
              });
            },
          });
        })();
        return;
      }
      if (builtin === "t23d") {
        // v2.0 t23d: エンジン（Hunyuan3D-2 WinPortable）が未設定なら先に場所を聞く。
        // 設定済みならスキーマフォーム（BuiltinFilterDialog）で作成。
        const openDlg = (currentModel: string) => {
          setBuiltinDlg({
            id: null,
            builtin: "t23d",
            label: "3D生成",
            spec: JSON.stringify({ model: currentModel }),
            create: { x: canvasX, y: canvasY, linkFrom },
          });
        };
        void (async () => {
          try {
            const s = await invoke<T23dStatus>("t23d_status");
            if (s.engine_ok) {
              openDlg(s.current_model);
              return;
            }
            setPrompt({
              title:
                "3D生成エンジンの場所（Hunyuan3D-2 WinPortable のフォルダを絶対パスで。例 F:\\Hunyuan3D2_WinPortable\\Hunyuan3D2_WinPortable）",
              initial: s.engine_path || "",
              onConfirm: async (path) => {
                try {
                  await invoke("t23d_set_engine", { path });
                  openDlg("hunyuan3d-2");
                } catch (e) {
                  setConfirm({
                    title: "エンジンを認識できませんでした",
                    message: String(e),
                    okLabel: "OK",
                    onConfirm: () => setConfirm(null),
                  });
                }
              },
            });
          } catch (e) {
            console.error("t23d_status failed", e);
            setConfirm({
              title: "3D生成の状態を取得できませんでした",
              message: String(e),
              okLabel: "OK",
              onConfirm: () => setConfirm(null),
            });
          }
        })();
        return;
      }
      if (builtin === "md_codeblock") {
        // v1.4.0 AI後段: Markdownのフェンス付きコードブロックを取り出してソース化。
        // 言語を指定すると拡張子も自動推定（c→.c, py→.py …）。そのままgcc等へ繋げる。
        setPrompt({
          title: "コード抽出: 言語（空欄=最初のブロック。例 c / py / rust）",
          initial: "c",
          onConfirm: (lang) => {
            const l = lang.trim().toLowerCase();
            create(l ? `コード抽出: ${l}` : "コード抽出", {
              builtin: "md_codeblock",
              ...(l ? { lang: l } : {}),
              accept_mime: "text/*",
            });
          },
        });
      } else if (builtin === "select") {
        // v1.5.3 WS-P1: セレクタ（オペ層 §5.4.1）。パラメータなし＝即作成。
        // 入力順の first-match（最初の有データ枝を採用）・no-data 吸収を内包。
        create("セレクタ", { builtin: "select", accept_mime: "*/*" });
      } else if (builtin === "tap") {
        // v1.8 記録カード: 素通ししながら中身をファイルに残す。名前＝prefix（既定はラベル）。
        setPrompt({
          title: "記録の名前（この名前で毎回上書き保存します）",
          initial: "記録",
          onConfirm: (name) => {
            const stem = name.trim() || "記録";
            create(stem, { builtin: "tap", accept_mime: "*/*", prefix: stem });
          },
        });
      } else if (builtin === "ask") {
        // v1.8 問い合わせカード: 質問だけ聞いて作成。答えの付箋は最初の実行で用意される。
        setPrompt({
          title: "問い合わせ: 人に聞くこと",
          initial: "この案で進めてよいですか？",
          onConfirm: (q) => {
            const question = q.trim() || "この案で進めてよいですか？";
            create(`問い合わせ: ${question}`, {
              builtin: "ask",
              accept_mime: "*/*",
              question,
            });
          },
        });
      } else if (builtin === "loop") {
        // v1.8 反復カード: 上流をもう一度引き直す。最大周回数＝ヒューズ（必須）、
        // 停止条件は「出力にこの語が入っていたら終わり」の1段目（決定的ルール）。
        setPrompt({
          title: "反復: 最大何周まで繰り返しますか（暴走止め）",
          initial: "3",
          onConfirm: (times) => {
            const n = Math.max(1, Math.min(50, parseInt(times, 10) || 3));
            setPrompt({
              title: "反復: 出力にこの語が現れたら終わり（空欄＝回数だけで止める）",
              initial: "",
              onConfirm: (word) => {
                const w = word.trim();
                create(w ? `反復: ${w}まで（最大${n}周）` : `反復: 最大${n}周`, {
                  builtin: "loop",
                  accept_mime: "*/*",
                  max_rounds: n,
                  ...(w ? { until: "contains", pattern: w } : {}),
                });
              },
            });
          },
        });
      } else if (builtin === "text_grep") {
        setPrompt({
          title: "grep: 抽出する正規表現",
          initial: "",
          onConfirm: (pat) =>
            create(`grep: ${pat}`, { builtin: "text_grep", pattern: pat, accept_mime: "text/*" }),
        });
      } else if (builtin === "text_replace") {
        setPrompt({
          title: "置換: 検索する正規表現",
          initial: "",
          onConfirm: (pat) =>
            setPrompt({
              title: "置換: 置き換え文字列",
              initial: "",
              onConfirm: (rep) =>
                create(`置換: ${pat}→${rep}`, {
                  builtin: "text_replace",
                  pattern: pat,
                  replacement: rep,
                  accept_mime: "text/*",
                }),
            }),
        });
      } else if (builtin === "text_head" || builtin === "text_tail") {
        // v1.5.12 P1: Unix標準テキストフィルタ。行数だけ聞いて作成（詳細はダブルクリックで）。
        const isHead = builtin === "text_head";
        setPrompt({
          title: isHead ? "head: 先頭から残す行数" : "tail: 末尾から残す行数",
          initial: "10",
          onConfirm: (v) => {
            const n = Math.max(1, parseInt(v, 10) || 10);
            create(isHead ? `先頭 ${n}行` : `末尾 ${n}行`, {
              builtin,
              lines: n,
              accept_mime: "text/*",
            });
          },
        });
      } else if (builtin === "text_sort") {
        // 既定（昇順・辞書順）で即作成。降順・数値比較・重複除去は設定ダイアログで。
        create("並べ替え", { builtin: "text_sort", accept_mime: "text/*" });
      } else if (builtin === "text_count") {
        create("行数・単語数・文字数", { builtin: "text_count", accept_mime: "text/*" });
      } else if (builtin === "text_cut") {
        setPrompt({
          title: "cut: 取り出す列（1始まり。例 1,3-5）",
          initial: "1",
          onConfirm: (f) =>
            create(`列の取り出し: ${f.trim()}`, {
              builtin: "text_cut",
              delimiter: "tab",
              fields: f.trim(),
              accept_mime: "text/*",
            }),
        });
      } else if (builtin === "web_fetch") {
        // v1.7 P1: URL を聞いて作成。空欄でも作る（URL はテキストカード接続 or 設定
        // ダイアログで後から — 実機FB 2026-07-16: 空欄=キャンセルだと接続運用の入口が無い）。
        setPrompt({
          title: "Webページ取得: URL（空欄=URLを書いたテキストカードを接続）",
          initial: "",
          allowEmpty: true,
          onConfirm: (u) => {
            const url = u.trim();
            let host = "";
            try {
              host = url ? new URL(url).host : "";
            } catch {
              /* 不正なURLでもカードは作る（設定ダイアログで直せる） */
            }
            create(host ? `Web取得: ${host}` : "Webページ取得", {
              builtin: "web_fetch",
              ...(url ? { url } : {}),
              accept_mime: "text/*",
            });
          },
        });
      } else if (builtin === "web_feed") {
        // v1.7 P3: フィードURLを聞いて作成（web_fetch と同型・空欄=接続運用）。
        setPrompt({
          title: "フィード取得: URL（空欄=URLを書いたテキストカードを接続）",
          initial: "",
          allowEmpty: true,
          onConfirm: (u) => {
            const url = u.trim();
            let host = "";
            try {
              host = url ? new URL(url).host : "";
            } catch {
              /* 不正なURLでもカードは作る（設定ダイアログで直せる） */
            }
            create(host ? `フィード: ${host}` : "フィード取得", {
              builtin: "web_feed",
              ...(url ? { url } : {}),
              accept_mime: "text/*",
            });
          },
        });
      } else if (builtin === "web_capture") {
        // v1.7 P4: URL を聞いて作成（web_fetch と同型・空欄=接続運用）。
        setPrompt({
          title: "ページキャプチャ: URL（空欄=URLを書いたテキストカードを接続）",
          initial: "",
          allowEmpty: true,
          onConfirm: (u) => {
            const url = u.trim();
            let host = "";
            try {
              host = url ? new URL(url).host : "";
            } catch {
              /* 不正なURLでもカードは作る（設定ダイアログで直せる） */
            }
            create(host ? `キャプチャ: ${host}` : "ページキャプチャ", {
              builtin: "web_capture",
              ...(url ? { url } : {}),
              accept_mime: "text/*",
            });
          },
        });
      } else if (builtin === "to_url") {
        // v1.7 P5: テキスト→URLカード群。パラメータなし＝そのまま作る。
        create("URLカード化", { builtin: "to_url", accept_mime: "text/*" });
      } else if (builtin === "text_convert") {
        // nkf 相当。既定=自動判定→UTF-8。文字コードの明示は設定ダイアログで。
        create("UTF-8へ変換", { builtin: "text_convert", encoding: "auto", accept_mime: "text/*" });
      } else if (builtin === "text_concat") {
        // v1.5.17 P2 集約系: 複数入力を接続順に連結（cat）。区切りは設定ダイアログで。
        create("連結（cat）", { builtin: "text_concat", accept_mime: "text/*" });
      } else if (builtin === "text_diff") {
        create("差分（diff）", { builtin: "text_diff", accept_mime: "text/*" });
      } else if (builtin === "text_paste") {
        create("行の横結合（paste）", { builtin: "text_paste", delimiter: "tab", accept_mime: "text/*" });
      } else if (builtin === "speak") {
        // v1.5.15 L2 OSアダプタ第1弾: OS の音声合成で読み上げ（fire-and-forget・出力なし）。
        create("読み上げ", { builtin: "speak", accept_mime: "text/*" });
      } else if (builtin === "import_new") {
        // v1.6: フォルダの未登録ファイルをカード登録（対象フォルダは設定ダイアログで）。
        create("新規カードを登録", { builtin: "import_new" });
      } else if (builtin === "img_resize") {
        setPrompt({
          title: "リサイズ: 幅（px・高さは自動）",
          initial: "800",
          onConfirm: (w) =>
            create(`リサイズ: 幅${w}px`, {
              builtin: "img_resize",
              width: parseInt(w, 10) || 800,
              accept_mime: "image/*",
            }),
        });
      } else if (builtin === "img_convert") {
        setPrompt({
          title: "形式変換: png / jpeg / webp / bmp / gif",
          initial: "png",
          onConfirm: (f) =>
            create(`変換: ${f.trim().toLowerCase()}`, {
              builtin: "img_convert",
              format: f.trim().toLowerCase(),
              accept_mime: "image/*",
            }),
        });
      } else if (builtin === "audio_clip" || builtin === "video_clip") {
        // v1.3.11 #4/#5: ffmpeg を使う音声/動画の簡易クリップ（外部コマンドフィルタの
        // プリセット）。ffmpeg を .texos\filters か PATH に置けば動く。動画は -c copy で
        // 再エンコードせずに切り出す＝容量が最小・高速（#5「容量が最小限」）。
        const isVid = builtin === "video_clip";
        setPrompt({
          title: "開始位置（秒）",
          initial: "0",
          onConfirm: (start) =>
            setPrompt({
              title: "長さ（秒）",
              initial: "10",
              onConfirm: (dur) => {
                const s = String(parseFloat(start) || 0);
                const d = String(parseFloat(dur) || 10);
                const copy = isVid ? " -c copy" : "";
                create(
                  isVid ? `動画クリップ ${s}s+${d}s` : `音声クリップ ${s}s+${d}s`,
                  {
                    name: "ffmpeg.exe",
                    type: "command",
                    cmd: `-y -ss ${s} -i {in} -t ${d}${copy} {out}`,
                    output_ext: isVid ? "mp4" : "mp3",
                    accept_mime: isVid ? "video/*" : "audio/*",
                    timeout_secs: 600,
                    note: "ffmpeg が必要（.texos\\filters か PATH に置いてください）",
                  }
                );
              },
            }),
        });
      } else if (builtin === "shell") {
        // シェルカード統合（2026-07-06）: 旧ランチャー版（開くだけ）と旧対話フィルタ版
        // （一行目プロンプト）を1カードに統合。作成から専用ダイアログで設定する
        // （シェル選択・コマンド・ウィンドウ 開いたまま/閉じる/出さない）。
        setShellDlg({
          id: null,
          label: "",
          create: { x: canvasX, y: canvasY, linkFrom },
        });
      } else if (builtin === "audio_norm") {
        // v1.3.11 #4: ラウドネス正規化（EBU R128 loudnorm）。
        create("音声ノーマライズ", {
          name: "ffmpeg.exe",
          type: "command",
          cmd: `-y -i {in} -af loudnorm {out}`,
          output_ext: "mp3",
          accept_mime: "audio/*",
          timeout_secs: 600,
          note: "ffmpeg が必要（.texos\\filters か PATH に置いてください）",
        });
      } else if (builtin === "to_wav16k") {
        // v1.4.8 ①: Whisper の入力アダプタ。m4a（標準サウンドレコーダー）/動画/ステレオ等を
        // whisper.cpp が要求する 16kHz mono WAV に変換する。Whisper カードの前段に繋ぐ。
        // -vn で動画が来ても音声だけ取り出す（将来「動画から取り出し」等の専用プリセットへ拡張予定）。
        create("Whisper用WAVに変換", {
          name: "ffmpeg.exe",
          type: "command",
          cmd: `-y -i {in} -ar 16000 -ac 1 -vn {out}`,
          output_ext: "wav",
          accept_mime: "audio/*",
          timeout_secs: 600,
          note: "ffmpeg が必要（.texos\\filters か PATH に置いてください）。16kHz mono WAV に変換します（Whisper の前段）。",
        });
      } else if (builtin === "whisper") {
        // v1.4.8 ①: Whisper 文字起こし。導入済み（ffmpeg/whisper本体/モデル）なら即カード化、
        // 未導入ならセットアップダイアログを開く（A案・app_data共有）。
        void (async () => {
          try {
            const s = await invoke<WhisperStatus>("whisper_status");
            if (s.ffmpeg && s.whisper && s.current_ready) {
              create("Whisper 文字起こし", whisperSpec(s));
            } else {
              setWhisperDlg({ x: canvasX, y: canvasY, linkFrom });
            }
          } catch (e) {
            console.error("whisper_status failed", e);
          }
        })();
      } else if (builtin.startsWith("mic_record:")) {
        // v1.4.9: マイク録音ジェネレータ。record:true で stdin を開き、実行中パネルの
        // 「停止」が 'q' を送って ffmpeg を正常終了→WAV確定。-ar 16000 -ac 1 で Whisper 直結可。
        const device = builtin.slice("mic_record:".length);
        create("マイク録音", {
          name: "ffmpeg.exe",
          type: "generator",
          record: true,
          stop_cmd: "q",
          cmd: `-hide_banner -f dshow -i "audio=${device}" -ar 16000 -ac 1 -y {out}`,
          output_ext: "wav",
          accept_mime: "audio/*",
          timeout_secs: 0,
          note: `マイク録音（${device}）。▶実行で録音開始 → 実行中パネルの「停止」で終了し WAV を確定（16kHz mono・Whisper 直結可）。`,
        });
      } else {
        // 外部プログラム: 設定ダイアログ（v1.5）。NAME/PATH/Type/CMD/IN/OUT/Prefix を
        // フォームで設定する — JSON直編集・プロンプト連鎖は現実的でない（実機指摘）。
        setFilterDlg({ x: canvasX, y: canvasY });
      }
    },
    [refresh, pushUndo, linkNewNode]
  );

  // v1.3.11 ランチャー: 一般プログラム/ターミナルを登録するカードを作成。
  // builtin: "program"（exeをファイル選択）/ "cmd" / "powershell"。
  const handleAddLauncher = useCallback(
    (builtin: string, canvasX: number, canvasY: number, linkFrom?: string) => {
      const create = async (label: string, spec: Record<string, unknown>) => {
        try {
          const vid = await invoke<string>("create_launcher", {
            label,
            spec: JSON.stringify(spec),
            x: canvasX,
            y: canvasY,
          });
          pushUndo([{ kind: "delete", ids: [vid] }]);
          await refresh();
          if (linkFrom) await linkNewNode(linkFrom, vid);
        } catch (e) {
          console.error("create_launcher failed", e);
          setConfirm({
            title: "ランチャーを作成できませんでした",
            message: String(e),
            okLabel: "OK",
            onConfirm: () => setConfirm(null),
          });
        }
      };
      if (builtin === "installed") {
        // v1.3.11 #A: インストール済みアプリ（スタートメニュー）から検索して選ぶ。
        setAppPicker({ x: canvasX, y: canvasY, linkFrom });
      } else if (builtin === "cmd") {
        void create("コマンドプロンプト", { name: "cmd", type: "launcher" });
      } else if (builtin === "powershell") {
        void create("PowerShell", { name: "powershell", type: "launcher" });
      } else {
        // 一般プログラム: exe を選び、表示名を聞いて登録（アイコンは exe から取得）。
        (async () => {
          const sel = await open({
            multiple: false,
            title: "登録するプログラム（.exe）を選択",
            filters: [{ name: "プログラム", extensions: ["exe", "lnk", "bat", "cmd", "com"] }],
          });
          if (typeof sel !== "string") return;
          const base = sel.replace(/^.*[\\/]/, "").replace(/\.(exe|lnk|bat|cmd|com)$/i, "");
          setPrompt({
            title: "ランチャーの表示名",
            initial: base || "プログラム",
            onConfirm: (label) =>
              create(label, {
                name: sel.replace(/^.*[\\/]/, ""),
                path: sel,
                type: "launcher",
              }),
          });
        })();
      }
    },
    [refresh, pushUndo, linkNewNode]
  );

  // v1.3.11 ランチャー実行: プログラムを detached 起動（接続ファイルがあれば開く）。
  // 起動後、ウィンドウに戻ったときプロジェクト直下の新規ファイルを実身として回収する
  // （アプリが新規保存→新しい実身。pendingLauncherScanRef で1回だけ）。
  const handleRunLauncher = useCallback(
    async (id: string) => {
      try {
        await invoke("run_launcher", { id });
        const obj = objectsRef.current.find((o) => o.id === id);
        pendingLauncherScanRef.current = {
          x: (obj?.pos_x ?? 0) + 180,
          y: obj?.pos_y ?? 0,
        };
      } catch (e) {
        console.error("run_launcher failed", e);
        setConfirm({
          title: "起動できませんでした",
          message: String(e),
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
      }
    },
    []
  );

  // v1.3.11 #A: アプリピッカーで選んだインストール済みアプリをランチャーとして配置。
  const handleCreateLauncherFromApp = useCallback(
    async (app: { name: string; path: string }) => {
      const pick = appPicker;
      setAppPicker(null);
      if (!pick) return;
      try {
        const vid = await invoke<string>("create_launcher", {
          label: app.name,
          spec: JSON.stringify({
            name: app.path.replace(/^.*[\\/]/, ""),
            path: app.path,
            type: "launcher",
          }),
          x: pick.x,
          y: pick.y,
        });
        pushUndo([{ kind: "delete", ids: [vid] }]);
        await refresh();
        if (pick.linkFrom) await linkNewNode(pick.linkFrom, vid);
      } catch (e) {
        console.error("create_launcher (app) failed", e);
      }
    },
    [appPicker, refresh, pushUndo, linkNewNode]
  );

  // v1.3.11 ランチャーライブラリ: 登録／再配置／削除。
  const handleSaveLauncherToLibrary = useCallback(
    (id: string) => {
      const obj = objectsRef.current.find((o) => o.id === id);
      const spec = obj?.filter_ref;
      if (!spec) return;
      setPrompt({
        title: "ランチャーライブラリに登録: 名前（同名は上書き）",
        initial: obj?.label ?? "",
        onConfirm: async (name) => {
          try {
            await invoke("save_launcher_library", { name, spec });
            await reloadLauncherLibrary();
          } catch (e) {
            setConfirm({
              title: "登録できませんでした",
              message: String(e),
              okLabel: "OK",
              onConfirm: () => setConfirm(null),
            });
          }
        },
      });
    },
    [reloadLauncherLibrary]
  );

  const handleAddLibraryLauncher = useCallback(
    // メニュー再編（2026-07-12）: 矢印引き出しメニューからも配置できるよう linkFrom 対応。
    async (name: string, x: number, y: number, linkFrom?: string) => {
      const entry = launcherLibraryRef.current.find((e) => e.name === name);
      if (!entry) return;
      try {
        const vid = await invoke<string>("create_launcher", { label: name, spec: entry.spec, x, y });
        pushUndo([{ kind: "delete", ids: [vid] }]);
        await refresh();
        if (linkFrom) await linkNewNode(linkFrom, vid);
      } catch (e) {
        console.error("add library launcher failed", e);
      }
    },
    [refresh, pushUndo, linkNewNode]
  );

  const handleDeleteLibraryLauncher = useCallback(
    async (name: string) => {
      try {
        await invoke("delete_launcher_library", { name });
        await reloadLauncherLibrary();
      } catch (e) {
        console.error("delete launcher library failed", e);
      }
    },
    [reloadLauncherLibrary]
  );

  // フローを実行（手動トリガー — 確定方針4）。結果カードは緑シグナル＋Ctrl+Zで戻せる。
  const handleRunFlow = useCallback(
    async (id: string) => {
      // v1.5.4 WS-P2: op カード（トリガ▶）は run_flow ではなく fire_op（代理発行）。
      // fire_op はスレッドへ発行してすぐ返る＝進行はジョブパネル・結果は job:done の refresh。
      // 同一終端の多重はジョブ台帳（register_flow）が拒否するので二度押しガードは不要。
      // v1.5.6 WS-M M1: OUT バーチャルカード（op:"out"）は通常の run_flow（さかのぼり pull
      // 実行・§9.3）へ流す。IN は実行の口ではないので案内だけ。
      const opObj = objectsRef.current.find((o) => o.id === id);
      const opKind: string | null = (() => {
        if (opObj?.kind !== "op") return null;
        try {
          const k = JSON.parse(opObj.filter_ref ?? "{}").op;
          return typeof k === "string" ? k : null;
        } catch {
          return null;
        }
      })();
      if (opKind === "in") {
        setConfirm({
          title: "IN カードは実行できません",
          message: "IN はモジュールの入口（素通し）です。実行は OUT カードの ▶ か、下流のフィルタカードから行ってください。",
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
        return;
      }
      if (opKind !== null && opKind !== "out") {
        try {
          const n = await invoke<number>("fire_op", { id });
          await refresh(); // fired_count の書き戻しを拾う
          setConfirm({
            title: "発行しました",
            message: `${n} 件のフローを発行しました。進行状況は背景ジョブパネルに表示されます。`,
            okLabel: "OK",
            onConfirm: () => setConfirm(null),
          });
        } catch (e) {
          console.error("fire_op failed", e);
          setConfirm({
            title: "発行できませんでした",
            message: String(e),
            okLabel: "OK",
            onConfirm: () => setConfirm(null),
          });
        }
        return;
      }
      // v1.5.2 WS-C: 二度押し抑止は**カード単位**（並行解禁後は別フローの同時実行が正規の
      // 使い方。同一カードの連打だけ弾く）。バックエンド register_flow にも同じガードがある。
      if (runningCardsRef.current.has(id)) return;
      runningCardsRef.current.add(id);
      // 起動元プロジェクトを記憶 — 実行中に切り替えられたら結果は起動元にあり、
      // このキャンバスには無い（refresh・緑シグナル・UNDO はスキップする）。
      const startPath = projectPathRef.current;
      let isShell = false;
      try {
        const obj = objectsRef.current.find((o) => o.id === id);
        if (obj?.filter_ref) {
          const s = JSON.parse(obj.filter_ref);
          isShell = typeof s.shell === "string";
        }
      } catch {
        /* spec が読めなければシェル扱いしない */
      }
      try {
        const res = await invoke<{
          created: string[];
          updated: string[];
          outputs: string[];
          skipped: string[];
          errors: string[];
          shell_capture: { exit_code: number | null; output: string; truncated: boolean } | null;
        }>("run_flow", { id });
        // シェルカード統合（2026-07-06）: ウィンドウなしシェル実行は捕捉出力を結果パネルで
        // 表示して終わり（カード生成なし・「フローを実行しました」ダイアログは出さない）。
        if (res.shell_capture) {
          const label = objectsRef.current.find((o) => o.id === id)?.label ?? "シェル";
          setShellOut({
            label,
            exitCode: res.shell_capture.exit_code,
            output: res.shell_capture.output,
            truncated: res.shell_capture.truncated,
          });
          return;
        }
        // シェルのウィンドウあり起動: ウィンドウ自体がフィードバックなので
        // 「フローを実行しました」ダイアログは出さない（従来は空の報告が出て煩雑だった）。
        if (isShell) return;
        // v1.5.2 WS-C: 実行中にプロジェクトが切り替わっていたら、結果は起動元プロジェクトに
        // materialize されている。このキャンバスの refresh・緑シグナル・UNDO は行わない。
        const samePrj = projectPathRef.current === startPath;
        if (samePrj) {
          if (res.created.length) {
            setTouched((prev) => new Set([...prev, ...res.created]));
            pushUndo([{ kind: "delete", ids: res.created }]);
          }
          // 固定名上書きで中身が変わった既存カード: 緑シグナルのみ（削除Undoはしない）
          if (res.updated.length) {
            setTouched((prev) => new Set([...prev, ...res.updated]));
          }
          await refresh();
        }
        const lines = [
          ...res.outputs.map((o) => `生成: ${o}`),
          ...res.skipped.map((s) => `スキップ: ${s}`),
        ];
        if (!samePrj) {
          lines.push("（結果は起動元プロジェクトに出力されました。開き直すと表示されます）");
        }
        setConfirm({
          title: "フローを実行しました",
          message: lines.join("\n") || "出力はありません",
          warning: res.errors.length ? "エラー:\n" + res.errors.join("\n") : undefined,
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
      } catch (e) {
        console.error("run_flow failed", e);
        setConfirm({
          title: "フローを実行できませんでした",
          message: String(e),
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
      } finally {
        runningCardsRef.current.delete(id);
      }
    },
    [refresh, pushUndo]
  );

  // v1.5.5: タイマ/ウォッチャの稼働/停止トグル（構成中の誤発火防止スイッチ・実機FB）。
  const handleToggleOpPause = useCallback(
    async (id: string) => {
      try {
        await invoke<boolean>("toggle_op_paused", { id });
        await refresh();
      } catch (e) {
        console.error("toggle_op_paused failed", e);
        setConfirm({
          title: "稼働状態を切り替えられませんでした",
          message: String(e),
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
      }
    },
    [refresh]
  );

  // v1.5.5 WS-P2 B（オペ層 §6.2）: 運用前評価。実行せず、このカードから上流に pull で
  // 到達する範囲を静的検査し「<カード名>の評価.md」レポートカードを生成/上書きする。
  const handleEvaluateFlow = useCallback(
    async (id: string) => {
      try {
        const res = await invoke<{
          errors: number;
          warnings: number;
          infos: number;
          report_id: string;
        }>("evaluate_flow", { id });
        setTouched((prev) => new Set([...prev, res.report_id]));
        await refresh();
        const clean = res.errors === 0 && res.warnings === 0 && res.infos === 0;
        setConfirm({
          title: "運用前評価が完了しました",
          message: clean
            ? "問題は見つかりませんでした。評価レポートカードを生成しました。"
            : `エラー ${res.errors}・警告 ${res.warnings}・情報 ${res.infos} 件。詳細は評価レポートカードを開いてください。`,
          warning: res.errors ? "エラーがあります。実行前に確認してください。" : undefined,
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
      } catch (e) {
        console.error("evaluate_flow failed", e);
        setConfirm({
          title: "運用前評価を実行できませんでした",
          message: String(e),
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
      }
    },
    [refresh]
  );

  // フィルタ定義の編集。外部プログラムは設定ダイアログ（v1.5）、
  // 内蔵フィルタは従来の JSON 直編集（バックエンドが保存前に検証する）。
  const handleEditFilter = useCallback(
    (id: string, current: string) => {
      let builtin: string | null = null;
      let isShell = false;
      let opKind: string | null = null;
      try {
        const parsed = JSON.parse(current || "{}");
        const b = parsed.builtin;
        builtin = typeof b === "string" ? b : null;
        isShell = typeof parsed.shell === "string";
        // v1.5.4 WS-P2: op カード（トリガ/タイマ/ウォッチャ）は op キーで見分ける。
        opKind = typeof parsed.op === "string" ? parsed.op : null;
      } catch {
        builtin = null;
      }
      const label = objectsRef.current.find((o) => o.id === id)?.label ?? "";
      if (opKind) {
        setBuiltinDlg({ id, builtin: `op_${opKind}`, label, spec: current });
        return;
      }
      // シェルカード統合（2026-07-06）: shell キーを持つカードは専用ダイアログ。
      // 従来は外部プログラム用ダイアログに流れ、NAME/PATH 検証が赤エラーになっていた。
      if (isShell) {
        setShellDlg({ id, label, spec: current });
        return;
      }
      // v1.4.1 AIカードは専用設定ダイアログ。
      if (builtin === "ai") {
        setAiDlg({ id, label, spec: current });
        return;
      }
      // v1.5 Text-To-Image カードも専用設定ダイアログ（取得済みモデル一覧を添えて開く）。
      if (builtin === "t2i") {
        void (async () => {
          let models: string[] = [];
          let known: string[] = [];
          let assets = { loras: [] as string[], embeddings: [] as string[], vae: [] as string[] };
          try {
            const s = await invoke<T2iStatus>("t2i_status");
            models = s.models;
            known = s.known.map((k) => k.name);
            assets = { loras: s.loras, embeddings: s.embeddings, vae: s.vae };
          } catch {
            /* status 取得不可でも編集は開く */
          }
          setT2iDlg({ id, label, spec: current, models, known, assets });
        })();
        return;
      }
      if (!builtin) {
        setFilterDlg({ x: 0, y: 0, edit: { id, label, spec: current } });
        return;
      }
      // v1.4.29 ⑤: スキーマのあるビルトインは種別別のフォームダイアログで編集。
      if (hasBuiltinSchema(builtin)) {
        setBuiltinDlg({ id, builtin, label, spec: current });
        return;
      }
      // スキーマ未定義の builtin（将来追加分）は生 JSON の直編集にフォールバック。
      // v1.8: 1行の入力欄だと中身が読めず編集もできないので、整形して複数行で開く
      //（フォームのある builtin の「JSONで編集」と同じ見え方に揃える）。
      setPrompt({
        title: "フィルタ定義（JSON）",
        initial: prettyJson(current),
        multiline: true,
        onConfirm: async (spec) => {
          try {
            await invoke("update_filter", { id, spec });
            await refresh();
          } catch (e) {
            console.error("update_filter failed", e);
            setConfirm({
              title: "フィルタ定義を保存できませんでした",
              message: String(e),
              okLabel: "OK",
              onConfirm: () => setConfirm(null),
            });
          }
        },
      });
    },
    [refresh]
  );

  // v1.5: フィルタ設定に名前を付けてライブラリへ登録（同名は上書き）。
  const handleSaveFilterToLibrary = useCallback(
    (id: string) => {
      const obj = objectsRef.current.find((o) => o.id === id);
      const spec = obj?.filter_ref;
      if (!spec) return;
      setPrompt({
        title: "ライブラリに登録: フィルタ名（同名は上書き）",
        initial: obj?.label ?? "",
        onConfirm: async (name) => {
          try {
            await invoke("save_filter_library", { name, spec });
            await reloadFilterLibrary();
          } catch (e) {
            setConfirm({
              title: "ライブラリに登録できませんでした",
              message: String(e),
              okLabel: "OK",
              onConfirm: () => setConfirm(null),
            });
          }
        },
      });
    },
    [reloadFilterLibrary]
  );

  // v1.5: ライブラリのフィルタをカード化（インスタンス化）。配置時にプログラムの所在をチェック。
  const handleAddLibraryFilter = useCallback(
    // メニュー再編（2026-07-12）: 矢印引き出しメニューからも配置できるよう linkFrom 対応。
    async (name: string, x: number, y: number, linkFrom?: string) => {
      const entry = filterLibraryRef.current.find((e) => e.name === name);
      if (!entry) return;
      try {
        const vid = await invoke<string>("create_filter", {
          label: name,
          spec: entry.spec,
          x,
          y,
        });
        pushUndo([{ kind: "delete", ids: [vid] }]);
        await refresh();
        if (linkFrom) await linkNewNode(linkFrom, vid);
        try {
          const s = JSON.parse(entry.spec);
          if (s.name) {
            const found = await invoke<string | null>("check_filter_program", {
              name: s.name,
              path: s.path ?? null,
            });
            if (!found) {
              setConfirm({
                title: "プログラムが見つかりません",
                message:
                  `「${s.name}」が見つかりません（指定パス → .texos\\filters → 環境変数PATH の順で探索）。\n` +
                  "カードは配置しました。右クリック→フィルタ設定で PATH を確認するか、プログラムをこのプロジェクトに同梱してください。",
                okLabel: "OK",
                onConfirm: () => setConfirm(null),
              });
            }
          }
        } catch {
          // spec が壊れていても配置自体は成立させる（編集ダイアログで直せる）
        }
      } catch (e) {
        setConfirm({
          title: "フィルタを配置できませんでした",
          message: String(e),
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
      }
    },
    [refresh, pushUndo, linkNewNode]
  );

  // v1.5.12 メニュー再編: ライブラリ登録名の変更。「カテゴリ/名前」の形にすると
  // メニューがカテゴリに畳まれる（menuBuilder groupBySlash）＝カテゴリ移動もこれ1本。
  const renameLibraryEntry = useCallback(
    async (kind: "launcher" | "filter" | "module", name: string, newName: string) => {
      const nn = newName.trim();
      if (!nn || nn === name) return;
      const cmd =
        kind === "launcher"
          ? "rename_launcher_library"
          : kind === "filter"
          ? "rename_filter_library"
          : "rename_module_library";
      try {
        await invoke(cmd, { old: name, new: nn });
      } catch (e) {
        setConfirm({
          title: "名前を変更できませんでした",
          message: String(e),
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
      }
      if (kind === "launcher") await reloadLauncherLibrary();
      else if (kind === "filter") await reloadFilterLibrary();
      else await reloadModuleLibrary();
    },
    [reloadLauncherLibrary, reloadFilterLibrary, reloadModuleLibrary]
  );
  const handleRenameLibrary = useCallback(
    (kind: "launcher" | "filter" | "module", name: string) => {
      setPrompt({
        title: "新しい名前（「カテゴリ/名前」でメニューを畳めます）",
        initial: name,
        onConfirm: (nn) => void renameLibraryEntry(kind, name, nn),
      });
    },
    [renameLibraryEntry]
  );
  // 既存カテゴリへの移動: 現在のカテゴリ部分を差し替える（category="" は平置きへ）。
  const handleMoveLibraryCategory = useCallback(
    (kind: "launcher" | "filter" | "module", name: string, category: string) => {
      const i = name.indexOf("/");
      const leaf = i > 0 && i < name.length - 1 ? name.slice(i + 1).trim() : name;
      void renameLibraryEntry(kind, name, category ? `${category}/${leaf}` : leaf);
    },
    [renameLibraryEntry]
  );

  // v1.5: ライブラリからエントリを削除（配置済みカードには影響しない）。
  const handleDeleteLibraryFilter = useCallback(
    (name: string) => {
      setConfirm({
        title: "ライブラリから削除",
        message: `「${name}」をライブラリから削除します。\n（配置済みのカードには影響しません）`,
        okLabel: "削除",
        onConfirm: async () => {
          setConfirm(null);
          try {
            await invoke("delete_filter_library", { name });
          } catch (e) {
            console.error("delete_filter_library failed", e);
          }
          await reloadFilterLibrary();
        },
      });
    },
    [reloadFilterLibrary]
  );

  // v1.5.8 WS-M M2（§10.1）: ロック済み図形のメンバーを名前付きモジュールとして
  // 個人ライブラリ（AppData/modules）へ登録。実身は常にコピー同梱（§10.2）・
  // 合計サイズがしきい値超過なら確認してから登録（巨大データ警告）。
  const handleSaveModuleToLibrary = useCallback(
    (shapeId: string) => {
      const sh = shapesRef.current.find((s) => s.id === shapeId);
      if (!sh) return;
      if (!sh.locked || !sh.members.length) {
        setConfirm({
          title: "ライブラリに登録（モジュール）",
          message:
            "先に図形右クリック「メンバーをロック」でメンバーを固定してください。\nライブラリ化はロックした瞬間のスナップショット（所属メンバー）を保存します。",
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
        return;
      }
      const doSave = async (name: string, allowLarge: boolean) => {
        try {
          const res = await invoke<{ saved: boolean; total_bytes: number; cards: number }>(
            "save_module_library",
            { name, shapeId, allowLarge }
          );
          if (!res.saved) {
            // §10.2 巨大データ警告: しきい値超過は確認してから同梱する。
            const mb = (res.total_bytes / (1024 * 1024)).toFixed(1);
            setConfirm({
              title: "モジュールのデータが大きい",
              message: `同梱する実身データの合計が ${mb} MB あります。\nライブラリはアプリ共通の置き場（AppData）にコピーを保持します。登録しますか？`,
              okLabel: "登録する",
              onConfirm: () => {
                setConfirm(null);
                void doSave(name, true);
              },
            });
            return;
          }
          await reloadModuleLibrary();
          setConfirm({
            title: "ライブラリに登録しました",
            message: `モジュール「${name}」（カード ${res.cards} 枚）を登録しました。\nキャンバス右クリック「自動化カードを追加 → ユーザーのモジュール」で、どのプロジェクトにも展開できます。`,
            okLabel: "OK",
            onConfirm: () => setConfirm(null),
          });
        } catch (e) {
          setConfirm({
            title: "ライブラリに登録できませんでした",
            message: String(e),
            okLabel: "OK",
            onConfirm: () => setConfirm(null),
          });
        }
      };
      setPrompt({
        title: "ライブラリに登録: モジュール名（同名は上書き）",
        initial: sh.label || "モジュール",
        onConfirm: (name) => void doSave(name, false),
      });
    },
    [reloadModuleLibrary]
  );

  // v1.5.8 WS-M M2: ライブラリのモジュールをキャンバスへ展開（§8.5 透過＝インライン展開）。
  // ID は全再割当・実身はプロジェクト CAS へ・図形はロック済みで置かれる。
  // 未導入の依存があれば注意カード（付箋）が添えられる（§10.3 初回アテンション）。
  const handleAddLibraryModule = useCallback(
    async (name: string, x: number, y: number) => {
      try {
        const res = await invoke<{
          new_card_ids: string[];
          shape_id: string;
          note_id: string | null;
          missing: string[];
        }>("place_module_from_library", { name, x, y });
        await refresh();
        // 展開したカードを選択状態で迎える（send_selection の受け入れと同じ流儀）。
        setSelected(new Set(res.new_card_ids));
        setSelectedNotes(new Set());
        if (res.missing.length) {
          setConfirm({
            title: "モジュールを展開しました（依存に注意）",
            message:
              `未導入の依存があります:\n${res.missing.map((s) => `・${s}`).join("\n")}\n\n` +
              "注意カード（付箋）を図形の横に添えました。導入後はそのまま実行できます。",
            okLabel: "OK",
            onConfirm: () => setConfirm(null),
          });
        }
      } catch (e) {
        setConfirm({
          title: "モジュールを展開できませんでした",
          message: String(e),
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
      }
    },
    [refresh]
  );

  // v1.5.8 WS-M M2: ライブラリからモジュールを削除（展開済みのカードには影響しない）。
  const handleDeleteLibraryModule = useCallback(
    (name: string) => {
      setConfirm({
        title: "ライブラリから削除",
        message: `モジュール「${name}」をライブラリから削除します（同梱データのコピーも消えます）。\n（展開済みのカードには影響しません）`,
        okLabel: "削除",
        onConfirm: async () => {
          setConfirm(null);
          try {
            await invoke("delete_module_library", { name });
          } catch (e) {
            console.error("delete_module_library failed", e);
          }
          await reloadModuleLibrary();
        },
      });
    },
    [reloadModuleLibrary]
  );

  // v1.5.8 WS-M M2（T3 決着）: 全個人ライブラリをプロジェクトへ書き出し（バックアップ代用）。
  const handleExportLibraries = useCallback(() => {
    setConfirm({
      title: "個人ライブラリをプロジェクトへ書き出し",
      message:
        "アプリ共通の個人ライブラリ（ユーザーのフィルタ・ランチャー・モジュール）の写しを、このプロジェクトの「ライブラリバックアップ」フォルダへ書き出します。\n（既存の「ライブラリバックアップ」フォルダは上書きされます）",
      okLabel: "書き出す",
      onConfirm: async () => {
        setConfirm(null);
        try {
          const res = await invoke<{
            path: string;
            filters: number;
            launchers: number;
            modules: number;
          }>("export_libraries_to_project");
          setConfirm({
            title: "書き出しました",
            message: `フィルタ ${res.filters} 件・ランチャー ${res.launchers} 件・モジュール ${res.modules} 件\n→ ${res.path}`,
            okLabel: "OK",
            onConfirm: () => setConfirm(null),
          });
        } catch (e) {
          setConfirm({
            title: "書き出せませんでした",
            message: String(e),
            okLabel: "OK",
            onConfirm: () => setConfirm(null),
          });
        }
      },
    });
  }, []);

  // v1.5.9: ライブラリバックアップから取り込み（読み戻し）。バックアップの一覧を取得し、
  // 項目クリックで1件ずつ個人ライブラリへ戻すサブメニューを開く（keepOpen・✓=取り込み済み・
  // 同名上書きは事前にラベルで知らせる）。運用: 別PCへの移植・自分のライブラリを壊したときの復元。
  const handleImportLibraries = useCallback(
    async (openSubmenu: (items: MenuItem[]) => void) => {
      type Entry = { name: string; exists: boolean };
      let list: { filters: Entry[]; launchers: Entry[]; modules: Entry[] };
      try {
        list = await invoke("list_library_backup");
      } catch (e) {
        setConfirm({
          title: "取り込めません",
          message: String(e),
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
        return;
      }
      const total = list.filters.length + list.launchers.length + list.modules.length;
      if (!total) {
        setConfirm({
          title: "ライブラリバックアップから取り込み",
          message: "「ライブラリバックアップ」フォルダはありますが、中身が空です。",
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
        return;
      }
      // 取り込み済みマーク（このサブメニューを開いている間だけの状態）。
      const done = new Set<string>();
      const importOne = async (kind: string, name: string) => {
        try {
          await invoke("import_library_backup_entry", { kind, name });
          done.add(`${kind}:${name}`);
        } catch (e) {
          console.error("import_library_backup_entry failed", e);
        }
      };
      const reloadAll = () =>
        Promise.all([reloadFilterLibrary(), reloadLauncherLibrary(), reloadModuleLibrary()]);
      const build = (): MenuItem[] => {
        const item = (kind: string, icon: string, e: Entry): MenuItem => ({
          label: done.has(`${kind}:${e.name}`)
            ? `✓ ${icon} ${e.name}（取り込み済み）`
            : `　${icon} ${e.name}${e.exists ? "（同名を上書き）" : ""}`,
          keepOpen: true,
          onClick: async () => {
            await importOne(kind, e.name);
            await reloadAll();
            openSubmenu(build());
          },
        });
        const section = (title: string, kind: string, icon: string, entries: Entry[]): MenuItem[] =>
          entries.length
            ? [
                { label: `── ${title} ──`, onClick: () => {}, disabled: true },
                ...entries.map((e) => item(kind, icon, e)),
              ]
            : [];
        return [
          ...section("フィルタ", "filter", "⚗", list.filters),
          ...section("ランチャー", "launcher", "🚀", list.launchers),
          ...section("モジュール", "module", "📦", list.modules),
          { divider: true, label: "", onClick: () => {} },
          {
            label: `すべて取り込み（${total}件）`,
            keepOpen: true,
            onClick: async () => {
              for (const e of list.filters) await importOne("filter", e.name);
              for (const e of list.launchers) await importOne("launcher", e.name);
              for (const e of list.modules) await importOne("module", e.name);
              await reloadAll();
              openSubmenu(build());
            },
          },
          { label: "OK（閉じる）", onClick: () => {} },
        ];
      };
      openSubmenu(build());
    },
    [reloadFilterLibrary, reloadLauncherLibrary, reloadModuleLibrary]
  );

  // ドロップポイントから flow 矢印を下流へ辿り、末端のフィルタ（その先に
  // フィルタが繋がっていないもの）を実行する。中間のフィルタ連鎖は run_flow の
  // pull 評価がメモリ上で通過させる（中間ファイル・中間カードは生まれない）。
  const runDownstreamFlows = useCallback(
    async (startId: string) => {
      const isFilterId = (vid: string) => {
        const o = objectsRef.current.find((x) => x.id === vid);
        return !!o && (o.kind === "filter" || !!o.filter_ref);
      };
      const rels = await invoke<RelationInfo[]>("list_relations");
      const terminals = new Set<string>();
      const queue = [startId];
      const seen = new Set<string>([startId]);
      while (queue.length) {
        const cur = queue.shift()!;
        const outs = rels.filter(
          (r) => r.kind === "flow" && r.from_id === cur && isFilterId(r.to_id)
        );
        if (!outs.length && cur !== startId && isFilterId(cur)) terminals.add(cur);
        for (const r of outs) {
          if (!seen.has(r.to_id)) {
            seen.add(r.to_id);
            queue.push(r.to_id);
          }
        }
      }
      if (!terminals.size) {
        setConfirm({
          title: "フローがありません",
          message: "ドロップポイントからフィルタへ矢印を引いてください",
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
        return;
      }
      for (const t of terminals) await handleRunFlow(t);
    },
    [handleRunFlow]
  );
  runDownstreamFlowsRef.current = runDownstreamFlows;

  // ドロップポイントへファイルを選んで投入（D&Dの代替。ダブルクリックでも呼ぶ）。
  const handleFeedDrop = useCallback(
    async (id: string) => {
      const file = await open({ multiple: false, title: "投入するファイルを選択（下流のフローを自動実行）" });
      if (!file || Array.isArray(file)) return;
      try {
        await invoke("drop_into_point", { id, path: file });
        await refresh();
        await runDownstreamFlows(id);
      } catch (e) {
        console.error("drop_into_point failed", e);
        setConfirm({
          title: "投入できませんでした",
          message: String(e),
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
      }
    },
    [refresh, runDownstreamFlows]
  );

  // Rename = change the virtual body's display label only. 実身(CAS)は不変。
  const handleRenameObject = useCallback((id: string, current: string) => {
    setPrompt({
      title: "表示名を変更（実身は変更されません）",
      initial: current,
      onConfirm: async (label) => {
        try {
          await invoke("rename_object", { id, label });
          setObjects((prev) => prev.map((o) => (o.id === id ? { ...o, label } : o)));
        } catch (e) {
          console.error("rename_object failed", e);
        }
      },
    });
  }, []);

  // v1.3.10 TexoS内リネーム: 実身の実ファイルを改名（committed_path/label/ext を更新）。
  // カードIDは不変なので、この実身を指す仮身（window_of）は全部自動追従する。
  const handleRenameReal = useCallback(
    (id: string, current: string) => {
      setPrompt({
        title: "実ファイル名の変更（実体ファイルを改名・表示名は変わりません）",
        initial: current,
        onConfirm: async (name) => {
          try {
            await invoke("rename_real", { id, newName: name });
            await refresh();
          } catch (e) {
            console.error("rename_real failed", e);
            setConfirm({
              title: "リネームできませんでした",
              message: String(e),
              okLabel: "OK",
              onConfirm: () => setConfirm(null),
            });
          }
        },
      });
    },
    [refresh]
  );

  const handleDeleteObject = useCallback(async (id: string) => {
    // v1.5.6 WS-M M1（§8.1 メンバー保全・実機FB）: ロック中図形のメンバーは削除も不可。
    // ここが削除の要衝（右クリック削除・Del キー単体が全部通る）＝入口で一括ガード。
    const obj = objectsRef.current.find((o) => o.id === id);
    if (obj) {
      const lockShape = lockedMemberShape(obj.id);
      if (lockShape) {
        setConfirm({
          title: "モジュールがロックされています",
          message: `このカードは 🔒「${lockShape.label || "図形"}」のメンバーです。削除するには、先に図形メニューの「メンバーをロック」を解除してください。`,
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
        return;
      }
    }
    try {
      await invoke("delete_object", { id });
      // 復活の呪文: 削除はソフトデリートなのでCtrl+Zで矢印リンクごと復活する。
      pushUndo([{ kind: "restore", ids: [id] }]);
      setObjects((prev) => prev.filter((o) => o.id !== id));
      setRelations((prev) => prev.filter((r) => r.from_id !== id && r.to_id !== id));
    } catch (e) {
      console.error("delete_object failed", e);
    }
  }, [pushUndo, lockedMemberShape]);

  // Confirm before deleting a committed object (its real file is removed too).
  const handleRequestDelete = useCallback(
    (id: string) => {
      const obj = objectsRef.current.find((o) => o.id === id);
      if (obj?.committed_path) {
        setConfirm({
          title: "削除しますか？",
          message:
            "このオブジェクトと、プロジェクトフォルダ内の実身ファイルを削除します。\n（CAS内の元データは保持されます）",
          okLabel: "削除",
          onConfirm: () => {
            setConfirm(null);
            handleDeleteObject(id);
          },
        });
      } else {
        handleDeleteObject(id);
      }
    },
    [handleDeleteObject]
  );

  // Double-click / 開く. Project objects navigate; files commit+open.
  // v1.3.9 ①: ダブルクリック＝閲覧（非破壊）。実身化・実行・編集は一切しない。
  // 「そのカードが何であるかを、状態を変えずに覗く」。重い動詞（編集／実行）は明示操作。
  const handleOpenObject = useCallback(
    async (id: string) => {
      const obj = objectsRef.current.find((o) => o.id === id);
      // v1.3.11 ランチャー: ダブルクリック＝起動（ランチャーの主目的は起動）。
      // filter_ref を持つため、フィルタ判定より先に処理する。
      if (obj?.kind === "launcher") {
        handleRunLauncher(id);
        return;
      }
      // v1.4.1 AI対話カード: 覗く＝チャットパネルをトグル（設定は右クリックメニュー）。
      // v1.4.17 画像生成カードも同じ流儀＝ダブルクリックでインライン生成パネルをトグル。
      if (obj?.filter_ref) {
        try {
          const s = JSON.parse(obj.filter_ref);
          if ((s.builtin === "ai" && s.mode === "chat") || s.builtin === "t2i") {
            const isOpen = expandedRef.current.has(id);
            invoke("set_expanded", { id, expanded: !isOpen }).catch(() => {});
            setExpanded((prev) => {
              const next = new Set(prev);
              if (isOpen) next.delete(id);
              else next.add(id);
              return next;
            });
            return;
          }
        } catch {
          /* 壊れたJSONは下のフィルタ設定へ流す */
        }
      }
      // フィルタカード: 覗く＝定義を見る → 設定を開く（実行はメニュー／▶ボタンで明示）。
      if (obj && (obj.kind === "filter" || obj.filter_ref)) {
        handleEditFilter(id, obj.filter_ref ?? "{}");
        return;
      }
      // ドロップポイント: ファイルダイアログで明示的に投入（誤爆しない）。
      if (obj?.kind === "drop") {
        handleFeedDrop(id);
        return;
      }
      // プロジェクト仮身: 入る（ナビゲーション・非破壊）。
      if (obj?.project_ref) {
        try {
          const info = await invoke<ProjectInfo>("open_project_object", { id });
          setProject(info);
        } catch (e) {
          console.error("open_project_object failed", e);
          setConfirm({
            title: "プロジェクトを開けませんでした",
            message: String(e),
            okLabel: "OK",
            onConfirm: () => setConfirm(null),
          });
        }
        return;
      }
      // URL link → open in default browser.
      if (obj?.url_ref) {
        invoke("open_with_default", { path: obj.url_ref }).catch((e) =>
          console.error("open url failed", e)
        );
        return;
      }
      // Folder link → open the folder in Explorer.
      if (obj?.folder_ref) {
        invoke("open_with_default", { path: obj.folder_ref }).catch((e) =>
          console.error("open folder failed", e)
        );
        return;
      }
      // インラインプレビューがある種別はキャンバス内で閲覧（仮身のまま・CAS不変）。
      // v1.3.11 #13: オフィス系（docx=mammoth / 表計算=SheetJS）も DOM プレビュー対象。
      const DOC_PREVIEW_EXTS = [
        "docx", "xlsx", "xls", "ods", "csv", "tsv",
        "pptx", "ppt", "doc", "odt", "odp",
      ];
      if (
        obj &&
        (obj.mime_type.startsWith("image/") ||
          obj.mime_type.startsWith("audio/") ||
          obj.mime_type.startsWith("video/") ||
          obj.mime_type === "application/pdf" ||
          (obj.ext ?? "").toLowerCase() === "pdf" ||
          DOC_PREVIEW_EXTS.includes((obj.ext ?? "").toLowerCase()) ||
          // v2.0 t23d Phase 2: glb/gltf は three.js のインライン 3D ビューア。
          ["glb", "gltf"].includes((obj.ext ?? "").toLowerCase()) ||
          obj.mime_type.startsWith("model/gltf") ||
          isTextMime(obj.mime_type, obj.ext))
      ) {
        // v1.3.10: ダブルクリックはプレビューのトグル（再度ダブルクリックで閉じる）。
        // 旧「ダブルクリック＝実身にする」の名残で×ボタンだけが閉じる手段だった。
        // テキストは未コミットなら読取専用、実身なら編集パネル（ObjectCard が分岐）。
        const isOpen = expandedRef.current.has(id);
        invoke("set_expanded", { id, expanded: !isOpen }).catch(() => {});
        setExpanded((prev) => {
          const next = new Set(prev);
          if (isOpen) next.delete(id);
          else next.add(id);
          return next;
        });
        return;
      }
      // インラインプレビューが無い種別（docx 等）。
      // v1.3.10 補完: 実身（committed_path あり）は**プロジェクトの実ファイルをそのまま開く**
      // ＝編集が直接プロジェクトに保存される。一時コピー(texos_view)を開くと編集が破棄され
      // 保存できない（ユーザー指摘）。仮身は従来どおり一時的閲覧（非破壊・破棄）でよい。
      if (obj?.committed_path) {
        invoke("open_with_default", { path: obj.committed_path }).catch((e) =>
          console.error("open committed failed", e)
        );
      } else {
        invoke("view_temp", { id }).catch((e) => console.error("view_temp failed", e));
      }
    },
    [handleEditFilter, handleFeedDrop, handleRunLauncher]
  );

  // Place a reference to an existing project as a project-object card.
  // 既存 TexoS プロジェクトを project_ref カードとして配置（パス指定）。linkFrom があれば繋ぐ。
  const linkProjectByPath = useCallback(
    async (path: string, canvasX: number, canvasY: number, linkFrom?: string) => {
      try {
        const vid = await invoke<string>("link_project", { path, x: canvasX, y: canvasY });
        pushUndo([{ kind: "delete", ids: [vid] }]);
        await refresh();
        if (linkFrom) await linkNewNode(linkFrom, vid);
      } catch (e) {
        console.error("link_project failed", e);
        setConfirm({
          title: "リンクできませんでした",
          message: String(e),
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
      }
    },
    [refresh, pushUndo, linkNewNode]
  );

  const handleLinkProject = useCallback(
    async (canvasX: number, canvasY: number, linkFrom?: string) => {
      const dir = await open({ directory: true, title: "リンクするTexoSプロジェクトを選択" });
      if (!dir || Array.isArray(dir)) return;
      await linkProjectByPath(dir, canvasX, canvasY, linkFrom);
    },
    [linkProjectByPath]
  );

  // Add a URL link card (prompt URL, then title).
  const handleAddUrl = useCallback(
    (canvasX: number, canvasY: number, linkFrom?: string) => {
      setPrompt({
        title: "URLを入力",
        initial: "https://",
        onConfirm: (url) => {
          setPrompt({
            title: "タイトルを入力",
            initial: url,
            onConfirm: async (title) => {
              try {
                const vid = await invoke<string>("create_url", { title, url, x: canvasX, y: canvasY });
                pushUndo([{ kind: "delete", ids: [vid] }]);
                await refresh();
                if (linkFrom) await linkNewNode(linkFrom, vid);
              } catch (e) {
                console.error("create_url failed", e);
              }
            },
          });
        },
      });
    },
    [refresh, pushUndo, linkNewNode]
  );

  // メニュー再編（2026-07-12）: 「新規ファイルを作成」= 型（拡張子）を選び、名前を付けて
  // 空の仮身カードを作る（テキストカード統合・Windows の新規作成メニュー方式）。CAS 実身
  // なので未保存でも消えない。表示名「名前.拡張子」＝実ファイル名の予約（コミット時に
  // その名で書き出し）。名前が拡張子付きで入力されても二重にしない。
  const createEmptyFileAt = useCallback(
    async (ext: string, name: string, canvasX: number, canvasY: number, linkFrom?: string) => {
      const e2 = ext.trim().replace(/^\./, "").toLowerCase();
      if (!e2) return;
      const base = name.trim().replace(new RegExp(`\\.${e2}$`, "i"), "");
      const label = `${base || "新規"}.${e2}`;
      try {
        const vid = await invoke<string>("create_empty_object", {
          ext: e2,
          label,
          x: canvasX,
          y: canvasY,
        });
        setTouched((prev) => new Set(prev).add(vid));
        pushUndo([{ kind: "delete", ids: [vid] }]);
        await refresh();
        if (linkFrom) await linkNewNode(linkFrom, vid);
      } catch (e) {
        console.error("create_empty_object failed", e);
        setConfirm({
          title: "新規ファイルを作成できませんでした",
          message: String(e),
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
      }
    },
    [refresh, pushUndo, linkNewNode]
  );

  // 定番型（テキスト〜HTML）: 名前だけ聞いて拡張子を付けて作成。空欄OK＝「新規.拡張子」。
  const handleCreateEmptyFile = useCallback(
    (ext: string, canvasX: number, canvasY: number, linkFrom?: string) => {
      setPrompt({
        title: `新規ファイルの名前（.${ext} を付けて作成・空欄なら「新規.${ext}」）`,
        initial: "",
        allowEmpty: true,
        onConfirm: (name) => void createEmptyFileAt(ext, name, canvasX, canvasY, linkFrom),
      });
    },
    [createEmptyFileAt]
  );

  // 「その他の拡張子...」: 名前欄＋拡張子欄の2欄プロンプト（拡張子が空欄ならキャンセル扱い）。
  const handleCreateEmptyFileOther = useCallback(
    (canvasX: number, canvasY: number, linkFrom?: string) => {
      setPrompt({
        title: "新規ファイルを作成（その他の拡張子）",
        label: "名前（空欄なら「新規」）",
        initial: "",
        allowEmpty: true,
        field2: { label: "拡張子（例: py, ps1, yaml）", initial: "" },
        onConfirm: (name, ext) => {
          if (ext) void createEmptyFileAt(ext, name, canvasX, canvasY, linkFrom);
        },
      });
    },
    [createEmptyFileAt]
  );

  // v1.3.11 修正（#8/#9）: 拡張子だけ指定した空ファイルは「正式なファイル」にならない
  // （docx 等は中身が無いと壊れ、実質テキスト）。そこで OS の「新規作成」を使う方式に変更:
  // プロジェクトフォルダを既定にした配置ダイアログを開き、ダイアログ内の右クリック→新規作成で
  // 正しい型のファイルを作って選ぶ（または既存ファイルを選ぶ）。選んだファイルがプロジェクト内
  // なら実身として取り込まれる（path_inside 判定）。配置位置はクリック位置を維持する。
  // メニュー再編（2026-07-12）: この方式は「新規ファイルを作成 ...」サブメニュー末尾の
  // 「Office文書など（OSのダイアログで作成）...」として残す（空だと壊れる形式の受け皿）。
  const handleNewFile = useCallback(
    async (canvasX: number, canvasY: number, linkFrom?: string) => {
      try {
        const sel = await open({
          multiple: false,
          defaultPath: project?.path,
          title: "新規ファイルを作成（ダイアログ内で右クリック→新規作成）または選択",
        });
        if (typeof sel !== "string") return;
        const vid = await invoke<string>("import_file", {
          path: sel,
          label: "",
          x: canvasX,
          y: canvasY,
        });
        setTouched((prev) => new Set(prev).add(vid));
        pushUndo([{ kind: "delete", ids: [vid] }]);
        await refresh();
        if (linkFrom) await linkNewNode(linkFrom, vid);
      } catch (e) {
        console.error("new file (place) failed", e);
        setConfirm({
          title: "新規ファイルを作成できませんでした",
          message: String(e),
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
      }
    },
    [refresh, pushUndo, linkNewNode, project]
  );

  const handleSetMemo = useCallback(
    (id: string, current: string) => {
      setPrompt({
        title: "コメント（カードに表示）",
        initial: current,
        onConfirm: async (memo) => {
          try {
            await invoke("set_memo", { id, memo });
            setObjects((prev) => prev.map((o) => (o.id === id ? { ...o, memo } : o)));
          } catch (e) {
            console.error("set_memo failed", e);
          }
        },
      });
    },
    []
  );

  const handleChangeUrl = useCallback(
    (id: string, current: string) => {
      setPrompt({
        title: "URLを変更",
        initial: current,
        onConfirm: async (url) => {
          try {
            await invoke("update_url", { id, url });
            await refresh();
          } catch (e) {
            console.error("update_url failed", e);
          }
        },
      });
    },
    [refresh]
  );

  // v1.7 P2 URLカードの仮身昇格: 「更新を確認」= 条件付きGET→比較→破棄（マークのみ・
  // 実身は作らない）。結果はダイアログで知らせ、更新マークは refresh で反映される。
  const handleCheckUrlUpdate = useCallback(
    async (id: string) => {
      try {
        const r = await invoke<{ updated: boolean; message: string }>("web_check_update", { id });
        await refresh();
        setConfirm({
          title: r.updated ? "更新があります" : "更新を確認",
          message: r.message,
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
      } catch (e) {
        setConfirm({
          title: "更新の確認に失敗しました",
          message: String(e),
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
      }
    },
    [refresh]
  );

  // 「再取得」= 取得→内容が変わっていれば新しい実身カード＋取得系譜の更新（前の実身は
  // 残る）。変化なしなら実身は作らない（304/同一ハッシュ・実機FB 2026-07-17）。
  // 未取得なら初回取得。
  const handleRefetchUrl = useCallback(
    async (id: string) => {
      try {
        const r = await invoke<{ created: string | null; message: string }>("web_refetch", { id });
        await refresh();
        // 新カードが増えたときは画面の変化で分かる。作らなかったときだけ理由を告げる。
        if (!r.created) {
          setConfirm({
            title: "再取得",
            message: r.message,
            okLabel: "OK",
            onConfirm: () => setConfirm(null),
          });
        }
      } catch (e) {
        setConfirm({
          title: "再取得に失敗しました",
          message: String(e),
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
      }
    },
    [refresh]
  );

  // 一括「このプロジェクトのURLカードの更新を確認」（対象=取得済みのURLカードのみ）。
  const handleCheckAllUrlUpdates = useCallback(async () => {
    try {
      const r = await invoke<{
        checked: number;
        updated: number;
        skipped: number;
        errors: string[];
      }>("web_check_update_all");
      await refresh();
      const lines = [
        `確認 ${r.checked} 枚 / 更新あり ${r.updated} 枚`,
        ...(r.skipped ? [`未取得のためスキップ: ${r.skipped} 枚（web_fetch か「再取得」で実身化すると対象になります）`] : []),
        ...(r.errors.length ? ["", "エラー:", ...r.errors] : []),
      ];
      setConfirm({
        title: "URLカードの更新を確認",
        message: lines.join("\n"),
        okLabel: "OK",
        onConfirm: () => setConfirm(null),
      });
    } catch (e) {
      setConfirm({
        title: "更新の確認に失敗しました",
        message: String(e),
        okLabel: "OK",
        onConfirm: () => setConfirm(null),
      });
    }
  }, [refresh]);

  // Create a new sub/sibling project and place a project-object card.
  const handleNewSubproject = useCallback(
    (sibling: boolean, canvasX: number, canvasY: number) => {
      setPrompt({
        title: sibling
          ? "新規プロジェクト名（分岐・兄弟フォルダ）"
          : "新規プロジェクト名（内包・サブフォルダ）",
        initial: sibling ? "新しいプロジェクト" : "サブプロジェクト",
        onConfirm: async (label) => {
          try {
            const vid = await invoke<string>("create_subproject", {
              label, x: canvasX, y: canvasY, sibling,
            });
            // UNDOで消えるのは参照カードのみ（作成されたフォルダは残る）。
            pushUndo([{ kind: "delete", ids: [vid] }]);
            await refresh();
          } catch (e) {
            console.error("create_subproject failed", e);
            setConfirm({
              title: "プロジェクトを作成できませんでした",
              message: String(e),
              okLabel: "OK",
              onConfirm: () => setConfirm(null),
            });
          }
        },
      });
    },
    [refresh, pushUndo]
  );

  const handleRecommit = useCallback(
    (id: string) => {
      setConfirm({
        title: "実身化し直す（編集を破棄）",
        message:
          "プロジェクトフォルダの編集済みファイルを、元の実身（不変）で上書きします。編集内容は失われます。よろしいですか？",
        okLabel: "実身化し直す",
        onConfirm: async () => {
          setConfirm(null);
          try {
            await invoke("commit_object", { id, force: true });
            // The restore bumps the file's mtime; re-baseline so it isn't
            // flagged as a session change (edits were discarded, not made).
            mtimeBaseRef.current.delete(id);
            await refresh();
            setTouched((prev) => {
              const next = new Set(prev);
              next.delete(id);
              return next;
            });
          } catch (e) {
            console.error("recommit failed", e);
          }
        },
      });
    },
    [refresh]
  );

  // v1.3.9 問題点2: 実体喪失の実身を、移動/改名された正しいファイルへ繋ぎ直す。
  // committed_path だけ差し替え（real_id・CASは不変）。自動CASロールバックの代替。
  const handleRelink = useCallback(
    async (id: string) => {
      try {
        const sel = await open({
          multiple: false,
          title: "再リンクする実ファイルを選択",
          defaultPath: project?.path,
        });
        if (typeof sel !== "string") return;
        await invoke("relink_object", { id, newPath: sel });
        mtimeBaseRef.current.delete(id);
        await refresh();
      } catch (e) {
        console.error("relink failed", e);
      }
    },
    [refresh, project]
  );

  // v1.3.11 #12: リンクフォルダの参照先を選び直す（リンク切れの繋ぎ直し）。フォルダ名が
  // 変更された想定で、元のリンク先の「親フォルダ」をデフォルトで開く（旧仕様はデスクトップ）。
  const handleRelinkFolder = useCallback(
    async (id: string, currentPath: string) => {
      const parent = currentPath ? currentPath.replace(/[\\/][^\\/]*$/, "") : undefined;
      try {
        const sel = await open({
          directory: true,
          title: "リンクし直すフォルダを選択",
          defaultPath: parent || project?.path,
        });
        if (typeof sel !== "string") return;
        await invoke("relink_folder", { id, newPath: sel });
        await refresh();
      } catch (e) {
        console.error("relink_folder failed", e);
      }
    },
    [refresh, project]
  );

  // v1.8.8: プロジェクトカードの参照先を選び直す（フォルダ整理で移動・改名したプロジェクト
  // への繋ぎ直し）。既定で元の参照先の親フォルダを開く（handleRelinkFolder と同じ想定）。
  // 選んだ先が TexoS プロジェクトでなければバックエンドが拒否するので、その旨を表示する。
  const handleRelinkProject = useCallback(
    async (id: string, currentPath: string) => {
      const parent = currentPath ? currentPath.replace(/[\\/][^\\/]*$/, "") : undefined;
      try {
        const sel = await open({
          directory: true,
          title: "リンクし直すTexoSプロジェクトのフォルダを選択",
          defaultPath: parent || project?.path,
        });
        if (typeof sel !== "string") return;
        await invoke("relink_project", { id, newPath: sel });
        await refresh();
      } catch (e) {
        console.error("relink_project failed", e);
        setConfirm({
          title: "リンクし直せませんでした",
          message: String(e),
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
      }
    },
    [refresh, project]
  );

  // v1.8.8: 現在のプロジェクトフォルダ直下のフォルダ（内包プロジェクト／普通のフォルダ）へ
  // リンクカードを置く。import_file はフォルダが TexoS プロジェクトなら project_ref、
  // そうでなければ folder_ref のカードを作る（配置・ドロップと同じ判定）。
  // 複数置くときは起点から格子状に並べ、まとめて1回の UNDO で消せるようにする。
  const placeInnerFolders = useCallback(
    async (paths: string[], canvasX: number, canvasY: number) => {
      const COLS = 4;
      const DX = 170;
      const DY = 120;
      const created: string[] = [];
      const errors: string[] = [];
      for (let i = 0; i < paths.length; i++) {
        const x = canvasX + (i % COLS) * DX;
        const y = canvasY + Math.floor(i / COLS) * DY;
        try {
          created.push(await invoke<string>("import_file", { path: paths[i], label: "", x, y }));
        } catch (e) {
          errors.push(`${paths[i]}: ${String(e)}`);
        }
      }
      if (created.length) pushUndo([{ kind: "delete", ids: created }]);
      await refresh();
      if (errors.length) {
        setConfirm({
          title: "一部のフォルダをリンクできませんでした",
          message: errors.join("\n"),
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
      }
    },
    [refresh, pushUndo]
  );

  // v1.3.9 問題点2: 正しいファイルが無いとき、CASスナップショットから実体を書き出す。
  const handleRestoreFromCas = useCallback(
    (id: string) => {
      setConfirm({
        title: "CASから復元",
        message:
          "取り込み時点のスナップショット（CAS）から実体ファイルを書き出して復元します。リネーム／移動した最新版がある場合は、先に「実ファイルを再リンク」を使ってください。",
        okLabel: "CASから復元",
        onConfirm: async () => {
          setConfirm(null);
          try {
            await invoke("commit_object", { id, force: true });
            mtimeBaseRef.current.delete(id);
            await refresh();
            setTouched((prev) => {
              const next = new Set(prev);
              next.delete(id);
              return next;
            });
          } catch (e) {
            console.error("restore from cas failed", e);
          }
        },
      });
    },
    [refresh]
  );

  // v1.3.10 補完: タイトル(ラベル)ダブルクリックは実身・仮身とも「表示名を変更」に統一。
  // 実身だけ実ファイル改名になると混乱する（UIの気づき2 問題点2）。実ファイル名の変更は
  // 右クリックメニュー（表示名の下）から行う。
  const handleRenameTitle = useCallback(
    (id: string) => {
      const obj = objectsRef.current.find((o) => o.id === id);
      if (!obj) return;
      handleRenameObject(id, obj.label);
    },
    [handleRenameObject]
  );

  // 全コミット: materialize every object so the project folder can leave
  // TexoS as a plain Windows folder. External references get a red warning.
  const handleCommitAll = useCallback(async () => {
    const kindLabel = (k: string) =>
      k === "url"
        ? "URLリンク"
        : k === "external_project"
        ? "外部プロジェクト"
        : k === "folder"
        ? "リンクフォルダ"
        : k === "filter"
        ? "フィルタカード"
        : k === "drop"
        ? "ドロップポイント"
        : "プログラム";
    try {
      const plan = await invoke<CommitAllReport>("commit_all", { dryRun: true });
      const warning = plan.warnings.length
        ? "フォルダに含められない・注意が必要な項目:\n" +
          plan.warnings.map((w) => `・${w.label}（${kindLabel(w.kind)}）`).join("\n")
        : undefined;
      setConfirm({
        title: "すべて実身化（持ち出し）",
        message:
          `${plan.committed.length} 件を実身化し、` +
          `プロジェクトフォルダを単体で持ち出せる状態にします。\n` +
          `（実身化済み: ${plan.already} 件）`,
        warning,
        okLabel: "すべて実身化",
        onConfirm: async () => {
          setConfirm(null);
          try {
            const res = await invoke<CommitAllReport>("commit_all", { dryRun: false });
            await refresh();
            const errText = res.errors.length
              ? `\n\n失敗 ${res.errors.length} 件:\n` + res.errors.join("\n")
              : "";
            setConfirm({
              title: "すべて実身化 完了",
              message: `${res.committed.length} 件を実身化しました（既存 ${res.already} 件）。${errText}`,
              warning: res.warnings.length
                ? "含められなかった・注意項目:\n" +
                  res.warnings.map((w) => `・${w.label}（${kindLabel(w.kind)}）`).join("\n")
                : undefined,
              okLabel: "OK",
              onConfirm: () => setConfirm(null),
            });
          } catch (e) {
            console.error("commit_all failed", e);
            setConfirm({
              title: "すべて実身化に失敗しました",
              message: String(e),
              okLabel: "OK",
              onConfirm: () => setConfirm(null),
            });
          }
        },
      });
    } catch (e) {
      console.error("commit_all (dry run) failed", e);
    }
  }, [refresh]);

  // 既定のアプリで開く: commit, then open with the OS default app (always external).
  const handleOpenDefault = useCallback(
    async (id: string) => {
      try {
        const wasCommitted = !!objectsRef.current.find((o) => o.id === id)?.committed_path;
        const path = await invoke<string>("commit_object", { id });
        await refresh();
        if (!wasCommitted) setTouched((prev) => new Set(prev).add(id));
        await invoke("open_with_default", { path });
      } catch (e) {
        console.error("open_with_default failed", e);
        setConfirm({
          title: "開けませんでした",
          message: String(e),
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
      }
    },
    [refresh]
  );

  // コミットしてプロジェクトフォルダで表示: 仮身を実身化し、Explorerでフォーカス。
  // そこから他アプリへのドラッグ等は Windows 任せ（ピックアップの代替）。
  const handleCommitReveal = useCallback(
    async (id: string) => {
      try {
        const path = await invoke<string>("commit_object", { id });
        await refresh();
        setTouched((prev) => new Set(prev).add(id)); // 新規コミット → 緑
        await invoke("reveal_in_dir", { path });
      } catch (e) {
        console.error("commit/reveal failed", e);
        setConfirm({
          title: "実身化できませんでした",
          message: String(e),
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
      }
    },
    [refresh]
  );

  // アプリを選択して開く／編集: commit, then show the OS "Open with" picker.
  const handleOpenWith = useCallback(
    async (id: string) => {
      try {
        const wasCommitted = !!objectsRef.current.find((o) => o.id === id)?.committed_path;
        const path = await invoke<string>("commit_object", { id });
        await refresh();
        if (!wasCommitted) setTouched((prev) => new Set(prev).add(id));
        await invoke("open_with_chooser", { path });
      } catch (e) {
        console.error("open_with_chooser failed", e);
        setConfirm({
          title: "開けませんでした",
          message: String(e),
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
      }
    },
    [refresh]
  );

  // キャンバスで編集（テキスト）: commit, then expand an inline editor on the card.
  const handleEditInCanvas = useCallback(
    async (id: string) => {
      try {
        const wasCommitted = !!objectsRef.current.find((o) => o.id === id)?.committed_path;
        await invoke<string>("commit_object", { id });
        await invoke("set_expanded", { id, expanded: true });
        await refresh();
        if (!wasCommitted) setTouched((prev) => new Set(prev).add(id));
        setExpanded((prev) => new Set(prev).add(id));
      } catch (e) {
        console.error("edit-in-canvas failed", e);
        setConfirm({
          title: "キャンバス編集を開始できませんでした",
          message: String(e),
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
      }
    },
    [refresh]
  );

  // v1.3.1: persist the text editor panel width per object.
  // v1.5.16: 画像/PDF/動画/Doc プレビューも共用（height は PDF/Doc のコーナーリサイズのみ）。
  const handlePanelResize = useCallback(async (id: string, width: number, height?: number) => {
    try {
      await invoke("set_panel_size", { id, width, height: height ?? null });
      setObjects((prev) =>
        prev.map((o) =>
          o.id === id
            ? { ...o, panel_w: width, panel_h: height !== undefined ? height : o.panel_h }
            : o
        )
      );
    } catch (e) {
      console.error("set_panel_size failed", e);
    }
  }, []);

  const handleCollapse = useCallback((id: string) => {
    invoke("set_expanded", { id, expanded: false }).catch(() => {});
    setExpanded((prev) => {
      const next = new Set(prev);
      next.delete(id);
      return next;
    });
  }, []);

  // Expand a read-only view (image preview / audio player) — no commit needed.
  // 閲覧のみではシグナルは変えない (仮身=琥珀のまま / 実身=グレーのまま).
  const handleExpandView = useCallback((id: string) => {
    invoke("set_expanded", { id, expanded: true }).catch(() => {});
    setExpanded((prev) => new Set(prev).add(id));
  }, []);

  // 仮身に戻す（仮身化）= 実身→仮身。編集内容は失われるため確認ダイアログを出す。
  const handleUncommit = useCallback(
    (id: string) => {
      setConfirm({
        title: "仮身に戻す",
        message:
          "実身ファイルを削除し、仮身に戻します。この編集内容は失われます。よろしいですか？",
        okLabel: "仮身に戻す",
        onConfirm: async () => {
          setConfirm(null);
          try {
            await invoke("uncommit_object", { id });
            setTouched((prev) => {
              const next = new Set(prev);
              next.delete(id);
              return next;
            });
            setExpanded((prev) => {
              const next = new Set(prev);
              next.delete(id);
              return next;
            });
            await refresh();
          } catch (e) {
            console.error("uncommit_object failed", e);
          }
        },
      });
    },
    [refresh]
  );

  // --- v1.3 メモ２追加④: 選択カードへの一括操作 ------------------------------
  // 複数選択したカードに同じ動作（実身化・仮身化・削除）をまとめて適用する。
  // 対象はファイル系カードのみ — プロジェクト/URL/フォルダリンクはスキップ。

  // まとめて実身化（コミット）: 未コミットのファイル系カードを全部実身に。
  // フィルタ/ランチャー/AI 等の自動化カード（filter_ref を持つ＝中身の実体が無い）は
  // 実身化の対象外（単体でも実身化コマンドは出さない）。件数表示と挙動を揃える。
  const handleBulkCommit = useCallback(async () => {
    const targets = objectsRef.current.filter(
      (o) =>
        selectedRef.current.has(o.id) &&
        !o.committed_path && !o.project_ref && !o.url_ref && !o.folder_ref && !o.filter_ref
    );
    if (!targets.length) return;
    const done: string[] = [];
    const errors: string[] = [];
    for (const o of targets) {
      try {
        await invoke("commit_object", { id: o.id });
        done.push(o.id);
      } catch (e) {
        errors.push(`${o.label}: ${e}`);
      }
    }
    if (done.length) setTouched((prev) => new Set([...prev, ...done]));
    await refresh();
    if (errors.length) {
      setConfirm({
        title: "一部のカードを実身化できませんでした",
        message: `${done.length} 件を実身化しました。\n\n失敗 ${errors.length} 件:\n` + errors.join("\n"),
        okLabel: "OK",
        onConfirm: () => setConfirm(null),
      });
    }
  }, [refresh]);

  // まとめて仮身化（コミット取り消し）: 実ファイルを削除して参照に戻す。
  // 取り込み元＝コミット先のカード（仮身に戻す＝元ファイル削除）は除外。
  const handleBulkUncommit = useCallback(() => {
    const targets = objectsRef.current.filter(
      (o) =>
        selectedRef.current.has(o.id) &&
        !!o.committed_path &&
        !!o.origin_path &&
        o.origin_path !== o.committed_path
    );
    if (!targets.length) return;
    setConfirm({
      title: "選択カードをまとめて仮身に戻す",
      message:
        `${targets.length} 件の実身ファイルを削除し、仮身に戻します。\n` +
        "この編集内容は失われます。よろしいですか？",
      okLabel: "仮身に戻す",
      onConfirm: async () => {
        setConfirm(null);
        const ids: string[] = [];
        for (const o of targets) {
          try {
            await invoke("uncommit_object", { id: o.id });
            ids.push(o.id);
          } catch (e) {
            console.error("uncommit_object failed", o.id, e);
          }
        }
        setTouched((prev) => {
          const next = new Set(prev);
          ids.forEach((id) => next.delete(id));
          return next;
        });
        setExpanded((prev) => {
          const next = new Set(prev);
          ids.forEach((id) => next.delete(id));
          return next;
        });
        await refresh();
      },
    });
  }, [refresh]);

  // まとめて削除: 件数とコミット済み（実ファイルも消える）件数を出して確認。
  // v1.4.30: 選択コメント（付箋）も一緒に削除する（Delキーがコメントに効かない問題の本丸）。
  const handleBulkDelete = useCallback(() => {
    const targets = objectsRef.current.filter((o) => selectedRef.current.has(o.id));
    const noteTargets = notesRef.current.filter((n) => selectedNotesRef.current.has(n.id));
    if (!targets.length && !noteTargets.length) return;
    // v1.5.6 WS-M M1（§8.1 メンバー保全・実機FB）: ロック中図形のメンバーが含まれていたら
    // 一括削除ごと止める（グループ移動の弾きと同じ「全体を守る」方針・部分削除はしない）。
    // v1.5.9: 付箋（コメント）もメンバーになりうるので、カードと共通に判定する。
    const lockedHit =
      targets.find((o) => lockedMemberShape(o.id)) ??
      noteTargets.find((n) => lockedMemberShape(n.id));
    if (lockedHit) {
      const lockShape = lockedMemberShape(lockedHit.id);
      const example = "label" in lockedHit ? lockedHit.label : lockedHit.text;
      setConfirm({
        title: "モジュールがロックされています",
        message: `選択の中に 🔒「${lockShape?.label || "図形"}」のメンバー（例: ${example}）が含まれています。削除するには先にロックを解除してください（削除は行いませんでした）。`,
        okLabel: "OK",
        onConfirm: () => setConfirm(null),
      });
      return;
    }
    const committedCount = targets.filter((o) => o.committed_path).length;
    const parts = [
      ...(targets.length ? [`${targets.length} 件のカード`] : []),
      ...(noteTargets.length ? [`${noteTargets.length} 件のコメント`] : []),
    ];
    setConfirm({
      title: "選択をまとめて削除",
      message:
        `${parts.join("と")}を削除します。` +
        (committedCount
          ? `\nうち ${committedCount} 件は実身化済みで、プロジェクトフォルダ内の実身ファイルも削除されます。\n（CAS内の元データは保持されます）`
          : ""),
      okLabel: "削除",
      onConfirm: async () => {
        setConfirm(null);
        const ids: string[] = [];
        for (const o of targets) {
          try {
            await invoke("delete_object", { id: o.id });
            ids.push(o.id);
          } catch (e) {
            console.error("delete_object failed", o.id, e);
          }
        }
        const noteIds: string[] = [];
        for (const n of noteTargets) {
          try {
            await invoke("delete_note", { id: n.id });
            noteIds.push(n.id);
          } catch (e) {
            console.error("delete_note failed", n.id, e);
          }
        }
        // 復活の呪文: まとめて削除も1回のCtrl+Zで全員（カード＋コメント）戻ってくる。
        const undo: UndoAction[] = [];
        if (ids.length) undo.push({ kind: "restore", ids });
        if (noteIds.length) undo.push({ kind: "restoreNotes", ids: noteIds });
        if (undo.length) pushUndo(undo);
        setSelected(new Set());
        setSelectedNotes(new Set());
        await refresh();
      },
    });
  }, [refresh, pushUndo, lockedMemberShape]);

  // 一時的に閲覧: open a disposable temp copy (real body stays pristine).
  // 閲覧のみなのでシグナルは変えない。
  const handleViewTemp = useCallback(async (id: string) => {
    try {
      await invoke("view_temp", { id });
    } catch (e) {
      console.error("view_temp failed", e);
    }
  }, []);

  // 「元のファイルを呼ぶ」: instantiate another 仮身 from the same 実身.
  const handleDuplicate = useCallback(
    async (id: string) => {
      try {
        const vid = await invoke<string>("duplicate_virtual", { id });
        pushUndo([{ kind: "delete", ids: [vid] }]);
        await refresh();
      } catch (e) {
        console.error("duplicate_virtual failed", e);
      }
    },
    [refresh, pushUndo]
  );

  // v1.4.12 #4: Ctrl+D = 選択カードの仮身をまとめて作成（既存「仮身を作成」のキー化）。
  // 各カードの隣（duplicate_virtual の +36,+36 オフセット）に仮身を生やす。
  const handleDuplicateSelected = useCallback(async () => {
    const ids = objectsRef.current.filter((o) => selectedRef.current.has(o.id)).map((o) => o.id);
    if (!ids.length) return;
    try {
      const newIds: string[] = [];
      for (const id of ids) {
        const vid = await invoke<string>("duplicate_virtual", { id });
        newIds.push(vid);
        setTouched((prev) => new Set(prev).add(vid));
      }
      pushUndo([{ kind: "delete", ids: newIds }]);
      await refresh();
      setSelected(new Set(newIds));
    } catch (e) {
      console.error("duplicate selected failed", e);
    }
  }, [refresh, pushUndo, setSelected]);

  // v1.4.12: Alt+クリック／右クリック「つながりを選択」= 関係矢印でつながった連結成分を
  // まるごと選択（向きは問わない＝両方向に辿る）。フロー／モジュールを一掴みするための基盤。
  // relations は カード・コメント両方を端点に取りうるので、結果を selected/selectedNotes に振り分ける。
  // v1.5.11 UIの気づき３-2②: additive=true（Ctrl+Alt+クリック）は既存の選択を解除せず追加する。
  const handleSelectConnected = useCallback(
    (startId: string, additive = false) => {
      const adj = new Map<string, string[]>();
      for (const r of relationsRef.current) {
        (adj.get(r.from_id) ?? adj.set(r.from_id, []).get(r.from_id)!).push(r.to_id);
        (adj.get(r.to_id) ?? adj.set(r.to_id, []).get(r.to_id)!).push(r.from_id);
      }
      const seen = new Set<string>();
      const stack = [startId];
      while (stack.length) {
        const id = stack.pop()!;
        if (seen.has(id)) continue;
        seen.add(id);
        for (const nb of adj.get(id) ?? []) if (!seen.has(nb)) stack.push(nb);
      }
      const cardIds = objectsRef.current.filter((o) => seen.has(o.id)).map((o) => o.id);
      const noteIds = notesRef.current.filter((n) => seen.has(n.id)).map((n) => n.id);
      setSelected((prev) => (additive ? new Set([...prev, ...cardIds]) : new Set(cardIds)));
      setSelectedNotes((prev) => (additive ? new Set([...prev, ...noteIds]) : new Set(noteIds)));
    },
    [setSelected, setSelectedNotes]
  );

  // v1.4.14 プロジェクト間ワークフロー共有: 選択した部分グラフ（カード＋コメント＋内部の
  // 関係矢印）を別プロジェクトへ送る。mode="copy"=仮身化してコピー（元に依存・元は残る）/
  // "move"=実身化して移動（送り先に独立実身・元はソフトデリート＝Ctrl+Z 復活可）。
  // v1.4.17: 選択中の図形（コンテナとして使う枠）も一緒に運ぶ。
  const handleSendSelectionToProject = useCallback(
    async (mode: "copy" | "move", targetPath: string, targetName: string) => {
      const cardIds = [...selectedRef.current];
      const noteIds = [...selectedNotesRef.current];
      const shapeIds = [...selectedShapesRef.current];
      if (!cardIds.length && !noteIds.length && !shapeIds.length) return;
      const run = async () => {
        try {
          const res = await invoke<{
            moved: number;
            new_card_ids: string[];
            new_note_ids: string[];
            new_shape_ids: string[];
          }>("send_selection_to_project", {
            cardIds,
            noteIds,
            shapeIds,
            targetPath,
            moveOut: mode === "move",
          });
          // v1.4.22 ②: 送り先で新規採番されたIDを控える。送り先を開いたとき（下の
          // 「送り先を開く」でも、後で手動で開いても）選択状態で迎える。
          pendingSelectRef.current = {
            path: targetPath,
            cards: res.new_card_ids,
            notes: res.new_note_ids,
            shapes: res.new_shape_ids,
          };
          const openTarget = () => {
            setConfirm(null);
            void (async () => {
              try {
                const info = await invoke<ProjectInfo>("open_project", { path: targetPath });
                setProject(info);
              } catch (err) {
                console.error("open_project failed", err);
              }
            })();
          };
          if (mode === "move") {
            await refresh();
            setConfirm({
              title: "移動しました",
              message: `${res.moved}件を「${targetName}」へ移動しました。実体が送り先へ移り、元プロジェクトからは削除されました（Ctrl+Z では復活できません）。「送り先を開く」で、移動した物が選択された状態で開きます。`,
              okLabel: "送り先を開く",
              cancelLabel: "閉じる",
              onConfirm: openTarget,
            });
          } else {
            setConfirm({
              title: "コピーしました",
              message: `${res.moved}件を「${targetName}」へ仮身としてコピーしました。「送り先を開く」で、コピーした物が選択された状態で開きます。`,
              okLabel: "送り先を開く",
              cancelLabel: "閉じる",
              onConfirm: openTarget,
            });
          }
        } catch (e) {
          console.error("send_selection_to_project failed", e);
          setConfirm({
            title: "送れませんでした",
            message: String(e),
            okLabel: "OK",
            onConfirm: () => setConfirm(null),
          });
        }
      };
      if (mode === "move") {
        const total = cardIds.length + noteIds.length + shapeIds.length;
        setConfirm({
          title: "別プロジェクトへ移動",
          message: `${total}件を「${targetName}」へ移動します。実体が送り先へ移るため、元プロジェクトからは削除され、Ctrl+Z では復活できません。よろしいですか？（元にも残したい場合は「仮身化してコピー」を使ってください）`,
          okLabel: "移動する",
          onConfirm: () => {
            setConfirm(null);
            run();
          },
        });
      } else {
        run();
      }
    },
    [refresh]
  );

  // v1.4.15 データの掃除A（履歴・キャッシュ）: UNDO/REDO履歴＋サムネイル・一時プレビュー
  // キャッシュを掃除。ゴミ箱（削除の復活）・孤立CASは対象外（B=正規化GCで別途）。
  const handleCleanCaches = useCallback(() => {
    setConfirm({
      title: "履歴・キャッシュを掃除",
      message:
        "UNDO/REDO履歴と、サムネイル・一時プレビューのキャッシュを掃除します。\n\n" +
        "・UNDO履歴は消え、Ctrl+Z / Ctrl+Y で戻せなくなります（削除したものは「ゴミ箱を開く」からいつでも復活できます）。\n" +
        "・キャッシュは必要時に自動で再生成されます。",
      okLabel: "掃除する",
      onConfirm: async () => {
        setConfirm(null);
        try {
          const r = await invoke<{
            thumbs_count: number;
            thumbs_bytes: number;
            temp_count: number;
            temp_bytes: number;
          }>("clean_caches");
          const cleared = await clearHistory();
          const mb = (b: number) => (b / 1048576).toFixed(1);
          const freed = r.thumbs_bytes + r.temp_bytes;
          setConfirm({
            title: "掃除しました",
            message:
              `サムネイル ${r.thumbs_count}件・一時プレビュー ${r.temp_count}件（合計 ${mb(freed)}MB）を削除し、` +
              `UNDO/REDO履歴 ${cleared}件をクリアしました。`,
            okLabel: "OK",
            onConfirm: () => setConfirm(null),
          });
        } catch (e) {
          console.error("clean_caches failed", e);
          setConfirm({
            title: "掃除できませんでした",
            message: String(e),
            okLabel: "OK",
            onConfirm: () => setConfirm(null),
          });
        }
      },
    });
  }, [clearHistory]);

  // v1.4.15 データの掃除B（正規化）: どのカードからも参照されない孤立CASを削除。
  // まず dry-run（gc_cas_scan）で件数・サイズを出し、確認してから gc_cas_clean。
  const handleGcCas = useCallback(async () => {
    const mb = (b: number) => (b / 1048576).toFixed(1);
    try {
      const s = await invoke<{ orphan_count: number; orphan_bytes: number; total_count: number }>(
        "gc_cas_scan"
      );
      if (!s.orphan_count) {
        setConfirm({
          title: "孤立データはありません",
          message: `CAS 内 ${s.total_count}件はすべて生きたカードから参照されています。削除対象はありませんでした。`,
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
        return;
      }
      setConfirm({
        title: "孤立データを削除（正規化）",
        message:
          `どのカードからも参照されない実身データ（CAS）${s.orphan_count}件・${mb(s.orphan_bytes)}MB を削除します。\n\n` +
          "・削除すると元に戻せません（生きたカード・ゴミ箱＝削除の復活・UNDO対象は残ります）。\n" +
          "・古い（自己完結化前の）フォークが元プロジェクトのデータを参照している場合は、先にそのフォークを作り直してください。",
        okLabel: "削除する",
        onConfirm: async () => {
          setConfirm(null);
          try {
            const r = await invoke<{ orphan_count: number; orphan_bytes: number }>("gc_cas_clean");
            setConfirm({
              title: "掃除しました",
              message: `孤立データ ${r.orphan_count}件（${mb(r.orphan_bytes)}MB）を削除しました。`,
              okLabel: "OK",
              onConfirm: () => setConfirm(null),
            });
          } catch (e) {
            console.error("gc_cas_clean failed", e);
            setConfirm({
              title: "削除できませんでした",
              message: String(e),
              okLabel: "OK",
              onConfirm: () => setConfirm(null),
            });
          }
        },
      });
    } catch (e) {
      console.error("gc_cas_scan failed", e);
      setConfirm({
        title: "確認できませんでした",
        message: String(e),
        okLabel: "OK",
        onConfirm: () => setConfirm(null),
      });
    }
  }, []);

  // 送り先を最近一覧に無いフォルダから選ぶ（フォールバック）。
  const handleSendSelectionPickFolder = useCallback(
    async (mode: "copy" | "move") => {
      try {
        const picked = await open({ directory: true, title: "送り先プロジェクトのフォルダを選択" });
        if (typeof picked === "string") {
          const name = picked.split(/[\\/]/).filter(Boolean).pop() || picked;
          await handleSendSelectionToProject(mode, picked, name);
        }
      } catch (e) {
        console.error("pick target project failed", e);
      }
    },
    [handleSendSelectionToProject]
  );

  // v1.4.12 #3: Ctrl+左ドラッグでこのカードの仮身（化身）をドロップ位置に作る。
  // duplicate_virtual は filter_ref/kind も継ぐのでフィルタ系カードも複製できる。
  const handleCtrlClone = useCallback(
    async (id: string, clientX: number, clientY: number) => {
      const cp = screenToCanvas(transformRef.current, clientX, clientY);
      try {
        const vid = await invoke<string>("duplicate_virtual", { id });
        await invoke("move_object", { id: vid, x: cp.x, y: cp.y });
        setTouched((prev) => new Set(prev).add(vid));
        pushUndo([{ kind: "delete", ids: [vid] }]);
        await refresh();
      } catch (e) {
        console.error("ctrl-clone failed", e);
      }
    },
    [refresh, pushUndo]
  );

  // v1.3.6 変更②「実身にする」: 仮身が指す内容を独立した内部実身に確定する。
  // 元ファイルから独立 → 緑（このセッションで導入）→ 次回オープンでグレー。
  const handleMakeReal = useCallback(
    async (id: string) => {
      try {
        await invoke<string>("make_real", { id });
        setTouched((prev) => new Set(prev).add(id));
        await refresh();
      } catch (e) {
        console.error("make_real failed", e);
        setConfirm({
          title: "実身にできませんでした",
          message: String(e),
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
      }
    },
    [refresh]
  );

  // --- Z-order -------------------------------------------------------------

  const handleBringToFront = useCallback(async (id: string) => {
    const z = Math.max(0, ...objectsRef.current.map((o) => o.z_order)) + 1;
    try {
      await invoke("set_z_order", { id, z });
      setObjects((prev) => prev.map((o) => (o.id === id ? { ...o, z_order: z } : o)));
    } catch (e) {
      console.error("set_z_order failed", e);
    }
  }, []);

  const handleSendToBack = useCallback(async (id: string) => {
    const z = Math.min(0, ...objectsRef.current.map((o) => o.z_order)) - 1;
    try {
      await invoke("set_z_order", { id, z });
      setObjects((prev) => prev.map((o) => (o.id === id ? { ...o, z_order: z } : o)));
    } catch (e) {
      console.error("set_z_order failed", e);
    }
  }, []);

  // --- Group move (selection state itself lives in useSelection) -------------

  // Drag one selected card/note → move the whole selection together
  // (cards AND notes — v1.3.1 unified).
  // v1.4.17: ドラッグ中の矢印追従。カード/付箋と同じ命令的DOM更新で、動いているノードに
  // 接続された矢印の端点を再計算して <line data-rel-id> に直接書く。状態ベースで再描画すると
  // ドラッグ中の transform（DOM直接更新）と React の再レンダーが競合するため、この方式にする。
  // 確定描画は従来どおりドラッグ終了後の move 永続化＋再レンダーが担う。
  const redrawArrowsFor = useCallback(
    (moved: Map<string, { x: number; y: number }>) => {
      if (!moved.size) return;
      let cardById: Map<string, (typeof objectsRef.current)[number]> | null = null;
      let noteById: Map<string, (typeof notesRef.current)[number]> | null = null;
      const boxOf = (nid: string): NodeBox | null => {
        const m = moved.get(nid);
        cardById ??= new Map(objectsRef.current.map((o) => [o.id, o]));
        const o = cardById.get(nid);
        if (o) return { id: nid, x: m?.x ?? o.pos_x, y: m?.y ?? o.pos_y, w: CARD_W, h: CARD_H };
        noteById ??= new Map(notesRef.current.map((n) => [n.id, n]));
        const n = noteById.get(nid);
        if (n) {
          const s = measuredNoteSize(n);
          return { id: nid, x: m?.x ?? n.pos_x, y: m?.y ?? n.pos_y, w: s.w, h: s.h };
        }
        return null;
      };
      for (const r of relationsRef.current) {
        if (!moved.has(r.from_id) && !moved.has(r.to_id)) continue;
        const from = boxOf(r.from_id);
        const to = boxOf(r.to_id);
        if (!from || !to) continue;
        const p1 = nodeEdgePoint(from, nodeCenter(to));
        const p2 = nodeEdgePoint(to, nodeCenter(from));
        // （v1.5.6: ロック弾きの命令的リセットが redrawArrowsForRef 経由でここを呼ぶ）
        document
          .querySelectorAll<SVGLineElement>(`line[data-rel-id="${r.id}"]`)
          .forEach((ln) => {
            ln.setAttribute("x1", String(p1.x));
            ln.setAttribute("y1", String(p1.y));
            ln.setAttribute("x2", String(p2.x));
            ln.setAttribute("y2", String(p2.y));
          });
      }
    },
    [measuredNoteSize]
  );

  // 単体ドラッグ（ObjectCard/NoteCard 内部のDOM移動）用の矢印追従。
  const handleDragLive = useCallback(
    (id: string, x: number, y: number) => redrawArrowsFor(new Map([[id, { x, y }]])),
    [redrawArrowsFor]
  );
  // v1.5.6: 前方のハンドラ（handleMoveEnd のロック弾き）から呼ぶための差し込み（TDZ 回避）。
  redrawArrowsForRef.current = redrawArrowsFor;

  const handleGroupDragStart = useCallback((id: string, clientX: number, clientY: number) => {
    if (!selectedRef.current.size && !selectedNotesRef.current.size) return;
    const scale = transformRef.current.scale || 1;
    const cardStarts = new Map(
      objectsRef.current
        .filter((o) => selectedRef.current.has(o.id))
        .map((o) => [o.id, { x: o.pos_x, y: o.pos_y }])
    );
    const noteStarts = new Map(
      notesRef.current
        .filter((n) => selectedNotesRef.current.has(n.id))
        .map((n) => [n.id, { x: n.pos_x, y: n.pos_y }])
    );
    const elOf = (sel: string, oid: string) =>
      document.querySelector<HTMLElement>(`[${sel}="${CSS.escape(oid)}"]`);
    let dx = 0;
    let dy = 0;
    const onMove = (me: globalThis.MouseEvent) => {
      document.body.style.cursor = "grabbing"; // 手のひらはドラッグ開始後（G5）
      dx = (me.clientX - clientX) / scale;
      dy = (me.clientY - clientY) / scale;
      cardStarts.forEach((s, oid) => {
        const el = elOf("data-object-id", oid);
        if (el) el.style.transform = `translate(${s.x + dx}px, ${s.y + dy}px)`;
      });
      noteStarts.forEach((s, nid) => {
        const el = elOf("data-note-id", nid);
        if (el) el.style.transform = `translate(${s.x + dx}px, ${s.y + dy}px)`;
      });
      // 矢印も同時に追従（v1.4.17）。
      const moved = new Map<string, { x: number; y: number }>();
      cardStarts.forEach((s, oid) => moved.set(oid, { x: s.x + dx, y: s.y + dy }));
      noteStarts.forEach((s, nid) => moved.set(nid, { x: s.x + dx, y: s.y + dy }));
      redrawArrowsFor(moved);
    };
    const onUp = async () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
      document.body.style.cursor = "";
      // クリック（移動なし）= 複数選択の1枚に絞る（標準のウィンドウシステム流）。
      if (dx === 0 && dy === 0) {
        if (objectsRef.current.some((o) => o.id === id)) {
          setSelected(new Set([id]));
          setSelectedNotes(new Set());
        } else {
          setSelectedNotes(new Set([id]));
          setSelected(new Set());
        }
        return;
      }
      // v1.5.6 WS-M M1（§8.1）: グループ移動でもロック中図形のメンバーは外へ出せない。
      // 1枚でも違反があれば移動全体を戻す（モジュールの整合を優先・部分適用はしない）。
      // v1.5.9: 付箋（コメント）もメンバーになりうるので、カードと共通に判定する。
      const violCard = [...cardStarts.entries()].find(([oid, s]) =>
        lockViolation(oid, s.x + dx, s.y + dy)
      );
      const violNote = violCard
        ? undefined
        : [...noteStarts.entries()].find(([nid, s]) => noteLockViolation(nid, s.x + dx, s.y + dy));
      if (violCard || violNote) {
        const vShape = violCard
          ? lockViolation(violCard[0], violCard[1].x + dx, violCard[1].y + dy)
          : noteLockViolation(violNote![0], violNote![1].x + dx, violNote![1].y + dy);
        // React の VDOM 差分は座標不変だと DOM を書き戻さないため、**命令的に**元へ戻す
        //（ドラッグ中の transform 直書きと同じレーンで返す・矢印も再描画）。
        cardStarts.forEach((s, oid) => {
          const el = elOf("data-object-id", oid);
          if (el) el.style.transform = `translate(${s.x}px, ${s.y}px)`;
        });
        noteStarts.forEach((s, nid) => {
          const el = elOf("data-note-id", nid);
          if (el) el.style.transform = `translate(${s.x}px, ${s.y}px)`;
        });
        const back = new Map<string, { x: number; y: number }>();
        cardStarts.forEach((s, oid) => back.set(oid, { x: s.x, y: s.y }));
        noteStarts.forEach((s, nid) => back.set(nid, { x: s.x, y: s.y }));
        redrawArrowsFor(back);
        setConfirm({
          title: "モジュールがロックされています",
          message: `選択の中に 🔒「${vShape?.label || "図形"}」のメンバーが含まれています。外へ出すにはロックを解除してください（移動は取り消しました）。`,
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
        return;
      }
      const moves = [...cardStarts.entries()].map(([oid, s]) => ({
        id: oid,
        x: s.x + dx,
        y: s.y + dy,
      }));
      const noteMoves = [...noteStarts.entries()].map(([nid, s]) => ({
        id: nid,
        x: s.x + dx,
        y: s.y + dy,
      }));
      const oldCards = [...cardStarts.entries()].map(([oid, s]) => ({ id: oid, x: s.x, y: s.y }));
      const oldNotes = [...noteStarts.entries()].map(([nid, s]) => ({ id: nid, x: s.x, y: s.y }));
      pushUndo([
        { kind: "objects", moves: oldCards },
        { kind: "notes", moves: oldNotes },
      ]);
      try {
        await applyObjectMoves(moves);
        await applyNoteMoves(noteMoves);
        // v1.5.6: グループ移動でロック図形へ入ったカードも所属に追加（単体ドラッグと同じ）。
        let enteredShape: ShapeInfo | null = null;
        let enteredCount = 0;
        for (const [oid, s] of cardStarts) {
          const entry = lockEntry(oid, s.x, s.y, s.x + dx, s.y + dy);
          if (entry) {
            void addShapeMember(entry.id, oid);
            enteredShape = entry;
            enteredCount++;
          }
        }
        if (enteredShape) {
          setConfirm({
            title: "ロック中のモジュールに追加されました",
            message: `${enteredCount} 枚のカードが 🔒「${enteredShape.label || "図形"}」のメンバーになりました（ロック解除するまで外へ出せません・削除もできません）。`,
            okLabel: "OK",
            onConfirm: () => setConfirm(null),
          });
        }
      } catch (e) {
        console.error("group move failed", e);
      }
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  }, [pushUndo, applyObjectMoves, applyNoteMoves, redrawArrowsFor, lockViolation, noteLockViolation, lockEntry, addShapeMember]);

  // --- Clipboard paste (v1.3-④) ---------------------------------------------

  // Approximate footprint of each card, including its expanded panel.
  //（整列・空き領域探索の共通ヘルパ。v1.5.12 でペースト位置決めにも使うためここへ前倒し定義。）
  const layoutSize = useCallback(
    (o: VirtualBodyInfo): { w: number; h: number } => {
      if (!expanded.has(o.id)) return { w: CARD_W, h: CARD_H };
      const isPdf = o.mime_type === "application/pdf" || (o.ext ?? "").toLowerCase() === "pdf";
      // v1.5.16: プレビューがリサイズ可能になったので panel_w/panel_h を反映
      // （画像・動画の高さはアスペクト追従＝幅からの概算。従来の既定比率を維持）。
      if (o.mime_type.startsWith("image/")) {
        const w = o.panel_w ?? 420;
        return { w, h: CARD_H + 8 + Math.round(w * (480 / 420)) };
      }
      if (o.mime_type.startsWith("audio/")) return { w: 300, h: CARD_H + 8 + 90 };
      if (o.mime_type.startsWith("video/")) {
        const w = o.panel_w ?? 420;
        return { w, h: CARD_H + 8 + Math.round(w * (460 / 420)) };
      }
      if (isPdf) return { w: o.panel_w ?? 480, h: CARD_H + 8 + (o.panel_h ?? 600) + 40 };
      if (isTextMime(o.mime_type, o.ext)) return { w: o.panel_w ?? 360, h: CARD_H + 8 + 270 };
      return { w: CARD_W, h: CARD_H };
    },
    [expanded]
  );

  // 画像はバックエンドで新オブジェクト化。テキストはURLならURLカード、
  // それ以外はコメント（付箋）として作成。
  const handlePaste = useCallback(
    async (canvasX: number, canvasY: number, linkFrom?: string) => {
      try {
        const res = await invoke<{ kind: string; text: string | null; id: string | null; paths: string[] | null }>(
          "paste_clipboard",
          { x: canvasX, y: canvasY }
        );
        if (res.kind === "image") {
          if (res.id) pushUndo([{ kind: "delete", ids: [res.id] }]);
          await refresh();
          if (linkFrom && res.id) await linkNewNode(linkFrom, res.id);
        } else if (res.kind === "text" && res.text) {
          const t = res.text.trim();
          let vid: string;
          if (/^https?:\/\/\S+$/i.test(t)) {
            vid = await invoke<string>("create_url", { title: "", url: t, x: canvasX, y: canvasY });
          } else {
            // v1.3.1: テキストも画像と同格 — CASに.txt実身を作り仮身カード化。
            // コミットでファイル化、矢印リンク・図形グループの対象になる。
            vid = await invoke<string>("create_text_object", { text: res.text, x: canvasX, y: canvasY });
          }
          pushUndo([{ kind: "delete", ids: [vid] }]);
          await refresh();
          if (linkFrom) await linkNewNode(linkFrom, vid);
        } else if (res.kind === "files" && res.paths?.length) {
          // v1.5.12: ファイルのペースト = ドラッグインと同一動作（v1.3.4 の案内止まりを改定）。
          // handleDropFiles がカスケード配置・ドロップポイント投入・作成UNDO・導入ハイライト
          // まで面倒を見る＝文字通り「D&D と同じ」。切り取り（移動効果）は無視し常にコピー扱い、
          // 元ファイルには触れない（削除は貼り付け先の責務なので、無視すれば誰も消さない）。
          await handleDropFiles(res.paths, canvasX, canvasY);
        }
      } catch (e) {
        console.error("paste_clipboard failed", e);
      }
    },
    [refresh, pushUndo, linkNewNode, handleDropFiles]
  );

  // v1.5.12 実機FB: Ctrl+V の貼り付け位置。従来のビューポート中央は貼ったカードが迷子になる。
  // ①ポインタがキャンバス上 → その位置（右クリック貼り付けと同じ感覚）
  // ②ポインタが無い/UI上 → 可視領域を上から走査して最初の空きセル（既存物と重ねない）
  const pointerRef = useRef<{ x: number; y: number } | null>(null);
  useEffect(() => {
    const move = (e: MouseEvent) => { pointerRef.current = { x: e.clientX, y: e.clientY }; };
    const leave = () => { pointerRef.current = null; }; // ウィンドウ外へ出たら「ポインタ無し」
    window.addEventListener("mousemove", move);
    document.documentElement.addEventListener("mouseleave", leave);
    return () => {
      window.removeEventListener("mousemove", move);
      document.documentElement.removeEventListener("mouseleave", leave);
    };
  }, []);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey) || e.key.toLowerCase() !== "v") return;
      const ae = document.activeElement as HTMLElement | null;
      if (ae && (ae.tagName === "INPUT" || ae.tagName === "TEXTAREA" || ae.isContentEditable)) return;
      const p = pointerRef.current;
      // elementFromPoint で「今キャンバス（の子）を指しているか」を判定 —
      // メニューやパネル・ダイアログの上なら空き領域配置へフォールバック。
      const overCanvas = p && document.elementFromPoint(p.x, p.y)?.closest("[data-canvas-root]");
      if (p && overCanvas) {
        const c = screenToCanvas(transformRef.current, p.x, p.y);
        handlePaste(c.x - CARD_W / 2, c.y - CARD_H / 2);
        return;
      }
      // 可視領域の左上から行優先で走査し、既存カード/付箋（実寸）と重ならない最初のセルへ。
      const t = transformRef.current;
      const tl = screenToCanvas(t, 0, 0);
      const br = screenToCanvas(t, window.innerWidth, window.innerHeight);
      const GAP = 28;
      const pw = CARD_W + GAP;
      const ph = CARD_H + GAP;
      const rects = [
        ...objectsRef.current.map((o) => {
          const s = layoutSize(o);
          return { x: o.pos_x, y: o.pos_y, w: s.w, h: s.h };
        }),
        ...notesRef.current.map((n) => {
          const s = measuredNoteSize(n);
          return { x: n.pos_x, y: n.pos_y, w: s.w, h: s.h };
        }),
      ];
      const isFree = (x: number, y: number) =>
        !rects.some(
          (r) => x < r.x + r.w + GAP && x + CARD_W + GAP > r.x && y < r.y + r.h + GAP && y + CARD_H + GAP > r.y
        );
      const cols = Math.max(1, Math.floor((br.x - tl.x - GAP) / pw));
      const rows = Math.max(1, Math.floor((br.y - tl.y - GAP) / ph));
      for (let r = 0; r < rows; r++) {
        for (let c = 0; c < cols; c++) {
          const x = tl.x + GAP + c * pw;
          const y = tl.y + GAP + r * ph;
          if (isFree(x, y)) {
            handlePaste(x, y);
            return;
          }
        }
      }
      // 可視領域に空きが無い → 従来どおり中央（最後の砦）。
      const cc = screenToCanvas(t, window.innerWidth / 2, window.innerHeight / 2);
      handlePaste(cc.x - CARD_W / 2, cc.y - CARD_H / 2);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [handlePaste, layoutSize, measuredNoteSize]);

  // --- v1.3.4 クリップボード持ち出し（出す側） -------------------------------
  // 選択カードを CF_HDROP（ファイル）として OS クリップボードへ。未コミットの
  // 仮身はバックエンドでコミットしてから載せる（エクスポート＝コミット原則）。
  const handleCopyFiles = useCallback(
    async (ids: string[]) => {
      if (!ids.length) return;
      // コピーで新規に実身化されるのは「未コミットのファイル系カード」だけ。
      // 他のコミット経路と同じく今セッション中は緑（touched）にする（次回起動でグレー）。
      const idSet = new Set(ids);
      const freshlyCommitted = objectsRef.current
        .filter(
          (o) =>
            idSet.has(o.id) &&
            !o.committed_path &&
            !o.url_ref &&
            !o.project_ref &&
            !o.folder_ref &&
            o.kind !== "filter" &&
            o.kind !== "drop"
        )
        .map((o) => o.id);
      try {
        const rep = await invoke<{
          copied: string[];
          count: number;
          warnings: { label: string; kind: string; detail: string }[];
          errors: string[];
        }>("copy_objects_to_clipboard", { ids });
        // 未コミット仮身をコミットした場合があるのでシグナルを更新
        if (freshlyCommitted.length)
          setTouched((prev) => new Set([...prev, ...freshlyCommitted]));
        await refresh();
        if (rep.count === 0 && !rep.warnings.length && !rep.errors.length) return;
        const lines: string[] = [];
        if (rep.warnings.length) {
          const label = (w: { kind: string }) =>
            w.kind === "url"
              ? "URL"
              : w.kind === "filter"
              ? "フィルタカード"
              : w.kind === "drop"
              ? "ドロップポイント"
              : w.kind;
          lines.push(
            "コピーできない項目（ファイルを持たない）:\n" +
              rep.warnings.map((w) => `・${w.label}（${label(w)}）`).join("\n")
          );
        }
        if (rep.errors.length) lines.push("エラー:\n" + rep.errors.map((e) => `・${e}`).join("\n"));
        // 全部成功（スキップ・エラーなし）なら通常の Ctrl+C と同じく無音
        if (!lines.length) return;
        setConfirm({
          title: rep.count ? `${rep.count} 件をクリップボードにコピーしました` : "クリップボードにコピーできませんでした",
          message: lines.join("\n\n"),
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
      } catch (e) {
        console.error("copy_objects_to_clipboard failed", e);
      }
    },
    [refresh]
  );

  // --- v1.8.1 ドラッグアウト（出す側・ドラッグ経路） -------------------------
  // 持ち出しの規則は Ctrl+C と同一（バックエンドの resolve_export_paths を共用）。
  // 違うのは運び方だけ＝クリップボードに置くか、OS のドラッグに載せるか。
  //
  // 掴んだカードが選択に含まれていればその選択ぜんぶ、含まれていなければその1枚を運ぶ
  //（複数選択の1枚を素ドラッグ＝群移動、と同じ流儀）。
  //
  // ドラッグ中は OS のモーダルループが前面を握るので、警告ダイアログはここで出さず
  // 完了イベント（texos://drag-out）まで持ち越す。掴んだ瞬間にダイアログを出すと
  // フォーカスを奪ってドラッグ自体が壊れる。
  const pendingDragOut = useRef<{
    count: number;
    warnings: { label: string; kind: string; detail: string }[];
    errors: string[];
  } | null>(null);

  const handleDragOut = useCallback(
    async (id: string) => {
      const ids = selectedRef.current.has(id) ? [...selectedRef.current] : [id];
      // 持ち出しで新規に実身化されるのは「未コミットのファイル系カード」だけ
      //（handleCopyFiles と同じ判定＝今セッション中は緑にする）。
      const idSet = new Set(ids);
      const freshlyCommitted = objectsRef.current
        .filter(
          (o) =>
            idSet.has(o.id) &&
            !o.committed_path &&
            !o.url_ref &&
            !o.project_ref &&
            !o.folder_ref &&
            o.kind !== "filter" &&
            o.kind !== "drop"
        )
        .map((o) => o.id);
      try {
        const rep = await invoke<{
          copied: string[];
          count: number;
          warnings: { label: string; kind: string; detail: string }[];
          errors: string[];
        }>("start_object_drag", { ids });
        if (freshlyCommitted.length)
          setTouched((prev) => new Set([...prev, ...freshlyCommitted]));
        await refresh();
        if (rep.count === 0) {
          // ドラッグは始まっていない＝完了イベントは来ないので、ここで理由を出す。
          pendingDragOut.current = null;
          if (rep.warnings.length || rep.errors.length)
            setConfirm({
              title: "ドラッグで持ち出せませんでした",
              message: rep.warnings.length
                ? "ファイルを持たないカードです:\n" +
                  rep.warnings.map((w) => `・${w.label}`).join("\n")
                : rep.errors.map((e) => `・${e}`).join("\n"),
              okLabel: "OK",
              onConfirm: () => setConfirm(null),
            });
          return;
        }
        // 一部だけ持ち出せた場合の内訳はドロップ完了後に伝える。
        pendingDragOut.current =
          rep.warnings.length || rep.errors.length ? rep : null;
      } catch (e) {
        console.error("start_object_drag failed", e);
      }
    },
    [refresh]
  );

  // ドラッグアウト完了（ドロップ or 中止）。持ち越した内訳をここで出す。
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    listen<{ dropped: boolean; error?: string | null }>("texos://drag-out", (event) => {
      const pending = pendingDragOut.current;
      pendingDragOut.current = null;
      if (event.payload.error) {
        console.error("drag-out failed", event.payload.error);
        return;
      }
      // 中止（どこにも落とさなかった）なら黙って終わる。
      if (!event.payload.dropped || !pending) return;
      const lines: string[] = [];
      if (pending.warnings.length)
        lines.push(
          "持ち出せなかったカード（ファイルを持たない）:\n" +
            pending.warnings.map((w) => `・${w.label}`).join("\n")
        );
      if (pending.errors.length)
        lines.push("エラー:\n" + pending.errors.map((e) => `・${e}`).join("\n"));
      if (!lines.length) return;
      setConfirm({
        title: `${pending.count} 件を持ち出しました`,
        message: lines.join("\n\n"),
        okLabel: "OK",
        onConfirm: () => setConfirm(null),
      });
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  // v1.3.8 (G6): 画像カードをビットマップ(CF_DIB)としてコピー（RMBメニュー専用）。
  // 未保存の仮身は自動でコミットされ得るので、状態シグナル更新のため refresh。
  const handleCopyImage = useCallback(
    async (id: string) => {
      try {
        await invoke("copy_image_to_clipboard", { id });
        await refresh();
      } catch (e) {
        console.error("copy_image_to_clipboard failed", e);
      }
    },
    [refresh]
  );

  // v1.4.20 ナビゲーションキー（テキスト編集中は奪わない）:
  //   F      = 選択にフォーカス（選択が無ければ全体を表示）— Unity/Maya/Godot の Frame Selected 流
  //   Home   = 全体を表示 — Blender の View All 流
  //   Ctrl+0 = ズーム100% — Adobe/Figma/ブラウザ流
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const ae = document.activeElement as HTMLElement | null;
      if (ae && (ae.tagName === "INPUT" || ae.tagName === "TEXTAREA" || ae.isContentEditable)) return;
      if (e.key.toLowerCase() === "f" && !e.ctrlKey && !e.metaKey && !e.altKey && !e.shiftKey) {
        e.preventDefault();
        if (selectedRef.current.size || selectedNotesRef.current.size) {
          canvasApi.current?.fitSelection();
        } else {
          canvasApi.current?.fitAll();
        }
        return;
      }
      if (e.key === "Home" && !e.ctrlKey && !e.metaKey && !e.altKey) {
        e.preventDefault();
        canvasApi.current?.fitAll();
        return;
      }
      if (e.key === "0" && (e.ctrlKey || e.metaKey) && !e.altKey) {
        e.preventDefault();
        canvasApi.current?.zoomTo(100);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // v1.4.19 Ctrl+F → カードを検索（テキスト編集中は奪わない・再押下でトグル）。
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey) || e.key.toLowerCase() !== "f") return;
      const ae = document.activeElement as HTMLElement | null;
      if (ae && (ae.tagName === "INPUT" || ae.tagName === "TEXTAREA" || ae.isContentEditable)) return;
      e.preventDefault();
      setSearchOpen((v) => !v);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // Ctrl+C → 選択カードをファイルとしてコピー（テキスト編集中は奪わない）。
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey) || e.key.toLowerCase() !== "c") return;
      const ae = document.activeElement as HTMLElement | null;
      if (ae && (ae.tagName === "INPUT" || ae.tagName === "TEXTAREA" || ae.isContentEditable)) return;
      // キャンバスは userSelect:none のため、非折りたたみのテキスト選択が残っているのは
      // チャット応答などパネル内で意図的に選択した状態。カードが選択中でも奪わず標準コピーに任せる。
      const sel = window.getSelection();
      if (sel && !sel.isCollapsed && sel.toString()) return;
      const ids = [...selectedRef.current];
      if (!ids.length) return; // 選択がなければブラウザ標準コピーに任せる
      e.preventDefault();
      handleCopyFiles(ids);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [handleCopyFiles]);

  // --- Shapes (v1.3-②③) ------------------------------------------------------

  // Cards/notes whose center sits on the shape = the shape's thought-group.
  const cardsOnShape = useCallback((s: ShapeInfo) => {
    return objectsRef.current.filter((o) => {
      const cx = o.pos_x + CARD_W / 2;
      const cy = o.pos_y + CARD_H / 2;
      return cx >= s.pos_x && cx <= s.pos_x + s.width && cy >= s.pos_y && cy <= s.pos_y + s.height;
    });
  }, []);

  const notesOnShape = useCallback((s: ShapeInfo) => {
    return notesRef.current.filter((n) => {
      const sz = measuredNoteSize(n);
      const cx = n.pos_x + sz.w / 2;
      const cy = n.pos_y + sz.h / 2;
      return cx >= s.pos_x && cx <= s.pos_x + s.width && cy >= s.pos_y && cy <= s.pos_y + s.height;
    });
  }, []);

  // 従う図形を推移的に集める（子の上の孫も動く）。クリック選択とドラッグ追従で共用
  // することで「クリックで選択される集合＝ドラッグで動く集合」を一致させる（v1.4.21 ①）。
  const growShapeGroup = useCallback((base: ShapeInfo[]) => {
    const moving = new Map<string, ShapeInfo>(base.map((sh) => [sh.id, sh]));
    let grown = true;
    while (grown) {
      grown = false;
      for (const sh of shapesRef.current) {
        if (moving.has(sh.id)) continue;
        for (const parent of moving.values()) {
          if (sh.z_order > parent.z_order && shapeCenterOn(sh, parent)) {
            moving.set(sh.id, sh);
            grown = true;
            break;
          }
        }
      }
    }
    return moving;
  }, []);

  const handleShapeAdd = useCallback(
    async (kind: string, canvasX: number, canvasY: number) => {
      const size =
        kind === "arrow" ? { width: 280, height: 110 } : { width: 360, height: 250 };
      let x = canvasX, y = canvasY, w = size.width, h = size.height;
      // v1.4.25 ⑤: スナップ中は対角（左上・右下）を格子に乗せて生む。
      if (shapeSnap) {
        const g = SHAPE_GRID;
        x = Math.round(x / g) * g;
        y = Math.round(y / g) * g;
        w = Math.max(g, Math.round((x + w) / g) * g - x);
        h = Math.max(g, Math.round((y + h) / g) * g - y);
      }
      try {
        const vid = await invoke<string>("create_shape", { kind, x, y, width: w, height: h });
        pushUndo([{ kind: "deleteShapes", ids: [vid] }]);
        await refresh();
      } catch (e) {
        console.error("create_shape failed", e);
      }
    },
    [refresh, pushUndo, shapeSnap]
  );

  // v1.4.25 ⑤: 全図形の対角をグリッドへ強制スナップ（オプションのオン/オフに関わらず
  // 使える明示コマンド）。UNDO一手で全部戻せる。
  const handleSnapAllShapes = useCallback(async () => {
    const g = SHAPE_GRID;
    const olds: { id: string; x: number; y: number; w: number; h: number; rot: number }[] = [];
    const geoms: { id: string; x: number; y: number; w: number; h: number; rot: number }[] = [];
    for (const s of shapesRef.current) {
      const x = Math.round(s.pos_x / g) * g;
      const y = Math.round(s.pos_y / g) * g;
      const w = Math.max(g, Math.round((s.pos_x + s.width) / g) * g - x);
      const h = Math.max(g, Math.round((s.pos_y + s.height) / g) * g - y);
      if (x !== s.pos_x || y !== s.pos_y || w !== s.width || h !== s.height) {
        olds.push({ id: s.id, x: s.pos_x, y: s.pos_y, w: s.width, h: s.height, rot: s.rotation });
        geoms.push({ id: s.id, x, y, w, h, rot: s.rotation });
      }
    }
    if (!geoms.length) return;
    pushUndo([{ kind: "shapes", geoms: olds }]);
    try {
      await applyShapeGeoms(geoms);
    } catch (e) {
      console.error("snap all shapes failed", e);
    }
  }, [pushUndo, applyShapeGeoms]);

  // v1.4.23 ③: 選択部（カード＋コメント）を囲む角丸四角を新規作成し、図形を選択状態に
  // する（カード側の選択は維持）。ステップ①の分離モードと組み合わせて、混在した密集地でも
  // 「選択 → 囲む → 図形ドラッグ」で選択だけを新グループとして引き出せる。
  const handleShapeAroundSelection = useCallback(async () => {
    const selCards = objectsRef.current.filter((o) => selectedRef.current.has(o.id));
    const selNs = notesRef.current.filter((n) => selectedNotesRef.current.has(n.id));
    if (!selCards.length && !selNs.length) return;
    const boxes = [
      ...selCards.map((o) => ({ x: o.pos_x, y: o.pos_y, w: CARD_W, h: CARD_H })),
      ...selNs.map((n) => {
        const sz = measuredNoteSize(n);
        return { x: n.pos_x, y: n.pos_y, w: sz.w, h: sz.h };
      }),
    ];
    const pad = 28;
    let minX = Math.min(...boxes.map((b) => b.x)) - pad;
    let minY = Math.min(...boxes.map((b) => b.y)) - pad;
    let maxX = Math.max(...boxes.map((b) => b.x + b.w)) + pad;
    let maxY = Math.max(...boxes.map((b) => b.y + b.h)) + pad;
    // v1.4.25 ⑤: スナップ中は外側方向へ丸めて格子に乗せる（選択部を必ず包含したまま）。
    if (shapeSnap) {
      const g = SHAPE_GRID;
      minX = Math.floor(minX / g) * g;
      minY = Math.floor(minY / g) * g;
      maxX = Math.ceil(maxX / g) * g;
      maxY = Math.ceil(maxY / g) * g;
    }
    try {
      const vid = await invoke<string>("create_shape", {
        kind: "rect",
        x: Math.round(minX),
        y: Math.round(minY),
        width: Math.round(maxX - minX),
        height: Math.round(maxY - minY),
      });
      pushUndo([{ kind: "deleteShapes", ids: [vid] }]);
      await refresh();
      setSelectedShapes(new Set([vid])); // カード・コメントの選択はそのまま維持
    } catch (e) {
      console.error("create_shape (around selection) failed", e);
    }
  }, [measuredNoteSize, pushUndo, refresh, setSelectedShapes, shapeSnap]);

  const handleShapeGeomEnd = useCallback(
    async (id: string, x: number, y: number, w: number, h: number) => {
      const s = shapesRef.current.find((sh) => sh.id === id);
      if (!s) return;
      // v1.5.6 実機FB: メンバーシップは所属（members）なので、図形を縮めても**外れない**。
      // ただし「メンバーが図形の外に見える」状態は紛らわしいので知らせる（回収手段つき）。
      if (s.locked && (w !== s.width || h !== s.height)) {
        const newRect = { ...s, pos_x: x, pos_y: y, width: w, height: h };
        const outside = objectsRef.current.filter(
          (o) => s.members.includes(o.id) && !cardCenterInShape(o.pos_x, o.pos_y, newRect)
        );
        if (outside.length) {
          setConfirm({
            title: "メンバーが図形の外になります",
            message: `${outside.length} 枚のメンバーが 🔒「${s.label || "図形"}」の見た目の外になります（例: ${outside[0].label}）。所属は維持され、ロックの保護（移動・削除の禁止）も続きます。図形右クリック「メンバーを選択」でいつでも回収できます。`,
            okLabel: "OK",
            onConfirm: () => setConfirm(null),
          });
        }
      }
      if (s.pos_x !== x || s.pos_y !== y || s.width !== w || s.height !== h) {
        const old = { id, x: s.pos_x, y: s.pos_y, w: s.width, h: s.height, rot: s.rotation };
        pushUndo([{ kind: "shapes", geoms: [old] }]);
      }
      try {
        await applyShapeGeoms([{ id, x, y, w, h, rot: s.rotation }]);
      } catch (e) {
        console.error("update_shape_geom failed", e);
      }
    },
    [pushUndo, applyShapeGeoms, cardCenterInShape]
  );

  // クリック（ドラッグなし）= 図形上のカード・コメントをまとめて選択（思考のグループ）。
  // v1.4.21 ①: 推移的な子図形も選択に含める。ドラッグ時の追従集合と一致するので、
  // Ctrl+クリックで不要なカード・子図形を外してから図形ドラッグ＝選択だけ分離、が成立する。
  const handleShapeClickSelect = useCallback(
    (id: string) => {
      const s = shapesRef.current.find((sh) => sh.id === id);
      if (!s) return;
      const group = growShapeGroup([s]);
      const cards = new Set<string>();
      const notes = new Set<string>();
      for (const sh of group.values()) {
        cardsOnShape(sh).forEach((c) => cards.add(c.id));
        notesOnShape(sh).forEach((n) => notes.add(n.id));
      }
      setSelectedShapes(new Set(group.keys()));
      setSelected(cards);
      setSelectedNotes(notes);
    },
    [cardsOnShape, notesOnShape, growShapeGroup]
  );

  // 図形をドラッグ → 上に乗っているカード・コメントも一緒に動かす（グループ移動）。
  // v1.3 メモ④: 複数選択中の図形をドラッグすると選択図形がまとめて動く。
  // v1.3 メモ⑤: 前面に重なっている図形（中心が乗っていて手前）も従って動く。
  const shapeDragRef = useRef<{
    cardStarts: Map<string, { x: number; y: number }>;
    noteStarts: Map<string, { x: number; y: number }>;
    shapeStarts: Map<string, { x: number; y: number; rot: number }>;
  } | null>(null);
  const handleShapeDragCards = useCallback(
    (id: string, dx: number, dy: number, done: boolean) => {
      let drag = shapeDragRef.current;
      if (!drag) {
        const s = shapesRef.current.find((sh) => sh.id === id);
        if (!s) return;
        const sel = selectedShapesRef.current;
        const base = sel.has(id) && sel.size > 1
          ? shapesRef.current.filter((sh) => sel.has(sh.id))
          : [s];
        // 従う図形を推移的に集める（子の上の孫も動く）。
        const moving = growShapeGroup(base);
        // v1.4.21 ①「分離モード」: 図形群の上のカード/コメントが選択中なら、選択された
        // ものだけが追従し非選択は置いていく（選択＝分離の意図）。図形と無関係な選択
        //（上に乗っていない）の場合は従来通り全員追従。Shift+ドラッグ（図形のみ）は
        // ShapeCard 側で onDragCards 自体が呼ばれないので従来通り。
        const selCards = selectedRef.current;
        const selNotes = selectedNotesRef.current;
        let separating = false;
        for (const sh of moving.values()) {
          if (
            cardsOnShape(sh).some((c) => selCards.has(c.id)) ||
            notesOnShape(sh).some((n) => selNotes.has(n.id))
          ) {
            separating = true;
            break;
          }
        }
        if (separating) {
          // 分離モードでは推移的に拾った子図形のうち、明示選択されていないものは外す
          //（Ctrl+クリックで子図形を選択から外す＝置いていく、を可能にする）。
          const baseIds = new Set(base.map((b) => b.id));
          for (const sid of [...moving.keys()]) {
            if (!baseIds.has(sid) && !sel.has(sid)) moving.delete(sid);
          }
        }
        const cardStarts = new Map<string, { x: number; y: number }>();
        const noteStarts = new Map<string, { x: number; y: number }>();
        // v1.5.6 WS-M M1（§8.3 片ロック規則）: 動かない側のロック図形の**メンバー（所属）**は
        // その図形に残る（ドラッグする図形が手放す）。両ロックの仮身分裂（§8.3）は
        // 後回し＝当面は保守的に「残る」側に倒す（申し送り参照）。
        const movingIds = new Set(moving.keys());
        const lockedOutside = shapesRef.current.filter(
          (sh) => sh.locked && !movingIds.has(sh.id)
        );
        const heldByLock = (cardId: string) =>
          lockedOutside.some((L) => L.members.includes(cardId));
        for (const sh of moving.values()) {
          cardsOnShape(sh).forEach((c) => {
            if ((!separating || selCards.has(c.id)) && !heldByLock(c.id)) {
              cardStarts.set(c.id, { x: c.pos_x, y: c.pos_y });
            }
          });
          notesOnShape(sh).forEach((n) => {
            if (!separating || selNotes.has(n.id)) noteStarts.set(n.id, { x: n.pos_x, y: n.pos_y });
          });
        }
        drag = {
          cardStarts,
          noteStarts,
          // 自身はShapeCardがDOMを動かし、onGeomEndで永続化する。
          shapeStarts: new Map(
            [...moving.values()]
              .filter((sh) => sh.id !== id)
              .map((sh) => [sh.id, { x: sh.pos_x, y: sh.pos_y, rot: sh.rotation }])
          ),
        };
        shapeDragRef.current = drag;
      }
      drag.cardStarts.forEach((st, oid) => {
        const el = document.querySelector<HTMLElement>(`[data-object-id="${CSS.escape(oid)}"]`);
        if (el) el.style.transform = `translate(${st.x + dx}px, ${st.y + dy}px)`;
      });
      drag.noteStarts.forEach((st, nid) => {
        const el = document.querySelector<HTMLElement>(`[data-note-id="${CSS.escape(nid)}"]`);
        if (el) el.style.transform = `translate(${st.x + dx}px, ${st.y + dy}px)`;
      });
      drag.shapeStarts.forEach((st, sid) => {
        const el = document.querySelector<HTMLElement>(`[data-shape-id="${CSS.escape(sid)}"]`);
        if (el) el.style.transform = `translate(${st.x + dx}px, ${st.y + dy}px) rotate(${st.rot}deg)`;
      });
      // 図形に乗ったカード/付箋の矢印も同時に追従（v1.4.17）。
      const movedNodes = new Map<string, { x: number; y: number }>();
      drag.cardStarts.forEach((st, oid) => movedNodes.set(oid, { x: st.x + dx, y: st.y + dy }));
      drag.noteStarts.forEach((st, nid) => movedNodes.set(nid, { x: st.x + dx, y: st.y + dy }));
      redrawArrowsFor(movedNodes);
      if (done) {
        const moves = [...drag.cardStarts.entries()].map(([oid, st]) => ({
          id: oid,
          x: st.x + dx,
          y: st.y + dy,
        }));
        const noteMoves = [...drag.noteStarts.entries()].map(([nid, st]) => ({
          id: nid,
          x: st.x + dx,
          y: st.y + dy,
        }));
        const sizeOf = (sid: string) => shapesRef.current.find((sh) => sh.id === sid);
        const shapeGeoms = [...drag.shapeStarts.entries()].flatMap(([sid, st]) => {
          const sh = sizeOf(sid);
          return sh ? [{ id: sid, x: st.x + dx, y: st.y + dy, w: sh.width, h: sh.height, rot: st.rot }] : [];
        });
        const oldCards = [...drag.cardStarts.entries()].map(([oid, st]) => ({ id: oid, x: st.x, y: st.y }));
        const oldNotes = [...drag.noteStarts.entries()].map(([nid, st]) => ({ id: nid, x: st.x, y: st.y }));
        const oldShapes = [...drag.shapeStarts.entries()].flatMap(([sid, st]) => {
          const sh = sizeOf(sid);
          return sh ? [{ id: sid, x: st.x, y: st.y, w: sh.width, h: sh.height, rot: st.rot }] : [];
        });
        if (oldCards.length || oldNotes.length || oldShapes.length) {
          pushUndo([
            { kind: "objects", moves: oldCards },
            { kind: "notes", moves: oldNotes },
            { kind: "shapes", geoms: oldShapes },
          ]);
        }
        shapeDragRef.current = null;
        (async () => {
          try {
            await applyObjectMoves(moves);
            await applyNoteMoves(noteMoves);
            await applyShapeGeoms(shapeGeoms);
          } catch (e) {
            console.error("shape group move failed", e);
          }
        })();
      }
    },
    [cardsOnShape, notesOnShape, growShapeGroup, pushUndo, applyObjectMoves, applyNoteMoves, applyShapeGeoms, redrawArrowsFor, cardCenterInShape]
  );

  const handleShapeDelete = useCallback(
    async (id: string) => {
      try {
        await invoke("delete_shape", { id });
        pushUndo([{ kind: "restoreShapes", ids: [id] }]);
        setSelectedShapes((prev) => {
          if (!prev.has(id)) return prev;
          const next = new Set(prev);
          next.delete(id);
          return next;
        });
        setShapes((prev) => prev.filter((s) => s.id !== id));
      } catch (e) {
        console.error("delete_shape failed", e);
      }
    },
    [pushUndo]
  );

  // v1.5.6 WS-M M1（§8.1）: 図形「メンバーをロック」トグル。
  // ロック ON = その瞬間に乗っているカードをスナップショット（所属＝幾何ではない）。
  // OFF = 所属を消してゆるいグループ（動的包含）へ戻る。
  // v1.5.9: メンバーにコメント（付箋）も含める。付箋も虚身（virtual_bodies）なので
  // members に id を入れればロック保護もライブラリ化の範囲にも自然に乗る（要件1・2）。
  const handleShapeToggleLock = useCallback(
    async (id: string, locked: boolean) => {
      const s = shapesRef.current.find((sh) => sh.id === id);
      if (!s) return;
      const memberIds = locked
        ? [...cardsOnShape(s).map((c) => c.id), ...notesOnShape(s).map((n) => n.id)]
        : [];
      try {
        await invoke("set_shape_locked", { id, locked, memberIds });
        setShapes((prev) =>
          prev.map((sh) => (sh.id === id ? { ...sh, locked, members: memberIds } : sh))
        );
      } catch (e) {
        console.error("set_shape_locked failed", e);
      }
    },
    [cardsOnShape, notesOnShape]
  );

  // v1.5.6 実機FB: メンバーを id で選択（図形の外に出てしまったメンバーの回収手段）。
  // v1.5.9: メンバーにはコメント（付箋）も含まれるので、カードと付箋の両方を選択へ振り分ける。
  const handleSelectShapeMembers = useCallback((id: string) => {
    const s = shapesRef.current.find((sh) => sh.id === id);
    if (!s || !s.members.length) return;
    const aliveCards = new Set(objectsRef.current.map((o) => o.id));
    const aliveNotes = new Set(notesRef.current.map((n) => n.id));
    setSelected(new Set(s.members.filter((m) => aliveCards.has(m))));
    setSelectedNotes(new Set(s.members.filter((m) => aliveNotes.has(m))));
  }, []);

  // v1.5.6 WS-M M1（§9.1）: モジュール化＝境界をまたぐ flow 矢印に IN/OUT を自動挿入。
  const handleModularizeShape = useCallback(
    async (id: string) => {
      const s = shapesRef.current.find((sh) => sh.id === id);
      if (!s) return;
      // ロック済みなら所属スナップショット（正）、未ロックなら動的包含（ゆるいグループ）。
      const memberIds =
        s.locked && s.members.length ? s.members : cardsOnShape(s).map((c) => c.id);
      try {
        const created = await invoke<string[]>("modularize_members", { memberIds });
        // Ctrl+Z（カード削除）では繋ぎ替えた矢印が戻らないため undo には積まない。
        // 逆操作は各 IN/OUT の「溶解」（矢印を直結に戻して消す＝属性も復元される）。
        if (created.length) {
          setTouched((prev) => new Set([...prev, ...created]));
        }
        // v1.5.8 WS-M M2: 口（IN/OUT）もモジュールの一員。ロック中なら所属へ追加して
        // 削除・ドラッグアウトの保護とライブラリ登録の範囲に含める。
        if (created.length && s.locked) {
          for (const cid of created) {
            try {
              await invoke("add_shape_member", { shapeId: id, cardId: cid });
            } catch (e) {
              console.error("add_shape_member failed", e);
            }
          }
          setShapes((prev) =>
            prev.map((sh) =>
              sh.id === id ? { ...sh, members: [...sh.members, ...created] } : sh
            )
          );
        }
        await refresh();
        setConfirm({
          title: "モジュール化",
          message: created.length
            ? `境界をまたぐ矢印 ${created.length} 本に IN/OUT カードを挿入しました。元に戻すときは各 IN/OUT の右クリック「このカードを溶解」を使ってください。`
            : "境界をまたぐフロー矢印はありませんでした（このモジュールは独立しています）。",
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
      } catch (e) {
        console.error("modularize_members failed", e);
        setConfirm({
          title: "モジュール化できませんでした",
          message: String(e),
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
      }
    },
    [cardsOnShape, refresh]
  );

  // v1.5.6 WS-M M1（§9.2）: IN/OUT の溶解＝矢印を直結に繋ぎ変えてカードを消す。
  const handleDissolveOp = useCallback(
    async (id: string) => {
      try {
        await invoke("dissolve_op", { id });
        await refresh();
      } catch (e) {
        console.error("dissolve_op failed", e);
        setConfirm({
          title: "溶解できませんでした",
          message: String(e),
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
      }
    },
    [refresh]
  );

  const handleShapeStyle = useCallback(async (id: string, fill: string, opacity: number) => {
    const s = shapesRef.current.find((sh) => sh.id === id);
    try {
      if (s && (s.fill !== fill || s.opacity !== opacity)) {
        pushUndo([{ kind: "shapeStyle", id, fill: s.fill, opacity: s.opacity }]);
      }
      await invoke("set_shape_style", { id, fill, opacity });
      setShapes((prev) => prev.map((s) => (s.id === id ? { ...s, fill, opacity } : s)));
    } catch (e) {
      console.error("set_shape_style failed", e);
    }
  }, [pushUndo]);

  const handleShapeRotate = useCallback(async (id: string, rotation: number) => {
    const s = shapesRef.current.find((sh) => sh.id === id);
    if (!s) return;
    if (s.rotation !== rotation) {
      const old = { id, x: s.pos_x, y: s.pos_y, w: s.width, h: s.height, rot: s.rotation };
      pushUndo([{ kind: "shapes", geoms: [old] }]);
    }
    try {
      await applyShapeGeoms([{ id, x: s.pos_x, y: s.pos_y, w: s.width, h: s.height, rot: rotation }]);
    } catch (e) {
      console.error("rotate shape failed", e);
    }
  }, [pushUndo, applyShapeGeoms]);

  // v1.4.29 ③: 「図形の拡縮（拡大/縮小）」は廃止し、「選択範囲にフィット」へ一本化。
  // 選択中のカード・コメントを囲む大きさに図形を合わせる（「選択を図形で囲む」＝
  // handleShapeAroundSelection と同じ範囲計算）。選択が空なら図形の上のカードに
  // フォールバックするので、ハンドルが隠れて掴めない旧来の用途もそのまま満たす。
  const handleShapeFitToCards = useCallback(async (id: string) => {
    const s = shapesRef.current.find((sh) => sh.id === id);
    if (!s) return;
    const selCards = objectsRef.current.filter((o) => selectedRef.current.has(o.id));
    const selNs = notesRef.current.filter((n) => selectedNotesRef.current.has(n.id));
    const useSel = selCards.length > 0 || selNs.length > 0;
    const cards = useSel ? selCards : cardsOnShape(s);
    const ns = useSel ? selNs : notesOnShape(s);
    if (!cards.length && !ns.length) return;
    let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
    for (const c of cards) {
      minX = Math.min(minX, c.pos_x);
      minY = Math.min(minY, c.pos_y);
      maxX = Math.max(maxX, c.pos_x + CARD_W);
      maxY = Math.max(maxY, c.pos_y + CARD_H);
    }
    for (const n of ns) {
      const sz = measuredNoteSize(n);
      minX = Math.min(minX, n.pos_x);
      minY = Math.min(minY, n.pos_y);
      maxX = Math.max(maxX, n.pos_x + sz.w);
      maxY = Math.max(maxY, n.pos_y + sz.h);
    }
    const PAD = 28;
    const labelPad = s.label ? 22 : 0; // ラベルの居場所を上に確保
    let x = minX - PAD, y = minY - PAD - labelPad;
    let w = maxX - minX + PAD * 2, h = maxY - minY + PAD * 2 + labelPad;
    // v1.4.25 ⑤ と同じく、スナップ中は外側方向へ丸めて格子に乗せる（選択部を必ず包含）。
    if (shapeSnap) {
      const g = SHAPE_GRID;
      const x2 = Math.ceil((x + w) / g) * g;
      const y2 = Math.ceil((y + h) / g) * g;
      x = Math.floor(x / g) * g;
      y = Math.floor(y / g) * g;
      w = x2 - x;
      h = y2 - y;
    }
    pushUndo([
      { kind: "shapes", geoms: [{ id, x: s.pos_x, y: s.pos_y, w: s.width, h: s.height, rot: s.rotation }] },
    ]);
    try {
      await applyShapeGeoms([{
        id,
        x: Math.round(x),
        y: Math.round(y),
        w: Math.round(w),
        h: Math.round(h),
        rot: s.rotation,
      }]);
    } catch (e) {
      console.error("fit shape failed", e);
    }
  }, [cardsOnShape, notesOnShape, measuredNoteSize, pushUndo, applyShapeGeoms, shapeSnap]);

  const handleShapeLabel = useCallback(
    (id: string, current: string) => {
      setPrompt({
        title: "図形の名前（グループ名）",
        initial: current,
        onConfirm: async (label) => {
          try {
            await invoke("set_shape_label", { id, label });
            setShapes((prev) => prev.map((s) => (s.id === id ? { ...s, label } : s)));
          } catch (e) {
            console.error("set_shape_label failed", e);
          }
        },
      });
    },
    []
  );

  const handleShapeZ = useCallback(async (id: string, front: boolean) => {
    const zs = shapesRef.current.map((s) => s.z_order);
    const z = front ? Math.max(0, ...zs) + 1 : Math.min(0, ...zs) - 1;
    try {
      await invoke("set_shape_z", { id, z });
      await refresh();
    } catch (e) {
      console.error("set_shape_z failed", e);
    }
  }, [refresh]);

  // --- v1.3 メモ⑦: 選択カードの整列 ------------------------------------------

  const alignSelected = useCallback(
    async (
      mode: "row" | "col" | "grid" | "grid-keep" | "dist-h" | "dist-v",
      // v1.4.x: 選択ではなく明示の集合（図形スコープ等）を整列対象にできる。
      source?: { cards: VirtualBodyInfo[]; notes: NoteInfo[] },
      // v1.5.12: 整列後にこの図形を結果の外接矩形へフィットさせる（図形スコープ整列用。
      // ロック中図形からのはみ出しを、図形側を広げることで回収する）。undo は移動と1手に束ねる。
      fitShapeId?: string
    ) => {
      // v1.3.5: コメント(付箋)も整列対象に含める。カードとコメントを位置だけで
      // 統一的に扱い、結果をカード／ノートの move に振り分ける（自動レイアウト＝
      // 重なり解消だけは、コメントを常時最前面の注釈として対象外のまま据え置く）。
      type It = { id: string; x: number; y: number; w: number; h: number; note: boolean };
      // objects/notes は created_at 順で届く（list_objects/list_notes の ORDER BY）＝配列順が制作順。
      const selCards = source?.cards ?? objectsRef.current.filter((o) => selectedRef.current.has(o.id));
      const selNotes = source?.notes ?? notesRef.current.filter((n) => selectedNotesRef.current.has(n.id));
      const items: It[] = [
        ...selCards.map((o) => {
          const s = layoutSize(o);
          return { id: o.id, x: o.pos_x, y: o.pos_y, w: s.w, h: s.h, note: false };
        }),
        ...selNotes.map((n) => {
          const s = measuredNoteSize(n);
          return { id: n.id, x: n.pos_x, y: n.pos_y, w: s.w, h: s.h, note: true };
        }),
      ];
      if (items.length < 2) return;
      const GAP = 28;
      let moved: It[];
      if (mode === "row") {
        // 横一列: 一番上のY にそろえる（X は保持）。
        const y = Math.round(Math.min(...items.map((o) => o.y)));
        moved = items.map((o) => ({ ...o, y }));
      } else if (mode === "col") {
        // 縦一列: 一番左のX にそろえる（Y は保持）。
        const x = Math.round(Math.min(...items.map((o) => o.x)));
        moved = items.map((o) => ({ ...o, x }));
      } else if (mode === "grid") {
        // v1.5.12 UIの気づき３-④: 旧方式（近い段/列へのクラスタリング＝v1.3.11 #14）は、
        // X・Y とも近い2枚が同一セルに丸められて完全に重なる欠陥があった。
        // 「順序で重なりなく詰め直す」方式へ全面変更:
        //   順序 = 矢印の上流→下流を優先（Kahn法。戻る矢印=閉路は制作順最小を強制的に
        //          取り出して断つ＝「できるだけ左→右・上→下」）。同順位は制作順。
        //   幾何 = 列幅・行高は各列/各行の最大実寸（展開パネル・付箋実測）＋GAP。
        //          セルを実寸で確保するため重なりは構造上起きない。
        const ids = new Set(items.map((it) => it.id));
        const ARROW_KINDS = new Set(["link", "flow", "req", "fetched"]); // group=論理所属は向きでない
        const outEdges = new Map<string, string[]>();
        const indeg = new Map<string, number>(items.map((it) => [it.id, 0]));
        for (const r of relationsRef.current) {
          if (!ARROW_KINDS.has(r.kind)) continue;
          if (!ids.has(r.from_id) || !ids.has(r.to_id) || r.from_id === r.to_id) continue;
          const lst = outEdges.get(r.from_id);
          if (lst) lst.push(r.to_id);
          else outEdges.set(r.from_id, [r.to_id]);
          indeg.set(r.to_id, (indeg.get(r.to_id) ?? 0) + 1);
        }
        const remaining = new Set(items.map((it) => it.id));
        const order: It[] = [];
        while (remaining.size) {
          // 上流が確定済み（入次数0）のうち制作順が最小のもの。無ければ閉路→制作順最小を強制。
          let pick: It | undefined;
          for (const it of items) {
            if (remaining.has(it.id) && (indeg.get(it.id) ?? 0) <= 0) { pick = it; break; }
          }
          if (!pick) for (const it of items) { if (remaining.has(it.id)) { pick = it; break; } }
          remaining.delete(pick!.id);
          order.push(pick!);
          for (const nb of outEdges.get(pick!.id) ?? [])
            if (remaining.has(nb)) indeg.set(nb, (indeg.get(nb) ?? 0) - 1);
        }
        const ncols = Math.ceil(Math.sqrt(order.length));
        const nrows = Math.ceil(order.length / ncols);
        const colW = new Array<number>(ncols).fill(CARD_W);
        const rowH = new Array<number>(nrows).fill(CARD_H);
        order.forEach((it, k) => {
          colW[k % ncols] = Math.max(colW[k % ncols], it.w);
          rowH[Math.floor(k / ncols)] = Math.max(rowH[Math.floor(k / ncols)], it.h);
        });
        const x0 = Math.round(Math.min(...items.map((o) => o.x)));
        const y0 = Math.round(Math.min(...items.map((o) => o.y)));
        const colX: number[] = [];
        for (let c = 0, acc = x0; c < ncols; c++) { colX[c] = acc; acc += colW[c] + GAP; }
        const rowY: number[] = [];
        for (let r = 0, acc = y0; r < nrows; r++) { rowY[r] = acc; acc += rowH[r] + GAP; }
        moved = order.map((it, k) => ({
          ...it,
          x: colX[k % ncols],
          y: rowY[Math.floor(k / ncols)],
        }));
      } else if (mode === "grid-keep") {
        // v1.5.12 UIの気づき３-③の後継: 「重なりを解消して配置」＝思考の流れを邪魔しない整列。
        // 手作業の空間配置（フロー図の地図）はそのまま、重なりだけ解消して格子に置き直す。
        // 各アイテムを「元位置に最も近い格子セル」へ割当て、埋まっていれば最寄りの空きセルを
        // リング探索で確保する。旧・重なり解消（押し出し反復＝連鎖して縦暴走）と
        // 旧・格子（近傍丸め＝同一セルに潰れて完全重なり）の両欠陥を同時に避ける決定的算法。
        const pw = CARD_W + GAP;
        const ph = CARD_H + GAP;
        const x0 = Math.round(Math.min(...items.map((o) => o.x)));
        const y0 = Math.round(Math.min(...items.map((o) => o.y)));
        const occupied = new Set<string>();
        // 展開パネル・大きい付箋は複数セルを占有させる（実寸で場所を取る＝重なりゼロの担保）。
        const spanOf = (it: It) => ({
          cw: Math.max(1, Math.ceil((it.w + GAP) / pw)),
          ch: Math.max(1, Math.ceil((it.h + GAP) / ph)),
        });
        const fits = (c: number, r: number, cw: number, ch: number) => {
          if (c < 0 || r < 0) return false;
          for (let dc = 0; dc < cw; dc++)
            for (let dr = 0; dr < ch; dr++) if (occupied.has(`${c + dc},${r + dr}`)) return false;
          return true;
        };
        const mark = (c: number, r: number, cw: number, ch: number) => {
          for (let dc = 0; dc < cw; dc++)
            for (let dr = 0; dr < ch; dr++) occupied.add(`${c + dc},${r + dr}`);
        };
        // 空きセル探索の優先順（実機FB 2026-07-11）: あふれは右→下→右下の順で逃がす。
        // 左・上への押し出しは最後の手段（図形の左上アンカー・読み順の期待を崩さない）。
        const ringOffsets = (rad: number) => {
          const cells: { dc: number; dr: number }[] = [];
          for (let dc = -rad; dc <= rad; dc++)
            for (let dr = -rad; dr <= rad; dr++)
              if (Math.max(Math.abs(dc), Math.abs(dr)) === rad) cells.push({ dc, dr });
          const score = (o: { dc: number; dr: number }) =>
            ((o.dc < 0 ? 1 : 0) + (o.dr < 0 ? 1 : 0)) * 100 + // 負方向（左・上）は最後
            (o.dr === 0 ? 0 : o.dc === 0 ? 10 : 20) +          // 右 → 下 → 右下（斜め）
            Math.min(Math.abs(o.dc), Math.abs(o.dr));           // 斜め成分が小さいほど先
          return cells.sort((a, b) => score(a) - score(b));
        };
        // 元位置の読み順（上→下・左→右）で確定していく＝押し退けられる方向が予測可能。
        const byPos = [...items].sort((a, b) => a.y - b.y || a.x - b.x);
        moved = [];
        for (const it of byPos) {
          const { cw, ch } = spanOf(it);
          const tc = Math.max(0, Math.round((it.x - x0) / pw));
          const tr = Math.max(0, Math.round((it.y - y0) / ph));
          let cell: { c: number; r: number } | null = null;
          search: for (let rad = 0; rad < 100 && !cell; rad++) {
            for (const o of ringOffsets(rad)) {
              if (fits(tc + o.dc, tr + o.dr, cw, ch)) { cell = { c: tc + o.dc, r: tr + o.dr }; break search; }
            }
          }
          const c = cell?.c ?? tc;
          const r = cell?.r ?? tr;
          mark(c, r, cw, ch);
          moved.push({ ...it, x: x0 + c * pw, y: y0 + r * ph });
        }
      } else {
        // 均等配置: 両端を固定し、間を等間隔に。
        const horiz = mode === "dist-h";
        const sorted = [...items].sort((a, b) => (horiz ? a.x - b.x : a.y - b.y));
        const first = sorted[0];
        const last = sorted[sorted.length - 1];
        const span = horiz ? last.x - first.x : last.y - first.y;
        const step = span / (sorted.length - 1);
        moved = sorted.map((o, i) => ({
          ...o,
          x: horiz ? Math.round(first.x + step * i) : o.x,
          y: horiz ? o.y : Math.round(first.y + step * i),
        }));
      }
      const cardMoves = moved.filter((m) => !m.note).map((m) => ({ id: m.id, x: m.x, y: m.y }));
      const noteMoves = moved.filter((m) => m.note).map((m) => ({ id: m.id, x: m.x, y: m.y }));
      const fitShape = fitShapeId ? shapesRef.current.find((sh) => sh.id === fitShapeId) : undefined;
      const undo: UndoAction[] = [];
      if (cardMoves.length)
        undo.push({ kind: "objects", moves: selCards.map((o) => ({ id: o.id, x: o.pos_x, y: o.pos_y })) });
      if (noteMoves.length)
        undo.push({ kind: "notes", moves: selNotes.map((n) => ({ id: n.id, x: n.pos_x, y: n.pos_y })) });
      if (fitShape)
        undo.push({
          kind: "shapes",
          geoms: [{ id: fitShape.id, x: fitShape.pos_x, y: fitShape.pos_y, w: fitShape.width, h: fitShape.height, rot: fitShape.rotation }],
        });
      pushUndo(undo);
      try {
        if (cardMoves.length) await applyObjectMoves(cardMoves);
        if (noteMoves.length) await applyNoteMoves(noteMoves);
        if (fitShape) {
          // 整列結果の外接矩形へ図形をフィット（handleShapeFitToCards と同じ余白・スナップ規則）。
          let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
          for (const m of moved) {
            minX = Math.min(minX, m.x);
            minY = Math.min(minY, m.y);
            maxX = Math.max(maxX, m.x + m.w);
            maxY = Math.max(maxY, m.y + m.h);
          }
          const PAD = 28;
          const labelPad = fitShape.label ? 22 : 0;
          let fx = minX - PAD, fy = minY - PAD - labelPad;
          let fw = maxX - minX + PAD * 2, fh = maxY - minY + PAD * 2 + labelPad;
          if (shapeSnap) {
            const g = SHAPE_GRID;
            const x2 = Math.ceil((fx + fw) / g) * g;
            const y2 = Math.ceil((fy + fh) / g) * g;
            fx = Math.floor(fx / g) * g;
            fy = Math.floor(fy / g) * g;
            fw = x2 - fx;
            fh = y2 - fy;
          }
          await applyShapeGeoms([
            { id: fitShape.id, x: Math.round(fx), y: Math.round(fy), w: Math.round(fw), h: Math.round(fh), rot: fitShape.rotation },
          ]);
        }
      } catch (e) {
        console.error("align failed", e);
      }
    },
    [pushUndo, applyObjectMoves, applyNoteMoves, layoutSize, measuredNoteSize, applyShapeGeoms, shapeSnap]
  );

  // v1.4.x: 図形スコープの格子整列 = ビジュアルコマンド（空間的 foreach）の第一歩。
  // 図形に乗っているカード/コメントだけを格子に並べ直す（選択に依存しない）。
  // v1.5.12: 並び順は選択整列と同じ（矢印の上流→下流優先・同順位は制作順）。
  // 図形スコープの整列は最後に図形を結果へフィットさせる（実機FB: ロック中図形からの
  // はみ出しを図形側を広げて回収。「整列→フィット」で1手のundo）。
  const handleShapeAlignGrid = useCallback(
    (id: string) => {
      const s = shapesRef.current.find((sh) => sh.id === id);
      if (!s) return;
      void alignSelected("grid", { cards: cardsOnShape(s), notes: notesOnShape(s) }, id);
    },
    [alignSelected, cardsOnShape, notesOnShape]
  );

  // v1.5.12: 図形右クリック「重なりを解消して配置」＝配置（思考の地図）を保つ整列＋図形フィット。
  const handleShapeArrangeKeep = useCallback(
    (id: string) => {
      const s = shapesRef.current.find((sh) => sh.id === id);
      if (!s) return;
      void alignSelected("grid-keep", { cards: cardsOnShape(s), notes: notesOnShape(s) }, id);
    },
    [alignSelected, cardsOnShape, notesOnShape]
  );

  // v1.3.11 #1: 借用元（origin_path）が属するプロジェクトを開く。プロジェクト内なら
  // 切り替え、そうでなければ借用元フォルダをエクスプローラで表示。
  const handleOpenOriginProject = useCallback(
    async (path: string) => {
      try {
        const info = await invoke<ProjectInfo | null>("open_origin_project", { path });
        if (info) {
          setProject(info);
        } else {
          await invoke("reveal_in_dir", { path }).catch(() => {});
        }
      } catch (e) {
        console.error("open_origin_project failed", e);
        await invoke("reveal_in_dir", { path }).catch(() => {});
      }
    },
    []
  );

  // v1.5.12 UIの気づき３-③: 自動レイアウト（重なり解消）は廃止。ペア毎の押し出し反復は
  // 連鎖して縦に暴走する欠陥があり、役割は「格子状に整列」（順序で詰め直し・重なり構造ゼロ）が引き継ぐ。

  // Bounding box of all cards (incl. expanded panels) and notes — 全体を表示用.
  const getContentBounds = useCallback(() => {
    let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
    for (const o of objectsRef.current) {
      const s = layoutSize(o);
      minX = Math.min(minX, o.pos_x);
      minY = Math.min(minY, o.pos_y);
      maxX = Math.max(maxX, o.pos_x + s.w);
      maxY = Math.max(maxY, o.pos_y + s.h);
    }
    for (const n of notes) {
      const s = measuredNoteSize(n);
      minX = Math.min(minX, n.pos_x);
      minY = Math.min(minY, n.pos_y);
      maxX = Math.max(maxX, n.pos_x + s.w);
      maxY = Math.max(maxY, n.pos_y + s.h);
    }
    for (const s of shapesRef.current) {
      minX = Math.min(minX, s.pos_x);
      minY = Math.min(minY, s.pos_y);
      maxX = Math.max(maxX, s.pos_x + s.width);
      maxY = Math.max(maxY, s.pos_y + s.height);
    }
    if (minX === Infinity) return null;
    return { x: minX, y: minY, w: maxX - minX, h: maxY - minY };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [layoutSize, notes]);

  // Bounding box of the selected cards/notes — 選択範囲にフォーカス用.
  const getSelectionBounds = useCallback(() => {
    const sel = selectedRef.current;
    const selN = selectedNotesRef.current;
    if (!sel.size && !selN.size) return null;
    let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
    for (const o of objectsRef.current) {
      if (!sel.has(o.id)) continue;
      const s = layoutSize(o);
      minX = Math.min(minX, o.pos_x);
      minY = Math.min(minY, o.pos_y);
      maxX = Math.max(maxX, o.pos_x + s.w);
      maxY = Math.max(maxY, o.pos_y + s.h);
    }
    for (const n of notesRef.current) {
      if (!selN.has(n.id)) continue;
      const s = measuredNoteSize(n);
      minX = Math.min(minX, n.pos_x);
      minY = Math.min(minY, n.pos_y);
      maxX = Math.max(maxX, n.pos_x + s.w);
      maxY = Math.max(maxY, n.pos_y + s.h);
    }
    if (minX === Infinity) return null;
    return { x: minX, y: minY, w: maxX - minX, h: maxY - minY };
  }, [layoutSize]);

  const handleFocusSelection = useCallback(() => {
    canvasApi.current?.fitTo(getSelectionBounds());
  }, [getSelectionBounds]);

  // v1.4.19 検索・ゴミ箱復活から「IDを指定してフォーカス」する版。選択も置き換える。
  // setSelected 直後は selectedRef が古いので、bounds は引数の ID から直接計算する。
  const focusCards = useCallback(
    (cardIds: string[], noteIds: string[]) => {
      setSelected(new Set(cardIds));
      setSelectedNotes(new Set(noteIds));
      const cs = new Set(cardIds);
      const ns = new Set(noteIds);
      let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
      for (const o of objectsRef.current) {
        if (!cs.has(o.id)) continue;
        const s = layoutSize(o);
        minX = Math.min(minX, o.pos_x);
        minY = Math.min(minY, o.pos_y);
        maxX = Math.max(maxX, o.pos_x + s.w);
        maxY = Math.max(maxY, o.pos_y + s.h);
      }
      for (const n of notesRef.current) {
        if (!ns.has(n.id)) continue;
        const s = measuredNoteSize(n);
        minX = Math.min(minX, n.pos_x);
        minY = Math.min(minY, n.pos_y);
        maxX = Math.max(maxX, n.pos_x + s.w);
        maxY = Math.max(maxY, n.pos_y + s.h);
      }
      if (minX === Infinity) return;
      canvasApi.current?.fitTo({ x: minX, y: minY, w: maxX - minX, h: maxY - minY });
    },
    [layoutSize, measuredNoteSize, setSelected, setSelectedNotes]
  );

  // ゴミ箱からの復活後: refresh のコミットを待ってから該当カードへフォーカス
  //（図形は選択対象外なので図形矩形へ直接 fitTo）。
  const handleTrashRestored = useCallback(
    async (id: string, kind: "card" | "note" | "shape") => {
      await refresh();
      // setObjects のコミット後に objectsRef が新しくなるのを待つ（setTimeout(0) の既存イディオム）。
      setTimeout(() => {
        if (kind === "shape") {
          const s = shapesRef.current.find((sh) => sh.id === id);
          if (s) canvasApi.current?.fitTo({ x: s.pos_x, y: s.pos_y, w: s.width, h: s.height });
          return;
        }
        focusCards(kind === "card" ? [id] : [], kind === "note" ? [id] : []);
      }, 0);
    },
    [refresh, focusCards]
  );

  // Notes (free text annotations)
  const handleAddNote = useCallback(async (canvasX: number, canvasY: number, linkFrom?: string) => {
    try {
      const id = await invoke<string>("create_note", { text: "", x: canvasX, y: canvasY });
      pushUndo([{ kind: "deleteNotes", ids: [id] }]);
      setNewNoteId(id);
      await refresh();
      if (linkFrom) await linkNewNode(linkFrom, id);
    } catch (e) {
      console.error("create_note failed", e);
    }
  }, [refresh, pushUndo, linkNewNode]);

  // 後方定義のハンドラを ref に載せ、handleStartLink（前方）から呼べるようにする。
  // メニュー再編（2026-07-12）: 旧「テキストカード」（handleAddTextCard）は
  // 「新規ファイルを作成 → テキスト（.txt）」（createEmpty）に統合。
  linkActionsRef.current = {
    file: handleContextPlace,
    newFile: handleNewFile,
    createEmpty: handleCreateEmptyFile,
    createEmptyOther: handleCreateEmptyFileOther,
    paste: handlePaste,
    url: handleAddUrl,
    filter: handleAddFilter,
    launcher: handleAddLauncher,
    note: handleAddNote,
    libLauncher: handleAddLibraryLauncher,
    libFilter: handleAddLibraryFilter,
    libModule: handleAddLibraryModule,
  };

  const handleNoteMove = useCallback(async (id: string, x: number, y: number) => {
    const prev = notesRef.current.find((n) => n.id === id);
    // v1.5.9: ロック中図形のメンバー付箋のドラッグアウトは弾く（カードと共通の保護）。
    if (prev) {
      const viol = noteLockViolation(id, x, y);
      if (viol) {
        // 座標不変だと VDOM は書き戻さないので、命令的に元位置へ戻す（矢印も再描画）。
        const el = document.querySelector<HTMLElement>(`[data-note-id="${CSS.escape(id)}"]`);
        if (el) el.style.transform = `translate(${prev.pos_x}px, ${prev.pos_y}px)`;
        redrawArrowsForRef.current(new Map([[id, { x: prev.pos_x, y: prev.pos_y }]]));
        setConfirm({
          title: "モジュールがロックされています",
          message: `このコメントは 🔒「${viol.label || "図形"}」のメンバーです。外へ出すには、図形メニューの「メンバーをロック」を解除してください。`,
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
        return;
      }
    }
    try {
      await invoke("move_note", { id, x, y });
      if (prev && (prev.pos_x !== x || prev.pos_y !== y)) {
        pushUndo([{ kind: "notes", moves: [{ id, x: prev.pos_x, y: prev.pos_y }] }]);
      }
      setNotes((prev) => prev.map((n) => (n.id === id ? { ...n, pos_x: x, pos_y: y } : n)));
    } catch (e) {
      console.error("move_note failed", e);
    }
  }, [pushUndo, noteLockViolation]);

  const handleNoteSave = useCallback(async (id: string, text: string) => {
    try {
      const note = notes.find((n) => n.id === id);
      if (note?.file_ref) {
        // File-linked note: write back to the real file (in-canvas editing).
        await invoke("write_text_file", { path: note.file_ref, text });
        refresh(); // pick up mtime → dirty state on the source object
      } else {
        await invoke("update_note", { id, text });
        setNotes((prev) => prev.map((n) => (n.id === id ? { ...n, text } : n)));
      }
      setNewNoteId((cur) => (cur === id ? null : cur));
    } catch (e) {
      console.error("note save failed", e);
    }
  }, [notes, refresh]);

  const handleNoteResize = useCallback(
    async (id: string, width: number) => {
      const note = notes.find((n) => n.id === id);
      if (!note) return;
      try {
        await invoke("set_note_style", {
          id,
          width,
          fontSize: note.font_size,
          bold: note.bold,
        });
        if (note.width !== width) {
          pushUndo([{
            kind: "noteStyle", id, width: note.width, fontSize: note.font_size, bold: note.bold,
          }]);
        }
        setNotes((prev) => prev.map((n) => (n.id === id ? { ...n, width } : n)));
      } catch (e) {
        console.error("set_note_style failed", e);
      }
    },
    [notes, pushUndo]
  );

  const handleNoteStyle = useCallback(
    async (id: string, fontSize: number, bold: boolean) => {
      const note = notes.find((n) => n.id === id);
      if (!note) return;
      try {
        await invoke("set_note_style", { id, width: note.width, fontSize, bold });
        if (note.font_size !== fontSize || note.bold !== bold) {
          pushUndo([{
            kind: "noteStyle", id, width: note.width, fontSize: note.font_size, bold: note.bold,
          }]);
        }
        setNotes((prev) =>
          prev.map((n) => (n.id === id ? { ...n, font_size: fontSize, bold } : n))
        );
      } catch (e) {
        console.error("set_note_style failed", e);
      }
    },
    [notes, pushUndo]
  );

  // v1.4.16 付箋の充実: 付箋の色を設定（color=null で既定の黄へ）。楽観更新＋refresh不要。
  const handleNoteColor = useCallback(async (id: string, color: string | null) => {
    try {
      await invoke("set_note_color", { id, color });
      setNotes((prev) => prev.map((n) => (n.id === id ? { ...n, color } : n)));
    } catch (e) {
      console.error("set_note_color failed", e);
    }
  }, []);

  // v1.4.16 付箋の充実: 先頭行を見出しにする（titleSize=null で見出しなし）。
  const handleNoteTitle = useCallback(async (id: string, titleSize: number | null) => {
    try {
      await invoke("set_note_title", { id, titleSize });
      setNotes((prev) => prev.map((n) => (n.id === id ? { ...n, title_size: titleSize } : n)));
    } catch (e) {
      console.error("set_note_title failed", e);
    }
  }, []);

  // 自由なカラーピッカー: 一時的な <input type=color> を生成してダイアログを開く。
  const handleNoteColorPick = useCallback(
    (id: string) => {
      const cur = notesRef.current.find((n) => n.id === id)?.color;
      const input = document.createElement("input");
      input.type = "color";
      input.value = cur && /^#[0-9a-fA-F]{6}$/.test(cur) ? cur : "#3a3520";
      input.style.position = "fixed";
      input.style.left = "-9999px";
      document.body.appendChild(input);
      const done = () => {
        const v = input.value;
        input.remove();
        handleNoteColor(id, v);
      };
      input.addEventListener("change", done, { once: true });
      input.click();
    },
    [handleNoteColor]
  );

  const handleDeleteNote = useCallback(async (id: string) => {
    // v1.5.9: ロック中図形のメンバー付箋は削除も不可（カードの delete ガードと共通）。
    const lockShape = lockedMemberShape(id);
    if (lockShape) {
      setConfirm({
        title: "モジュールがロックされています",
        message: `このコメントは 🔒「${lockShape.label || "図形"}」のメンバーです。削除するには、先に図形メニューの「メンバーをロック」を解除してください。`,
        okLabel: "OK",
        onConfirm: () => setConfirm(null),
      });
      return;
    }
    try {
      await invoke("delete_note", { id });
      // 復活の呪文: 矢印リンクの行も残っているので、Ctrl+Zで一緒に戻る。
      pushUndo([{ kind: "restoreNotes", ids: [id] }]);
      setNotes((prev) => prev.filter((n) => n.id !== id));
    } catch (e) {
      console.error("delete_note failed", e);
    }
  }, [pushUndo, lockedMemberShape]);

  // v1.3-④: コメントを実ファイル化（以後はファイルリンク付箋として編集が書き戻る）。
  // v1.3.5: コメントの内容から独立したテキスト仮身カードを作る（実身化の後継）。
  // コメントは一切変更しない。コピペのテキスト仮身（create_text_object）と同一経路。
  const handleNoteToTextCard = useCallback(async (id: string) => {
    const note = notes.find((n) => n.id === id);
    if (!note) return;
    try {
      const vid = await invoke<string>("create_text_object", {
        text: note.text,
        x: note.pos_x + note.width + 28, // コメントの右隣に置く（重ならない）
        y: note.pos_y,
      });
      pushUndo([{ kind: "delete", ids: [vid] }]);
      await refresh();
    } catch (e) {
      console.error("note→text card failed", e);
    }
  }, [notes, pushUndo, refresh]);

  // v1.8.7: カードの実行ログを開く（右クリック・カード面のエラー印の両方から）。
  const handleShowTrace = useCallback((id: string, label: string) => {
    void invoke<string | null>("get_card_trace", { id })
      .then((raw) => {
        let trace: CardTrace | null = null;
        if (raw) {
          try {
            trace = JSON.parse(raw) as CardTrace;
          } catch {
            trace = null;
          }
        }
        setTraceDlg({ label, trace });
      })
      .catch(() => setTraceDlg({ label, trace: null }));
  }, []);

  const handleContextMenu = useCallback(
    (info: { clientX: number; clientY: number; canvasX: number; canvasY: number }) => {
      const el = document.elementFromPoint(info.clientX, info.clientY);

      // App のハンドラ群をメニュービルダー（ui/menuBuilder.ts）への窓口にまとめる。
      // サブメニューを同位置に開き直す（既存メニューのクローズ後に）。v1.5.9: バックアップ
      // 取り込みのような「非同期に組み立てるサブメニュー」からも使うため名前付きで持つ。
      const openSubmenu = (sub: MenuItem[]) =>
        window.setTimeout(() => setMenu({ x: info.clientX, y: info.clientY, items: sub }), 0);
      const actions: MenuActions = {
        openSubmenu,
        contextPlace: handleContextPlace,
        newFile: handleNewFile,
        createEmptyFile: handleCreateEmptyFile,
        createEmptyFileOther: handleCreateEmptyFileOther,
        addNote: handleAddNote,
        paste: handlePaste,
        shapeAdd: handleShapeAdd,
        addUrl: handleAddUrl,
        addFilter: handleAddFilter,
        runFlow: handleRunFlow,
        evaluateFlow: handleEvaluateFlow,
        toggleOpPause: handleToggleOpPause,
        // v1.5.6 WS-M M1: 図形ロック・モジュール化・IN/OUT 溶解。
        shapeToggleLock: handleShapeToggleLock,
        modularizeShape: handleModularizeShape,
        selectShapeMembers: handleSelectShapeMembers,
        dissolveOp: handleDissolveOp,
        addLauncher: handleAddLauncher,
        runLauncher: handleRunLauncher,
        addLibraryLauncher: handleAddLibraryLauncher,
        saveLauncherToLibrary: handleSaveLauncherToLibrary,
        deleteLibraryLauncher: handleDeleteLibraryLauncher,
        editFilter: handleEditFilter,
        inputOrder: (id: string, label: string) => setInputOrderDlg({ id, label }),
        saveFilterToLibrary: handleSaveFilterToLibrary,
        addLibraryFilter: handleAddLibraryFilter,
        deleteLibraryFilter: handleDeleteLibraryFilter,
        // v1.5.12 メニュー再編: ライブラリ名の編集（右クリック）とカテゴリ移動。
        renameLibrary: handleRenameLibrary,
        moveLibraryCategory: handleMoveLibraryCategory,
        // v1.5.8 WS-M M2: モジュールライブラリ（登録・展開・削除・全書き出し）。
        saveModuleToLibrary: handleSaveModuleToLibrary,
        addLibraryModule: handleAddLibraryModule,
        deleteLibraryModule: handleDeleteLibraryModule,
        exportLibraries: handleExportLibraries,
        importLibraries: () => void handleImportLibraries(openSubmenu),
        feedDrop: handleFeedDrop,
        runDrop: runDownstreamFlows,
        newSubproject: handleNewSubproject,
        linkProject: handleLinkProject,
        // メニュー再編（2026-07-12）: 登録済み（最近開いた）プロジェクトの一覧から選んで
        // リンクカードを置く（旧・矢印引き出しメニューの「既存プロジェクトへのリンク」）。
        // sendSelection と同じ「非同期で一覧取得→サブメニューを開き直す」定石。
        linkRegisteredProject: async (x, y) => {
          let recents: ProjectInfo[] = [];
          try {
            recents = await invoke<ProjectInfo[]>("list_recent_projects");
          } catch (e) {
            console.error("list_recent_projects failed", e);
          }
          const curPath = currentProjectRef.current?.path;
          const items: MenuItem[] = [
            ...recents
              .filter((r) => r.path !== curPath)
              .map((r) => ({
                label: `📁 ${r.name}`,
                hint: r.path,
                onClick: () => void linkProjectByPath(r.path, x, y),
              })),
            { label: "その他のプロジェクト...", onClick: () => void handleLinkProject(x, y) },
          ];
          window.setTimeout(() => setMenu({ x: info.clientX, y: info.clientY, items }), 0);
        },
        // v1.8.8: プロジェクトフォルダ直下のフォルダ（内包プロジェクト／普通のフォルダ）への
        // リンクカードを置く。linkRegisteredProject と同じ「一覧取得→サブメニュー」定石。
        linkInnerFolders: async (x, y) => {
          let list: InnerFolder[] = [];
          try {
            list = await invoke<InnerFolder[]>("list_inner_folders");
          } catch (e) {
            console.error("list_inner_folders failed", e);
          }
          const unlinked = list.filter((f) => !f.linked);
          const items: MenuItem[] = list.length
            ? [
                ...(unlinked.length >= 2
                  ? [
                      {
                        label: `未配置をまとめて配置（${unlinked.length}件）`,
                        hint: "まだリンクカードが無い内包フォルダすべてを、この場所から格子状に並べて配置します（Ctrl+Z で一括で戻せます）。",
                        onClick: () => void placeInnerFolders(unlinked.map((f) => f.path), x, y),
                      },
                      { divider: true, label: "", onClick: () => {} },
                    ]
                  : []),
                ...list.map((f) => ({
                  label: `${f.linked ? "✓ " : "　"}${f.is_project ? "🗂" : "📁"} ${f.name}${f.is_project ? "（プロジェクト）" : ""}`,
                  hint: `${f.path}${f.linked ? "\n（既にこのキャンバスにリンクカードがあります。選ぶともう1枚置きます）" : ""}`,
                  onClick: () => void placeInnerFolders([f.path], x, y),
                })),
              ]
            : [{ label: "（内包するフォルダはありません）", onClick: () => {}, disabled: true }];
          window.setTimeout(() => setMenu({ x: info.clientX, y: info.clientY, items }), 0);
        },
        commitAll: handleCommitAll,
        openSearch: () => setSearchOpen(true),
        openTrash: () => setTrashOpen(true),
        cleanCaches: handleCleanCaches,
        gcCas: handleGcCas,
        setTheme,
        uiAdjust: handleUiAdjust,
        toggleMenuHints: () =>
          setShowMenuHints((v) => {
            const nv = !v;
            localStorage.setItem("texos-menu-hints", nv ? "1" : "0");
            return nv;
          }),
        // v1.8.7 実行トレース
        showTrace: handleShowTrace,
        // v1.8.7 途中出力スイッチ（emit_mid）。未知キーを温存したまま1キーだけ切り替える。
        toggleEmitMid: (id: string, currentSpec: string) => {
          let s: Record<string, unknown>;
          try {
            s = JSON.parse(currentSpec || "{}");
          } catch {
            s = {};
          }
          if (s.emit_mid === "on") delete s.emit_mid;
          else s.emit_mid = "on";
          void invoke("update_filter", { id, spec: JSON.stringify(s) })
            .then(() => refresh())
            .catch((e) =>
              setConfirm({
                title: "設定を変更できませんでした",
                message: String(e),
                okLabel: "OK",
                onConfirm: () => setConfirm(null),
              })
            );
        },
        toggleDebugTrace: () =>
          setDebugTrace((v) => {
            const nv = !v;
            localStorage.setItem("texos-debug-trace", nv ? "1" : "0");
            return nv;
          }),
        // v1.5.4 WS-P2: リクエスト矢印（req）の表示トグル（永続化）。
        toggleReqArrows: () =>
          setShowReqArrows((v) => {
            const nv = !v;
            localStorage.setItem("texos-show-req", nv ? "1" : "0");
            return nv;
          }),
        toggleCardHints: () =>
          setShowCardHints((v) => {
            const nv = !v;
            localStorage.setItem("texos-card-hints", nv ? "1" : "0");
            return nv;
          }),
        toggleShapeSnap: () =>
          setShapeSnap((v) => {
            const nv = !v;
            localStorage.setItem("texos-shape-snap", nv ? "1" : "0");
            return nv;
          }),
        snapAllShapes: handleSnapAllShapes,
        switchProject: () => setShowChooser(true),
        showHelp: () => setHelpOpen(true),
        focusSelection: handleFocusSelection,
        selectConnected: handleSelectConnected,
        alignSelected,
        bulkCommit: handleBulkCommit,
        bulkUncommit: handleBulkUncommit,
        bulkDelete: handleBulkDelete,
        // v1.4.14: 選択を別プロジェクトへ（送り先の最近一覧を非同期取得してサブメニューを開く）。
        sendSelection: async (mode) => {
          let recents: ProjectInfo[] = [];
          try {
            recents = await invoke<ProjectInfo[]>("list_recent_projects");
          } catch (e) {
            console.error("list_recent_projects failed", e);
          }
          const curPath = currentProjectRef.current?.path;
          const items = [
            ...recents
              .filter((r) => r.path !== curPath)
              .map((r) => ({
                label: `📁 ${r.name}`,
                hint: r.path,
                onClick: () => handleSendSelectionToProject(mode, r.path, r.name),
              })),
            {
              label: "その他のプロジェクト...",
              onClick: () => handleSendSelectionPickFolder(mode),
            },
          ];
          window.setTimeout(() => setMenu({ x: info.clientX, y: info.clientY, items }), 0);
        },
        copyFiles: handleCopyFiles,
        copyImage: handleCopyImage,
        view: handleOpenObject,
        openObject: handleOpenObject,
        openDefault: handleOpenDefault,
        openWith: handleOpenWith,
        commitReveal: handleCommitReveal,
        openOriginProject: handleOpenOriginProject,
        relinkFolder: handleRelinkFolder,
        relinkProject: handleRelinkProject,
        relink: handleRelink,
        restoreFromCas: handleRestoreFromCas,
        editInCanvas: handleEditInCanvas,
        expandView: handleExpandView,
        collapse: handleCollapse,
        viewTemp: handleViewTemp,
        recommit: handleRecommit,
        uncommit: handleUncommit,
        duplicate: handleDuplicate,
        makeReal: handleMakeReal,
        renameObject: handleRenameObject,
        renameReal: handleRenameReal,
        setMemo: handleSetMemo,
        changeUrl: handleChangeUrl,
        checkUrlUpdate: handleCheckUrlUpdate,
        refetchUrl: handleRefetchUrl,
        checkAllUrlUpdates: handleCheckAllUrlUpdates,
        deleteObject: handleDeleteObject,
        requestDelete: handleRequestDelete,
        bringToFront: handleBringToFront,
        sendToBack: handleSendToBack,
        noteStyle: handleNoteStyle,
        noteColor: handleNoteColor,
        noteColorPick: handleNoteColorPick,
        noteTitle: handleNoteTitle,
        noteToTextCard: handleNoteToTextCard,
        deleteNote: handleDeleteNote,
        shapeClickSelect: handleShapeClickSelect,
        shapeAroundSelection: handleShapeAroundSelection,
        shapeStyle: handleShapeStyle,
        shapeRotate: handleShapeRotate,
        shapeFitToCards: handleShapeFitToCards,
        shapeAlignGrid: handleShapeAlignGrid,
        shapeArrangeKeep: handleShapeArrangeKeep,
        shapeLabel: handleShapeLabel,
        shapeZ: handleShapeZ,
        shapeDelete: handleShapeDelete,
      };

      // Note (comment) hit-test
      const noteEl = el?.closest<HTMLElement>("[data-note-id]");
      const noteId = noteEl?.dataset.noteId;
      if (noteId) {
        setMenu({
          x: info.clientX,
          y: info.clientY,
          items: buildNoteMenu(noteId, notes.find((n) => n.id === noteId), actions),
        });
        return;
      }

      const targetEl = el?.closest<HTMLElement>("[data-object-id]");
      const targetId = targetEl?.dataset.objectId;

      // v1.3 メモ⑦／メモ２追加④: 整列・一括操作のサブメニュー素材。
      const alignItems = buildAlignItems(actions);
      const selObjs = objectsRef.current.filter((o) => selectedRef.current.has(o.id));
      const bulkItems = buildBulkItems(
        {
          commitable: selObjs.filter(
            (o) =>
              !o.committed_path && !o.project_ref && !o.url_ref && !o.folder_ref && !o.filter_ref
          ).length,
          uncommitable: selObjs.filter(
            (o) => !!o.committed_path && !!o.origin_path && o.origin_path !== o.committed_path
          ).length,
          total: selObjs.length,
          notes: notesRef.current.filter((n) => selectedNotesRef.current.has(n.id)).length,
          ids: selObjs.map((o) => o.id),
        },
        actions
      );

      let items: MenuItem[];
      if (targetId) {
        items = buildObjectMenu(
          targetId,
          objectsRef.current.find((o) => o.id === targetId),
          { selected: selectedRef.current, expanded, alignItems, bulkItems },
          actions
        );
      } else if (
        el?.closest<HTMLElement>("[data-shape-id]") &&
        selectedShapesRef.current.has(el.closest<HTMLElement>("[data-shape-id]")!.dataset.shapeId!)
      ) {
        // v1.3-②③: shape menu (shapes sit behind cards)。
        // v1.3.7 memo②: 図形メニューは「その図形が選択中のとき」だけ。未選択の図形上の
        // 右クリックはキャンバスメニューにフォールバック（カテゴリ図形の上にファイル配置等
        // ができるように）。図形は左クリックで選択 → 右クリックで図形メニュー、の流れ。
        const shapeId = el.closest<HTMLElement>("[data-shape-id]")!.dataset.shapeId!;
        const sh = shapesRef.current.find((s) => s.id === shapeId);
        // v1.3.7: 上に「未選択の」カード/コメントが残っている時だけ「上のカードを
        // まとめて選択」を出す（普通クリックで全選択済みの流れでは冗長なので隠す）。
        const hasUnselectedAbove =
          !!sh &&
          (cardsOnShape(sh).some((c) => !selectedRef.current.has(c.id)) ||
            notesOnShape(sh).some((n) => !selectedNotesRef.current.has(n.id)));
        // v1.4.x: 図形に乗るカード/コメントが2枚以上なら「格子状に整列」を出す。
        const canArrangeAbove =
          !!sh && cardsOnShape(sh).length + notesOnShape(sh).length >= 2;
        items = buildShapeMenu(shapeId, sh, hasUnselectedAbove, canArrangeAbove, actions);
      } else {
        items = buildCanvasMenu(
          info,
          {
            selectionCount: selectedRef.current.size,
            noteCount: selectedNotesRef.current.size,
            theme,
            alignItems,
            bulkItems,
            library: filterLibrary.map((e) => e.name),
            launcherLibrary: launcherLibrary.map((e) => e.name),
            moduleLibrary: moduleLibrary.map((e) => e.name),
            audioDevices: audioDevicesRef.current,
            menuHints: showMenuHints,
            cardHints: showCardHints,
            shapeSnap,
            showReqArrows,
            debugTrace,
          },
          actions
        );
      }
      setMenu({ x: info.clientX, y: info.clientY, items });
    },
    [
      handleContextPlace,
      handleDeleteObject,
      handleRenameObject,
      handleRenameReal,
      handleOpenObject,
      handleOpenDefault,
      handleOpenWith,
      handleRequestDelete,
      handleRecommit,
      handleUncommit,
      handleDuplicate,
      handleSelectConnected,
      handleSendSelectionToProject,
      handleSendSelectionPickFolder,
      handleCleanCaches,
      handleGcCas,
      handleUiAdjust,
      handleViewTemp,
      handleNewSubproject,
      handleLinkProject,
      handleCommitAll,
      handleAddNote,
      handleDeleteNote,
      handleAddUrl,
      handleChangeUrl,
      handleCheckUrlUpdate,
      handleRefetchUrl,
      handleCheckAllUrlUpdates,
      handleSetMemo,
      handleNoteStyle,
      handleNoteColor,
      handleNoteColorPick,
      handleNoteTitle,
      handleEditInCanvas,
      handleExpandView,
      handleCollapse,
      handleBringToFront,
      handleSendToBack,
      handleCommitReveal,
      handleRelink,
      handleRelinkFolder,
      handleRelinkProject,
      placeInnerFolders,
      handleRestoreFromCas,
      handleFocusSelection,
      handlePaste,
      handleShapeAdd,
      handleShapeAroundSelection,
      handleSnapAllShapes,
      handleShapeClickSelect,
      handleShapeStyle,
      handleShapeRotate,
      handleShapeFitToCards,
      handleShapeLabel,
      handleShapeZ,
      handleShapeDelete,
      alignSelected,
      handleBulkCommit,
      handleBulkUncommit,
      handleBulkDelete,
      handleNoteToTextCard,
      handleAddFilter,
      handleRunFlow,
      handleEditFilter,
      handleSaveFilterToLibrary,
      handleAddLibraryFilter,
      handleDeleteLibraryFilter,
      handleSaveModuleToLibrary,
      handleAddLibraryModule,
      handleDeleteLibraryModule,
      handleExportLibraries,
      handleImportLibraries,
      handleAddLauncher,
      handleRunLauncher,
      handleAddLibraryLauncher,
      handleSaveLauncherToLibrary,
      handleDeleteLibraryLauncher,
      handleFeedDrop,
      runDownstreamFlows,
      theme,
      showMenuHints,
      showCardHints,
      shapeSnap,
      debugTrace,
      handleShowTrace,
      expanded,
      notes,
      filterLibrary,
      launcherLibrary,
      moduleLibrary,
      cardsOnShape,
      notesOnShape,
    ]
  );

  // Esc = 選択の全解除のみ（v1.3.8）。展開パネル/プレビューは閉じない
  // （プレビューはダブルクリックで開閉トグル／右上×／RMBメニューで閉じる、と手段が揃って
  //  いるため、Esc で閉じると意図しない動作になる — ユーザー指摘 2026-06-19, トグルは v1.3.10）。
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      // v1.4.12: ヘルプが開いていれば Esc はまずそれを閉じる（選択は解除しない）。
      if (helpOpen) {
        setHelpOpen(false);
        return;
      }
      setSelected(new Set());
      setSelectedNotes(new Set());
      setSelectedShapes(new Set());
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [helpOpen, setSelected, setSelectedNotes, setSelectedShapes]);

  // v1.4.12 #4: グローバルなショートカット。?(Shift+/)/F1=ヘルプ開閉、
  // Ctrl+A=全選択、Ctrl+D=選択を仮身化、Ctrl+G=整列メニュー。
  // テキスト入力中・ダイアログ/メニュー表示中は（ヘルプ開閉を除き）奪わない。
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const ae = document.activeElement as HTMLElement | null;
      const typing =
        !!ae && (ae.tagName === "INPUT" || ae.tagName === "TEXTAREA" || ae.isContentEditable);
      // ヘルプは入力中以外ならいつでも開閉できる（F1 はブラウザヘルプを潰す）。
      if (e.key === "F1" || (e.key === "?" && !typing)) {
        e.preventDefault();
        setHelpOpen((v) => !v);
        return;
      }
      if (!(e.ctrlKey || e.metaKey) || e.shiftKey || e.altKey || typing) return;
      if (prompt || confirm || filterDlg || builtinDlg || shellDlg || shellOut || traceDlg || aiDlg || whisperDlg || menu || appPicker) return;
      const k = e.key.toLowerCase();
      if (k === "a") {
        e.preventDefault();
        setSelected(new Set(objectsRef.current.map((o) => o.id)));
        setSelectedNotes(new Set(notesRef.current.map((n) => n.id)));
      } else if (k === "d") {
        e.preventDefault();
        void handleDuplicateSelected();
      } else if (k === "g") {
        e.preventDefault();
        const cards = objectsRef.current.filter((o) => selectedRef.current.has(o.id));
        const ns = notesRef.current.filter((n) => selectedNotesRef.current.has(n.id));
        if (cards.length + ns.length < 2) return;
        const xs = [...cards.map((o) => o.pos_x), ...ns.map((n) => n.pos_x)];
        const ys = [...cards.map((o) => o.pos_y), ...ns.map((n) => n.pos_y)];
        const cx = xs.reduce((a, b) => a + b, 0) / xs.length;
        const cy = ys.reduce((a, b) => a + b, 0) / ys.length;
        const t = transformRef.current;
        setMenu({
          x: cx * t.scale + t.x,
          y: cy * t.scale + t.y,
          items: [
            { label: "横一列に整列（水平）", onClick: () => alignSelected("row") },
            { label: "縦一列に整列（垂直）", onClick: () => alignSelected("col") },
            { label: "格子状に整列", onClick: () => alignSelected("grid") },
            { label: "横方向に均等配置", onClick: () => alignSelected("dist-h") },
            { label: "縦方向に均等配置", onClick: () => alignSelected("dist-v") },
          ],
        });
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [
    prompt,
    confirm,
    filterDlg,
    builtinDlg,
    shellDlg,
    shellOut,
    traceDlg,
    aiDlg,
    whisperDlg,
    menu,
    appPicker,
    handleDuplicateSelected,
    alignSelected,
    setSelected,
    setSelectedNotes,
  ]);

  // Delete キー = 選択カード＋選択コメントを削除（確認・一括Undoは既存の削除フローに乗せる）。
  // テキスト入力中・ダイアログ/メニュー表示中は奪わない。v1.4.30: コメント（付箋）にも効く。
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Delete") return;
      if (prompt || confirm || filterDlg || builtinDlg || shellDlg || shellOut || traceDlg || aiDlg || whisperDlg || menu) return;
      const ae = document.activeElement as HTMLElement | null;
      if (ae && (ae.tagName === "INPUT" || ae.tagName === "TEXTAREA" || ae.isContentEditable))
        return;
      const ids = objectsRef.current.filter((o) => selectedRef.current.has(o.id)).map((o) => o.id);
      const noteIds = notesRef.current
        .filter((n) => selectedNotesRef.current.has(n.id))
        .map((n) => n.id);
      if (!ids.length && !noteIds.length) return;
      e.preventDefault();
      if (ids.length + noteIds.length >= 2) handleBulkDelete();
      else if (ids.length === 1) handleRequestDelete(ids[0]);
      else handleDeleteNote(noteIds[0]); // 単独コメント: メニューの削除と同じ（確認なし・Ctrl+Zで復活）
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [prompt, confirm, filterDlg, builtinDlg, shellDlg, shellOut, traceDlg, aiDlg, whisperDlg, menu, handleBulkDelete, handleRequestDelete, handleDeleteNote]);

  const handleChosen = useCallback((info: ProjectInfo) => {
    setShowChooser(false);
    setProject(info);
  }, []);

  // Show the chooser when no project is open, or when explicitly switching.
  const chooserVisible = projectLoaded && (!project || showChooser);

  // Stacking ranks: stable sort by z_order (objects arrive in created order).
  // Expanded cards float above everything (編集中・プレビュー中は前面).
  const zRank = new Map<string, number>();
  [...objects]
    .sort((a, b) => a.z_order - b.z_order)
    .forEach((o, i) => zRank.set(o.id, i + 1));
  const cardZ = (id: string) => (expanded.has(id) ? 1000 + (zRank.get(id) ?? 0) : zRank.get(id) ?? 0);

  // v1.4 オートフロー: 接続時型チェック（確定方針3）。フィルタへ入る flow 矢印の
  // 入力MIMEが accept_mime に合わない場合は赤矢印で警告（実行前に分かる）。
  // 上流がフィルタなら「そのフィルタの出力MIME」で照合（連鎖対応）。
  // 空のドロップポイントは保留（中身が入った時点で判定）。付箋は入力不可＝赤。
  const objById = new Map(objects.map((o) => [o.id, o]));
  const invalidFlowIds = new Set<number>();
  for (const r of relations) {
    if (r.kind !== "flow") continue;
    const filt = objById.get(r.to_id);
    if (!filt || (filt.kind !== "filter" && !filt.filter_ref)) continue;
    let accept = "*/*";
    try {
      accept = JSON.parse(filt.filter_ref ?? "{}").accept_mime ?? "*/*";
    } catch {
      /* 壊れた定義は全受け入れ扱い（実行時にエラーで分かる） */
    }
    const src = objById.get(r.from_id);
    if (!src) {
      invalidFlowIds.add(r.id); // 付箋など、フロー入力にできないノード
      continue;
    }
    if (src.kind === "filter" || src.filter_ref) {
      const outMime = filterOutputMime(src.filter_ref);
      if (outMime && !mimeMatches(accept, outMime)) invalidFlowIds.add(r.id);
      continue; // 出力MIME不明（外部コマンド）はチェックしない
    }
    if (src.kind === "drop" && src.real_id.startsWith("drop:")) continue; // 空の入口
    // v1.7 P2: URLカードは url_ref 文字列を text/plain で供給する（実行契約。
    // バックエンド preflight.rs の型特例と同じ規則を保つこと）。
    if (src.url_ref) {
      if (!mimeMatches(accept, "text/plain")) invalidFlowIds.add(r.id);
      continue;
    }
    if (!mimeMatches(accept, src.mime_type)) invalidFlowIds.add(r.id);
  }

  return (
    <>
      <Canvas
        onDropFiles={handleDropFiles}
        onDropText={handleDropText}
        onContextMenu={handleContextMenu}
        onTransformChange={handleTransformChange}
        initialTransform={view?.transform}
        viewKey={view?.key}
        onSelectRect={handleSelectRect}
        onEmptyClick={handleClearSelection}
        getContentBounds={getContentBounds}
        getSelectionBounds={getSelectionBounds}
        apiRef={canvasApi}
      >
        {/* v1.3-②③: shapes render behind links and cards (background groups) */}
        {[...shapes]
          .sort((a, b) => a.z_order - b.z_order)
          .map((s) => (
            <ShapeCard
              key={s.id}
              shape={s}
              selected={selectedShapes.has(s.id)}
              onGeomEnd={handleShapeGeomEnd}
              onClickSelect={handleShapeClickSelect}
              onToggleSelect={handleShapeToggleSelect}
              onDragCards={handleShapeDragCards}
              snapGrid={shapeSnap ? SHAPE_GRID : undefined}
            />
          ))}
        <LinkLayer
          nodes={[
            ...objects.map((o) => ({ id: o.id, x: o.pos_x, y: o.pos_y, w: CARD_W, h: CARD_H })),
            ...notes.map((n) => {
              const s = measuredNoteSize(n);
              return { id: n.id, x: n.pos_x, y: n.pos_y, w: s.w, h: s.h };
            }),
          ]}
          relations={relations}
          tempLine={linkDrag}
          onDeleteRelation={handleDeleteRelation}
          onFlowRelationClick={handleFlowRelationClick}
          invalidIds={invalidFlowIds}
          showReq={showReqArrows}
        />
        {objects.map((obj) => (
          <ObjectCard
            key={obj.id}
            id={obj.id}
            realId={obj.real_id}
            label={obj.label}
            mimeType={obj.mime_type}
            showHints={showCardHints}
            x={obj.pos_x}
            y={obj.pos_y}
            committed={!!obj.committed_path}
            active={touched.has(obj.id)}
            lost={lostOf(obj) ?? ((lostRealOf(obj) || lostFolderOf(obj) || lostProjectOf(obj)) ? "real" : null)}
            isProject={obj.mime_type === PROJECT_MIME || !!obj.project_ref}
            isUrl={!!obj.url_ref}
            webUpdate={webUpdateOf(obj)}
            runAlert={runAlertOf(obj)}
            onShowTrace={handleShowTrace}
            isFolder={!!obj.folder_ref}
            isFilter={obj.kind !== "launcher" && (obj.kind === "filter" || !!obj.filter_ref)}
            filterRef={obj.filter_ref}
            isDrop={obj.kind === "drop"}
            isLauncher={obj.kind === "launcher"}
            launcherPath={launcherPathOf(obj)}
            memo={obj.memo}
            ext={obj.ext}
            expanded={expanded.has(obj.id)}
            editPath={obj.committed_path}
            panelW={obj.panel_w}
            panelH={obj.panel_h}
            onPanelResize={handlePanelResize}
            zIndex={cardZ(obj.id)}
            selected={selected.has(obj.id)}
            onToggleSelect={handleCardToggleSelect}
            onCtrlClone={handleCtrlClone}
            onSelectConnected={handleSelectConnected}
            onSelect={handleCardSelectOnly}
            onGroupDragStart={handleGroupDragStart}
            onMoveEnd={handleMoveEnd}
            onDragLive={handleDragLive}
            onDragOut={handleDragOut}
            onStartLink={handleStartLink}
            onOpen={handleOpenObject}
            onRenameTitle={handleRenameTitle}
            onCollapse={handleCollapse}
            onAfterSave={refresh}
            onRequestEdit={handleEditInCanvas}
            onRunFlow={obj.kind === "launcher" ? handleRunLauncher : handleRunFlow}
            onToggleOpPause={handleToggleOpPause}
          />
        ))}
        {notes.map((n) => (
          <NoteCard
            key={n.id}
            id={n.id}
            text={n.text}
            x={n.pos_x}
            y={n.pos_y}
            width={n.width}
            fontSize={n.font_size}
            bold={n.bold}
            color={n.color}
            titleSize={n.title_size}
            fileRef={n.file_ref}
            startEditing={n.id === newNoteId}
            zIndex={objects.length + 10}
            selected={selectedNotes.has(n.id)}
            onToggleSelect={handleNoteToggleSelect}
            onSelect={handleNoteSelectOnly}
            onSelectConnected={handleSelectConnected}
            onGroupDragStart={handleGroupDragStart}
            onStartLink={handleStartLink}
            onMeasure={handleNoteMeasure}
            onMoveEnd={handleNoteMove}
            onDragLive={handleDragLive}
            onSave={handleNoteSave}
            onResize={handleNoteResize}
          />
        ))}
      </Canvas>

      {project && (
        <div
          onClick={() => setShowChooser(true)}
          onContextMenu={(e) => {
            e.preventDefault();
            e.stopPropagation();
            setMenu({
              x: e.clientX,
              y: e.clientY,
              items: [
                { label: `「${project.name}」`, onClick: () => {}, disabled: true },
                {
                  label: "このプロジェクトをフォルダで開く",
                  onClick: () =>
                    invoke("open_with_default", { path: project.path }).catch(() => {}),
                },
                {
                  label: "プロジェクトを切り替え...",
                  onClick: () => setShowChooser(true),
                },
              ],
            });
          }}
          title="クリックでプロジェクトを切り替え／右クリックでメニュー"
          style={{
            position: "fixed",
            top: 8,
            left: 12,
            color: "var(--chrome-text, #cdd6e6cc)",
            fontSize: 12,
            fontFamily: "sans-serif",
            background: "var(--chrome-bg, #1c2130cc)",
            border: "1px solid var(--chrome-border, #2a3346)",
            borderRadius: 6,
            padding: "4px 10px",
            cursor: "pointer",
          }}
        >
          📁 {project.name}
        </div>
      )}

      {/* v1.3 メモ①: バージョン表示（左下・ズーム表示と同じトーン） */}
      {appVersion && (
        <div
          style={{
            position: "fixed",
            bottom: 12,
            left: 16,
            color: "var(--chrome-text, #ffffff44)",
            opacity: 0.75,
            fontSize: 11,
            fontFamily: "monospace",
            userSelect: "none",
            pointerEvents: "none",
          }}
        >
          TexoS v{appVersion}
        </div>
      )}

      {/* v1.4.12: ショートカット・ヘルプを開く（? / F1 でも開閉） */}
      <div
        onClick={() => setHelpOpen(true)}
        title="キーボードショートカット（? または F1）"
        style={{
          position: "fixed",
          top: 8,
          right: 64,
          width: 24,
          height: 24,
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          color: "var(--chrome-text, #cdd6e6cc)",
          fontSize: 13,
          fontFamily: "sans-serif",
          background: "var(--chrome-bg, #1c2130cc)",
          border: "1px solid var(--chrome-border, #2a3346)",
          borderRadius: 6,
          cursor: "pointer",
        }}
      >
        ?
      </div>

      {/* 終了ボタン（自動保存済みのまま閉じる） */}
      <div
        onClick={() => getCurrentWindow().close()}
        title="TexoSを終了（変更は自動保存されています）"
        style={{
          position: "fixed",
          top: 8,
          right: 12,
          color: "var(--chrome-text, #cdd6e6cc)",
          fontSize: 12,
          fontFamily: "sans-serif",
          background: "var(--chrome-bg, #1c2130cc)",
          border: "1px solid var(--chrome-border, #2a3346)",
          borderRadius: 6,
          padding: "4px 12px",
          cursor: "pointer",
        }}
      >
        終了
      </div>

      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          items={menu.items}
          showHints={showMenuHints}
          onClose={() => setMenu(null)}
        />
      )}
      {filterDlg && (
        <FilterDialog
          initialLabel={filterDlg.edit?.label}
          initialSpec={filterDlg.edit?.spec}
          // v1.8.3: 出力先フォルダの参照もプロジェクト相対で保存する（可搬性）。
          projectRoot={projectPathRef.current}
          onCancel={() => setFilterDlg(null)}
          onSave={async (label, spec) => {
            const dlg = filterDlg;
            setFilterDlg(null);
            try {
              if (dlg.edit) {
                await invoke("update_filter", { id: dlg.edit.id, spec });
                if (label !== dlg.edit.label) {
                  await invoke("rename_object", { id: dlg.edit.id, label });
                }
              } else {
                const vid = await invoke<string>("create_filter", {
                  label,
                  spec,
                  x: dlg.x,
                  y: dlg.y,
                });
                pushUndo([{ kind: "delete", ids: [vid] }]);
              }
              await refresh();
            } catch (e) {
              console.error("filter save failed", e);
              setConfirm({
                title: "フィルタを保存できませんでした",
                message: String(e),
                okLabel: "OK",
                onConfirm: () => setConfirm(null),
              });
            }
          }}
        />
      )}
      {builtinDlg && (
        <BuiltinFilterDialog
          builtin={builtinDlg.builtin}
          initialLabel={builtinDlg.label}
          initialSpec={builtinDlg.spec}
          // v1.8.3: フォルダ欄の「参照…」でプロジェクト配下を選んだら相対パスに畳む。
          projectRoot={projectPathRef.current}
          onCancel={() => setBuiltinDlg(null)}
          onSave={async (label, spec) => {
            const dlg = builtinDlg;
            setBuiltinDlg(null);
            try {
              if (dlg.id) {
                await invoke("update_filter", { id: dlg.id, spec });
                if (label !== dlg.label) {
                  await invoke("rename_object", { id: dlg.id, label });
                }
              } else if (dlg.create) {
                // v1.5.4 WS-P2: op カード（タイマ/ウォッチャ）の新規作成もこのダイアログ経由。
                // spec の op キーで backend が kind='op' として作る。
                const vid = await invoke<string>("create_filter", {
                  label,
                  spec,
                  x: dlg.create.x,
                  y: dlg.create.y,
                });
                pushUndo([{ kind: "delete", ids: [vid] }]);
                if (dlg.create.linkFrom) await linkNewNode(dlg.create.linkFrom, vid);
              }
              await refresh();
            } catch (e) {
              console.error("builtin filter save failed", e);
              setConfirm({
                title: "設定を保存できませんでした",
                message: String(e),
                okLabel: "OK",
                onConfirm: () => setConfirm(null),
              });
            }
          }}
        />
      )}
      {shellDlg && (
        <ShellSettingsDialog
          initialLabel={shellDlg.label}
          initialSpec={shellDlg.spec}
          onCancel={() => setShellDlg(null)}
          onSave={async (label, spec) => {
            const dlg = shellDlg;
            setShellDlg(null);
            try {
              if (dlg.id) {
                await invoke("update_filter", { id: dlg.id, spec });
                if (label !== dlg.label) {
                  await invoke("rename_object", { id: dlg.id, label });
                }
              } else if (dlg.create) {
                const vid = await invoke<string>("create_filter", {
                  label,
                  spec,
                  x: dlg.create.x,
                  y: dlg.create.y,
                });
                pushUndo([{ kind: "delete", ids: [vid] }]);
                if (dlg.create.linkFrom) await linkNewNode(dlg.create.linkFrom, vid);
              }
              await refresh();
            } catch (e) {
              console.error("shell settings save failed", e);
              setConfirm({
                title: "シェルカードを保存できませんでした",
                message: String(e),
                okLabel: "OK",
                onConfirm: () => setConfirm(null),
              });
            }
          }}
        />
      )}
      {shellOut && (
        <ShellOutputDialog
          label={shellOut.label}
          exitCode={shellOut.exitCode}
          output={shellOut.output}
          truncated={shellOut.truncated}
          onClose={() => setShellOut(null)}
        />
      )}
      {traceDlg && (
        <TraceDialog
          label={traceDlg.label}
          trace={traceDlg.trace}
          debugMode={debugTrace}
          onClose={() => setTraceDlg(null)}
        />
      )}
      {aiDlg && (
        <AiSettingsDialog
          initialLabel={aiDlg.label}
          initialSpec={aiDlg.spec}
          onCancel={() => setAiDlg(null)}
          onSave={async (label, spec) => {
            const dlg = aiDlg;
            setAiDlg(null);
            try {
              if (dlg.id) {
                await invoke("update_filter", { id: dlg.id, spec });
                if (label !== dlg.label) {
                  await invoke("rename_object", { id: dlg.id, label });
                }
              } else if (dlg.create) {
                const vid = await invoke<string>("create_filter", {
                  label,
                  spec,
                  x: dlg.create.x,
                  y: dlg.create.y,
                });
                pushUndo([{ kind: "delete", ids: [vid] }]);
                if (dlg.create.linkFrom) await linkNewNode(dlg.create.linkFrom, vid);
              }
              await refresh();
            } catch (e) {
              console.error("ai settings save failed", e);
              setConfirm({
                title: "AI設定を保存できませんでした",
                message: String(e),
                okLabel: "OK",
                onConfirm: () => setConfirm(null),
              });
            }
          }}
        />
      )}
      {appPicker && (
        <AppPicker
          onPick={handleCreateLauncherFromApp}
          onCancel={() => setAppPicker(null)}
        />
      )}
      {helpOpen && <ShortcutHelp onClose={() => setHelpOpen(false)} />}
      {whisperDlg && (
        <WhisperSetupDialog
          onCancel={() => setWhisperDlg(null)}
          onCreate={async (st) => {
            const dlg = whisperDlg;
            setWhisperDlg(null);
            try {
              const vid = await invoke<string>("create_filter", {
                label: "Whisper 文字起こし",
                spec: JSON.stringify(whisperSpec(st)),
                x: dlg.x,
                y: dlg.y,
              });
              pushUndo([{ kind: "delete", ids: [vid] }]);
              if (dlg.linkFrom) await linkNewNode(dlg.linkFrom, vid);
              await refresh();
            } catch (e) {
              console.error("create whisper card failed", e);
              setConfirm({
                title: "Whisperカードを作成できませんでした",
                message: String(e),
                okLabel: "OK",
                onConfirm: () => setConfirm(null),
              });
            }
          }}
        />
      )}
      {searxngSetup && (
        <SearxngSetupDialog
          onCancel={() => setSearxngSetup(null)}
          onReady={
            searxngSetup.manage
              ? undefined
              : () => {
                  const d = searxngSetup;
                  setSearxngSetup(null);
                  handleAddFilter("web_search", d.x, d.y, d.linkFrom);
                }
          }
        />
      )}
      {t2iSetup && (
        <T2iSetupDialog
          onCancel={() => setT2iSetup(null)}
          onReady={(s) => {
            const dlg = t2iSetup;
            setT2iSetup(null);
            // 準備完了 → 設定ダイアログでパラメータを決めてカード作成。
            setT2iDlg({
              id: null,
              label: "画像生成",
              spec: JSON.stringify({
                builtin: "t2i",
                model: s.current_model || s.models[0],
                steps: 25,
                guidance: 7.5,
                width: 512,
                height: 512,
                accept_mime: "*/*",
                output_ext: "png",
              }),
              models: s.models,
              known: s.known.map((k) => k.name),
              assets: { loras: s.loras, embeddings: s.embeddings, vae: s.vae },
              create: { x: dlg.x, y: dlg.y, linkFrom: dlg.linkFrom },
            });
          }}
        />
      )}
      {t2iDlg && (
        <T2iSettingsDialog
          initialLabel={t2iDlg.label}
          initialSpec={t2iDlg.spec}
          models={t2iDlg.models}
          known={t2iDlg.known}
          assets={t2iDlg.assets}
          onCancel={() => setT2iDlg(null)}
          onOpenSetup={() => {
            const d = t2iDlg;
            setT2iDlg(null);
            setT2iSetup({ x: d.create?.x ?? 100, y: d.create?.y ?? 100, linkFrom: d.create?.linkFrom });
          }}
          onSave={async (label, spec) => {
            const dlg = t2iDlg;
            setT2iDlg(null);
            try {
              if (dlg.id) {
                await invoke("update_filter", { id: dlg.id, spec });
                if (label !== dlg.label) await invoke("rename_object", { id: dlg.id, label });
              } else if (dlg.create) {
                const vid = await invoke<string>("create_filter", {
                  label,
                  spec,
                  x: dlg.create.x,
                  y: dlg.create.y,
                });
                pushUndo([{ kind: "delete", ids: [vid] }]);
                if (dlg.create.linkFrom) await linkNewNode(dlg.create.linkFrom, vid);
              }
              await refresh();
            } catch (e) {
              console.error("t2i settings save failed", e);
              setConfirm({
                title: "画像生成カードを保存できませんでした",
                message: String(e),
                okLabel: "OK",
                onConfirm: () => setConfirm(null),
              });
            }
          }}
        />
      )}
      {/* v1.5.3 WS-P1: 入力の順序ダイアログ（{inN}・セレクタの採用順を並べ替え）。 */}
      {inputOrderDlg && (
        <InputOrderDialog
          cardId={inputOrderDlg.id}
          cardLabel={inputOrderDlg.label}
          onSaved={() => {
            setInputOrderDlg(null);
            void invoke<RelationInfo[]>("list_relations").then(setRelations).catch(() => {});
          }}
          onCancel={() => setInputOrderDlg(null)}
        />
      )}
      {/* v1.5.2 WS-C 背景ジョブパネル: 実行中ジョブの一覧＋個別停止/中断＋Stop-all。
          プロジェクト切替後も残る（最後の物理停止＝自律安全 §6.3）。 */}
      <JobsPanel jobs={jobs} />
      {prompt && (
        <PromptDialog
          title={prompt.title}
          initial={prompt.initial}
          allowEmpty={prompt.allowEmpty}
          label={prompt.label}
          field2={prompt.field2}
          multiline={prompt.multiline}
          onConfirm={(v, v2) => {
            // Close first, THEN run the callback — so a callback that opens
            // another prompt (e.g. URL → title) isn't immediately cleared.
            const cb = prompt.onConfirm;
            setPrompt(null);
            cb(v, v2);
          }}
          onCancel={() => setPrompt(null)}
        />
      )}
      {confirm && (
        <ConfirmDialog
          title={confirm.title}
          message={confirm.message}
          warning={confirm.warning}
          okLabel={confirm.okLabel}
          cancelLabel={confirm.cancelLabel}
          onConfirm={confirm.onConfirm}
          onCancel={() => setConfirm(null)}
        />
      )}
      {trashOpen && (
        <TrashDialog
          onClose={() => setTrashOpen(false)}
          onChanged={refresh}
          onRestored={handleTrashRestored}
        />
      )}
      {searchOpen && (
        <SearchDialog
          objects={objects}
          notes={notes}
          onFocus={focusCards}
          onClose={() => setSearchOpen(false)}
        />
      )}
      {chooserVisible && (
        <ProjectChooser
          onChosen={handleChosen}
          onCancel={project ? () => setShowChooser(false) : undefined}
        />
      )}
    </>
  );
}

export default App;
