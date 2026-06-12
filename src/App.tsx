import { useEffect, useState, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { getVersion } from "@tauri-apps/api/app";
import { Canvas, screenToCanvas, type Transform, type CanvasApi } from "./canvas/Canvas";
import { LinkLayer, type RelationInfo } from "./canvas/LinkLayer";
import { ObjectCard, CARD_W, CARD_H } from "./objects/ObjectCard";
import { NoteCard } from "./objects/NoteCard";
import { ShapeCard, type ShapeInfo } from "./objects/ShapeCard";
import { ContextMenu, type MenuItem } from "./ui/ContextMenu";
import { PromptDialog } from "./ui/PromptDialog";
import { ConfirmDialog } from "./ui/ConfirmDialog";
import { ProjectChooser, type ProjectInfo } from "./ui/ProjectChooser";
import {
  PROJECT_MIME,
  filterOutputMime,
  isTextMime,
  lostOf,
  mimeMatches,
  noteSize,
  shapeCenterOn,
  type NoteInfo,
  type VirtualBodyInfo,
} from "./model";
import { useUndo } from "./hooks/useUndo";
import { useSelection } from "./hooks/useSelection";
import {
  buildAlignItems,
  buildBulkItems,
  buildCanvasMenu,
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

function App() {
  const [project, setProject] = useState<ProjectInfo | null>(null);
  const [projectLoaded, setProjectLoaded] = useState(false);
  const [showChooser, setShowChooser] = useState(false);
  const [objects, setObjects] = useState<VirtualBodyInfo[]>([]);
  const [notes, setNotes] = useState<NoteInfo[]>([]);
  const [shapes, setShapes] = useState<ShapeInfo[]>([]);
  // v1.3 メモ①: カンバス左下に表示するアプリバージョン。
  const [appVersion, setAppVersion] = useState("");
  const [theme, setTheme] = useState<string>(() => localStorage.getItem("texos-theme") ?? "midnight");
  const [newNoteId, setNewNoteId] = useState<string | null>(null);
  const [relations, setRelations] = useState<RelationInfo[]>([]);
  const [linkDrag, setLinkDrag] = useState<LinkDrag | null>(null);
  const [menu, setMenu] = useState<{ x: number; y: number; items: MenuItem[] } | null>(null);
  const [prompt, setPrompt] = useState<{
    title: string;
    initial: string;
    onConfirm: (v: string) => void;
  } | null>(null);
  const [confirm, setConfirm] = useState<{
    title: string;
    message?: string;
    warning?: string;
    okLabel?: string;
    onConfirm: () => void;
  } | null>(null);
  // Objects newly committed / changed during this session (緑シグナル).
  // 閲覧のみでは入れない — 仮身は琥珀のまま、実身はグレーのまま (v1.2 仕様).
  const [touched, setTouched] = useState<Set<string>>(new Set());
  // Objects whose inline text editor is currently expanded.
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  // Saved canvas view (pan/zoom) for the current project, applied atomically.
  const [view, setView] = useState<{ key: string; transform: Transform } | null>(null);
  const saveTimer = useRef<number | null>(null);

  const transformRef = useRef<Transform>({ x: 0, y: 0, scale: 1 });
  const objectsRef = useRef<VirtualBodyInfo[]>([]);
  objectsRef.current = objects;
  const shapesRef = useRef<ShapeInfo[]>([]);
  shapesRef.current = shapes;
  const notesRef = useRef<NoteInfo[]>([]);
  notesRef.current = notes;
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
  } = useSelection({ objectsRef, notesRef, measuredNoteSize });

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

  // v1.3 メモ② UNDO/REDO — useUndo に分離（v1.3.2 リファクタ）。
  // Ctrl+Z / Ctrl+Shift+Z / Ctrl+Y のキーバインドもフック内。
  const {
    pushUndo,
    applyObjectMoves,
    applyNoteMoves,
    applyShapeGeoms,
    resetStacks,
    loadStacks,
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

  // v1.3-①: apply / persist the UI color preset.
  useEffect(() => {
    if (theme === "midnight") delete document.documentElement.dataset.texosTheme;
    else document.documentElement.dataset.texosTheme = theme;
    localStorage.setItem("texos-theme", theme);
  }, [theme]);

  // Re-check edit state when returning to the window (e.g. after editing externally)
  useEffect(() => {
    const onFocus = () => {
      if (project) refresh();
    };
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [project, refresh]);

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

  const handleContextPlace = useCallback(
    async (canvasX: number, canvasY: number) => {
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
    },
    [refresh, pushUndo]
  );

  const handleMoveEnd = useCallback(async (id: string, x: number, y: number) => {
    const prev = objectsRef.current.find((o) => o.id === id);
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
  }, [pushUndo]);

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

  // --- v1.4 オートフロー⑤ ----------------------------------------------------

  // フィルタカードを作成。引数はプロンプトの連鎖で聞く（フォーム自動生成はフェーズ2）。
  const handleAddFilter = useCallback(
    (builtin: string, canvasX: number, canvasY: number) => {
      // ドロップポイント＝パイプラインのデータ入口カード（投入で下流が自動実行）
      if (builtin === "drop_point") {
        (async () => {
          try {
            const vid = await invoke<string>("create_drop_point", { x: canvasX, y: canvasY });
            pushUndo([{ kind: "delete", ids: [vid] }]);
            await refresh();
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
      if (builtin === "text_grep") {
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
      } else {
        // 外部プログラム: exe をファイル選択してコマンドラインを自動構成する
        // （プレースホルダの手書きは現実的でない — v1.4 実機指摘）。
        (async () => {
          const exe = await open({
            multiple: false,
            title:
              builtin === "external_stdio"
                ? "プログラムを選択（標準入出力フィルタ型: prog < 入力 > 出力）"
                : "プログラムを選択（引数型: prog 入力 出力）",
            filters: [{ name: "プログラム", extensions: ["exe", "bat", "cmd", "com"] }],
          });
          if (!exe || Array.isArray(exe)) return;
          const name = exe.split(/[\\/]/).pop() ?? "program";
          const initialCmd =
            builtin === "external_stdio"
              ? `"${exe}" < "{in}" > "{out}"`
              : `"${exe}" "{in}" "{out}"`;
          setPrompt({
            title: "コマンドライン（必要なら引数を編集）",
            initial: initialCmd,
            onConfirm: (cmd) =>
              setPrompt({
                title: "出力ファイルの拡張子（空欄=入力と同じ）",
                initial: "txt",
                onConfirm: (ext) => {
                  const spec: Record<string, unknown> = { cmd, accept_mime: "*/*" };
                  if (ext.trim()) spec.output_ext = ext.trim().replace(/^\./, "");
                  create(name, spec);
                },
              }),
          });
        })();
      }
    },
    [refresh, pushUndo]
  );

  // フローを実行（手動トリガー — 確定方針4）。結果カードは緑シグナル＋Ctrl+Zで戻せる。
  const handleRunFlow = useCallback(
    async (id: string) => {
      try {
        const res = await invoke<{
          created: string[];
          outputs: string[];
          skipped: string[];
          errors: string[];
        }>("run_flow", { id });
        if (res.created.length) {
          setTouched((prev) => new Set([...prev, ...res.created]));
          pushUndo([{ kind: "delete", ids: res.created }]);
        }
        await refresh();
        const lines = [
          ...res.outputs.map((o) => `生成: ${o}`),
          ...res.skipped.map((s) => `スキップ: ${s}`),
        ];
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
      }
    },
    [refresh, pushUndo]
  );

  // フィルタ定義（JSON）の直接編集。バックエンドが保存前に検証する。
  const handleEditFilter = useCallback(
    (id: string, current: string) => {
      setPrompt({
        title: "フィルタ定義（JSON）",
        initial: current,
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

  const handleDeleteObject = useCallback(async (id: string) => {
    try {
      await invoke("delete_object", { id });
      // 復活の呪文: 削除はソフトデリートなのでCtrl+Zで矢印リンクごと復活する。
      pushUndo([{ kind: "restore", ids: [id] }]);
      setObjects((prev) => prev.filter((o) => o.id !== id));
      setRelations((prev) => prev.filter((r) => r.from_id !== id && r.to_id !== id));
    } catch (e) {
      console.error("delete_object failed", e);
    }
  }, [pushUndo]);

  // Confirm before deleting a committed object (its real file is removed too).
  const handleRequestDelete = useCallback(
    (id: string) => {
      const obj = objectsRef.current.find((o) => o.id === id);
      if (obj?.committed_path) {
        setConfirm({
          title: "削除しますか？",
          message:
            "このオブジェクトと、プロジェクトフォルダ内のコミット済み実ファイルを削除します。\n（CAS内の元データは保持されます）",
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
  const handleOpenObject = useCallback(
    async (id: string) => {
      const obj = objectsRef.current.find((o) => o.id === id);
      // v1.4: フィルタカードのダブルクリック＝そのカードの自然なアクション＝フロー実行。
      if (obj && (obj.kind === "filter" || obj.filter_ref)) {
        handleRunFlow(id);
        return;
      }
      // v1.4: ドロップポイントのダブルクリック＝ファイルを選んで投入（→自動実行）。
      if (obj?.kind === "drop") {
        handleFeedDrop(id);
        return;
      }
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
      // Double-click does the natural in-canvas action per type.
      if (obj && (obj.mime_type.startsWith("image/") || obj.mime_type.startsWith("audio/"))) {
        handleExpandView(id);
        return;
      }
      if (obj && isTextMime(obj.mime_type, obj.ext)) {
        handleEditInCanvas(id);
        return;
      }
      try {
        const wasCommitted = !!obj?.committed_path;
        const path = await invoke<string>("commit_object", { id });
        await refresh();
        if (!wasCommitted) setTouched((prev) => new Set(prev).add(id));
        await invoke("open_with_default", { path });
      } catch (e) {
        console.error("commit/open failed", e);
        setConfirm({
          title: "開けませんでした",
          message: String(e),
          okLabel: "OK",
          onConfirm: () => setConfirm(null),
        });
      }
    },
    [refresh, handleRunFlow, handleFeedDrop]
  );

  // Place a reference to an existing project as a project-object card.
  const handleLinkProject = useCallback(
    async (canvasX: number, canvasY: number) => {
      const dir = await open({ directory: true, title: "リンクするTexoSプロジェクトを選択" });
      if (!dir || Array.isArray(dir)) return;
      try {
        const vid = await invoke<string>("link_project", { path: dir, x: canvasX, y: canvasY });
        pushUndo([{ kind: "delete", ids: [vid] }]);
        await refresh();
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
    [refresh, pushUndo]
  );

  // Add a URL link card (prompt URL, then title).
  const handleAddUrl = useCallback(
    (canvasX: number, canvasY: number) => {
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
              } catch (e) {
                console.error("create_url failed", e);
              }
            },
          });
        },
      });
    },
    [refresh, pushUndo]
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
        title: "編集を破棄して再コミット",
        message:
          "プロジェクトフォルダの編集済みファイルを、元の実身（不変）で上書きします。編集内容は失われます。よろしいですか？",
        okLabel: "再コミット",
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
        title: "全コミット（データ持ち出し）",
        message:
          `${plan.committed.length} 件を実ファイルとしてコミットし、` +
          `プロジェクトフォルダを単体で持ち出せる状態にします。\n` +
          `（コミット済み: ${plan.already} 件）`,
        warning,
        okLabel: "全コミット実行",
        onConfirm: async () => {
          setConfirm(null);
          try {
            const res = await invoke<CommitAllReport>("commit_all", { dryRun: false });
            await refresh();
            const errText = res.errors.length
              ? `\n\n失敗 ${res.errors.length} 件:\n` + res.errors.join("\n")
              : "";
            setConfirm({
              title: "全コミット完了",
              message: `${res.committed.length} 件をコミットしました（既存 ${res.already} 件）。${errText}`,
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
              title: "全コミットに失敗しました",
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
          title: "コミットできませんでした",
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
  const handlePanelResize = useCallback(async (id: string, width: number) => {
    try {
      await invoke("set_panel_width", { id, width });
      setObjects((prev) => prev.map((o) => (o.id === id ? { ...o, panel_w: width } : o)));
    } catch (e) {
      console.error("set_panel_width failed", e);
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

  // コミットを取り消す（仮身に戻す）= Ctrl+Z for the 仮身→実身 conversion.
  const handleUncommit = useCallback(
    (id: string) => {
      setConfirm({
        title: "コミットを取り消す（仮身に戻す）",
        message:
          "実ファイルを削除し、参照（仮身）に戻します。この実ファイルへの編集内容は失われます。よろしいですか？",
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
  const handleBulkCommit = useCallback(async () => {
    const targets = objectsRef.current.filter(
      (o) =>
        selectedRef.current.has(o.id) &&
        !o.committed_path && !o.project_ref && !o.url_ref && !o.folder_ref
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
        title: "一部のカードをコミットできませんでした",
        message: `${done.length} 件をコミットしました。\n\n失敗 ${errors.length} 件:\n` + errors.join("\n"),
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
        !(o.origin_path && o.origin_path === o.committed_path)
    );
    if (!targets.length) return;
    setConfirm({
      title: "選択カードをまとめて仮身に戻す",
      message:
        `${targets.length} 件の実ファイルを削除し、参照（仮身）に戻します。\n` +
        "実ファイルへの編集内容は失われます。よろしいですか？",
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
  const handleBulkDelete = useCallback(() => {
    const targets = objectsRef.current.filter((o) => selectedRef.current.has(o.id));
    if (!targets.length) return;
    const committedCount = targets.filter((o) => o.committed_path).length;
    setConfirm({
      title: "選択カードをまとめて削除",
      message:
        `${targets.length} 件のカードを削除します。` +
        (committedCount
          ? `\nうち ${committedCount} 件はコミット済みで、プロジェクトフォルダ内の実ファイルも削除されます。\n（CAS内の元データは保持されます）`
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
        // 復活の呪文: まとめて削除も1回のCtrl+Zで全員戻ってくる。
        if (ids.length) {
          pushUndo([{ kind: "restore", ids }]);
        }
        setSelected(new Set());
        await refresh();
      },
    });
  }, [refresh, pushUndo]);

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
  const handleGroupDragStart = useCallback((_id: string, clientX: number, clientY: number) => {
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
    };
    const onUp = async () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
      if (dx === 0 && dy === 0) return;
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
      } catch (e) {
        console.error("group move failed", e);
      }
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  }, [pushUndo, applyObjectMoves, applyNoteMoves]);

  // --- Clipboard paste (v1.3-④) ---------------------------------------------

  // 画像はバックエンドで新オブジェクト化。テキストはURLならURLカード、
  // それ以外はコメント（付箋）として作成。
  const handlePaste = useCallback(
    async (canvasX: number, canvasY: number) => {
      try {
        const res = await invoke<{ kind: string; text: string | null; id: string | null }>(
          "paste_clipboard",
          { x: canvasX, y: canvasY }
        );
        if (res.kind === "image") {
          if (res.id) pushUndo([{ kind: "delete", ids: [res.id] }]);
          await refresh();
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
        }
      } catch (e) {
        console.error("paste_clipboard failed", e);
      }
    },
    [refresh, pushUndo]
  );

  // Ctrl+V → ビューポート中央に貼り付け（テキスト入力中は奪わない）。
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey) || e.key.toLowerCase() !== "v") return;
      const ae = document.activeElement as HTMLElement | null;
      if (ae && (ae.tagName === "INPUT" || ae.tagName === "TEXTAREA" || ae.isContentEditable)) return;
      const c = screenToCanvas(transformRef.current, window.innerWidth / 2, window.innerHeight / 2);
      handlePaste(c.x - CARD_W / 2, c.y - CARD_H / 2);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [handlePaste]);

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

  const handleShapeAdd = useCallback(
    async (kind: string, canvasX: number, canvasY: number) => {
      const size =
        kind === "arrow" ? { width: 280, height: 110 } : { width: 360, height: 250 };
      try {
        const vid = await invoke<string>("create_shape", { kind, x: canvasX, y: canvasY, ...size });
        pushUndo([{ kind: "deleteShapes", ids: [vid] }]);
        await refresh();
      } catch (e) {
        console.error("create_shape failed", e);
      }
    },
    [refresh, pushUndo]
  );

  const handleShapeGeomEnd = useCallback(
    async (id: string, x: number, y: number, w: number, h: number) => {
      const s = shapesRef.current.find((sh) => sh.id === id);
      if (!s) return;
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
    [pushUndo, applyShapeGeoms]
  );

  // クリック（ドラッグなし）= 図形上のカード・コメントをまとめて選択（思考のグループ）。
  const handleShapeClickSelect = useCallback(
    (id: string) => {
      const s = shapesRef.current.find((sh) => sh.id === id);
      if (!s) return;
      setSelectedShapes(new Set([id]));
      setSelected(new Set(cardsOnShape(s).map((c) => c.id)));
      setSelectedNotes(new Set(notesOnShape(s).map((n) => n.id)));
    },
    [cardsOnShape, notesOnShape]
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
        const cardStarts = new Map<string, { x: number; y: number }>();
        const noteStarts = new Map<string, { x: number; y: number }>();
        for (const sh of moving.values()) {
          cardsOnShape(sh).forEach((c) => cardStarts.set(c.id, { x: c.pos_x, y: c.pos_y }));
          notesOnShape(sh).forEach((n) => noteStarts.set(n.id, { x: n.pos_x, y: n.pos_y }));
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
    [cardsOnShape, notesOnShape, pushUndo, applyObjectMoves, applyNoteMoves, applyShapeGeoms]
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

  // v1.3 メモ⑥: ハンドルがカードの下に隠れて掴めない問題への対策。
  // メニューから一定倍率で拡縮（中心を保つ）するか、上のカード全体に合わせる。
  const handleShapeScale = useCallback(async (id: string, factor: number) => {
    const s = shapesRef.current.find((sh) => sh.id === id);
    if (!s) return;
    const w = Math.max(60, Math.round(s.width * factor));
    const h = Math.max(40, Math.round(s.height * factor));
    const x = Math.round(s.pos_x + (s.width - w) / 2);
    const y = Math.round(s.pos_y + (s.height - h) / 2);
    pushUndo([
      { kind: "shapes", geoms: [{ id, x: s.pos_x, y: s.pos_y, w: s.width, h: s.height, rot: s.rotation }] },
    ]);
    try {
      await applyShapeGeoms([{ id, x, y, w, h, rot: s.rotation }]);
    } catch (e) {
      console.error("scale shape failed", e);
    }
  }, [pushUndo, applyShapeGeoms]);

  const handleShapeFitToCards = useCallback(async (id: string) => {
    const s = shapesRef.current.find((sh) => sh.id === id);
    if (!s) return;
    const cards = cardsOnShape(s);
    const ns = notesOnShape(s);
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
    pushUndo([
      { kind: "shapes", geoms: [{ id, x: s.pos_x, y: s.pos_y, w: s.width, h: s.height, rot: s.rotation }] },
    ]);
    try {
      await applyShapeGeoms([{
        id,
        x: Math.round(minX - PAD),
        y: Math.round(minY - PAD - labelPad),
        w: Math.round(maxX - minX + PAD * 2),
        h: Math.round(maxY - minY + PAD * 2 + labelPad),
        rot: s.rotation,
      }]);
    } catch (e) {
      console.error("fit shape failed", e);
    }
  }, [cardsOnShape, notesOnShape, pushUndo, applyShapeGeoms]);

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
    async (mode: "row" | "col" | "grid" | "dist-h" | "dist-v") => {
      const sel = objectsRef.current.filter((o) => selectedRef.current.has(o.id));
      if (sel.length < 2) return;
      const GAP = 28;
      let moves: { id: string; x: number; y: number }[];
      if (mode === "row") {
        // 横一列: 一番上のカードのYにそろえる（Xは保持）。
        const y = Math.round(Math.min(...sel.map((o) => o.pos_y)));
        moves = sel.map((o) => ({ id: o.id, x: o.pos_x, y }));
      } else if (mode === "col") {
        // 縦一列: 一番左のカードのXにそろえる（Yは保持）。
        const x = Math.round(Math.min(...sel.map((o) => o.pos_x)));
        moves = sel.map((o) => ({ id: o.id, x, y: o.pos_y }));
      } else if (mode === "grid") {
        // 読み順（上→下、左→右）を保ったまま正方形に近い格子へ。
        const sorted = [...sel].sort((a, b) => a.pos_y - b.pos_y || a.pos_x - b.pos_x);
        const cols = Math.ceil(Math.sqrt(sorted.length));
        const x0 = Math.round(Math.min(...sel.map((o) => o.pos_x)));
        const y0 = Math.round(Math.min(...sel.map((o) => o.pos_y)));
        moves = sorted.map((o, i) => ({
          id: o.id,
          x: x0 + (i % cols) * (CARD_W + GAP),
          y: y0 + Math.floor(i / cols) * (CARD_H + GAP),
        }));
      } else {
        // 均等配置: 両端のカードを固定し、間を等間隔に。
        const horiz = mode === "dist-h";
        const sorted = [...sel].sort((a, b) => (horiz ? a.pos_x - b.pos_x : a.pos_y - b.pos_y));
        const first = sorted[0];
        const last = sorted[sorted.length - 1];
        const span = horiz ? last.pos_x - first.pos_x : last.pos_y - first.pos_y;
        const step = span / (sorted.length - 1);
        moves = sorted.map((o, i) => ({
          id: o.id,
          x: horiz ? Math.round(first.pos_x + step * i) : o.pos_x,
          y: horiz ? o.pos_y : Math.round(first.pos_y + step * i),
        }));
      }
      const olds = sel.map((o) => ({ id: o.id, x: o.pos_x, y: o.pos_y }));
      pushUndo([{ kind: "objects", moves: olds }]);
      try {
        await applyObjectMoves(moves);
      } catch (e) {
        console.error("align failed", e);
      }
    },
    [pushUndo, applyObjectMoves]
  );

  // --- Auto layout (重なり解消) ----------------------------------------------

  // Approximate footprint of each card, including its expanded panel.
  const layoutSize = useCallback(
    (o: VirtualBodyInfo): { w: number; h: number } => {
      if (!expanded.has(o.id)) return { w: CARD_W, h: CARD_H };
      const isPdf = o.mime_type === "application/pdf" || (o.ext ?? "").toLowerCase() === "pdf";
      if (o.mime_type.startsWith("image/")) return { w: 420, h: CARD_H + 8 + 480 };
      if (o.mime_type.startsWith("audio/")) return { w: 300, h: CARD_H + 8 + 90 };
      if (o.mime_type.startsWith("video/")) return { w: 420, h: CARD_H + 8 + 460 };
      if (isPdf) return { w: 480, h: CARD_H + 8 + 640 };
      if (isTextMime(o.mime_type, o.ext)) return { w: o.panel_w ?? 360, h: CARD_H + 8 + 270 };
      return { w: CARD_W, h: CARD_H };
    },
    [expanded]
  );

  // Iteratively push overlapping cards apart along the axis of least
  // penetration — positions stay as close to the original as possible.
  // Notes (コメント) are obstacles: cards move out of their way instead.
  const handleAutoLayout = useCallback(async () => {
    const MARGIN = 24;
    type Item = { id: string; x: number; y: number; w: number; h: number; ox: number; oy: number; fixed: boolean };
    const items: Item[] = objectsRef.current.map((o) => {
      const s = layoutSize(o);
      return { id: o.id, x: o.pos_x, y: o.pos_y, w: s.w + MARGIN, h: s.h + MARGIN, ox: o.pos_x, oy: o.pos_y, fixed: false };
    });
    for (const n of notes) {
      const s = measuredNoteSize(n);
      items.push({ id: n.id, x: n.pos_x, y: n.pos_y, w: s.w + MARGIN, h: s.h + MARGIN, ox: n.pos_x, oy: n.pos_y, fixed: true });
    }
    for (let iter = 0; iter < 300; iter++) {
      let any = false;
      for (let i = 0; i < items.length; i++) {
        for (let j = i + 1; j < items.length; j++) {
          const a = items[i];
          const b = items[j];
          if (a.fixed && b.fixed) continue;
          const ox = Math.min(a.x + a.w, b.x + b.w) - Math.max(a.x, b.x);
          const oy = Math.min(a.y + a.h, b.y + b.h) - Math.max(a.y, b.y);
          if (ox <= 0 || oy <= 0) continue;
          any = true;
          if (ox < oy) {
            const dir = a.x + a.w / 2 <= b.x + b.w / 2 ? 1 : -1;
            if (a.fixed) b.x += dir * ox;
            else if (b.fixed) a.x -= dir * ox;
            else {
              a.x -= (dir * ox) / 2;
              b.x += (dir * ox) / 2;
            }
          } else {
            const dir = a.y + a.h / 2 <= b.y + b.h / 2 ? 1 : -1;
            if (a.fixed) b.y += dir * oy;
            else if (b.fixed) a.y -= dir * oy;
            else {
              a.y -= (dir * oy) / 2;
              b.y += (dir * oy) / 2;
            }
          }
        }
      }
      if (!any) break;
    }
    const moves = items
      .filter((it) => !it.fixed && (it.x !== it.ox || it.y !== it.oy))
      .map((it) => ({ id: it.id, x: Math.round(it.x), y: Math.round(it.y) }));
    if (!moves.length) return;
    const olds = items
      .filter((it) => !it.fixed && (it.x !== it.ox || it.y !== it.oy))
      .map((it) => ({ id: it.id, x: it.ox, y: it.oy }));
    pushUndo([{ kind: "objects", moves: olds }]);
    try {
      await applyObjectMoves(moves);
    } catch (e) {
      console.error("auto layout failed", e);
    }
  }, [layoutSize, notes, pushUndo, applyObjectMoves]);

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

  // Notes (free text annotations)
  const handleAddNote = useCallback(async (canvasX: number, canvasY: number) => {
    try {
      const id = await invoke<string>("create_note", { text: "", x: canvasX, y: canvasY });
      pushUndo([{ kind: "deleteNotes", ids: [id] }]);
      setNewNoteId(id);
      await refresh();
    } catch (e) {
      console.error("create_note failed", e);
    }
  }, [refresh, pushUndo]);

  const handleNoteMove = useCallback(async (id: string, x: number, y: number) => {
    const prev = notesRef.current.find((n) => n.id === id);
    try {
      await invoke("move_note", { id, x, y });
      if (prev && (prev.pos_x !== x || prev.pos_y !== y)) {
        pushUndo([{ kind: "notes", moves: [{ id, x: prev.pos_x, y: prev.pos_y }] }]);
      }
      setNotes((prev) => prev.map((n) => (n.id === id ? { ...n, pos_x: x, pos_y: y } : n)));
    } catch (e) {
      console.error("move_note failed", e);
    }
  }, [pushUndo]);

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

  const handleDeleteNote = useCallback(async (id: string) => {
    try {
      await invoke("delete_note", { id });
      // 復活の呪文: 矢印リンクの行も残っているので、Ctrl+Zで一緒に戻る。
      pushUndo([{ kind: "restoreNotes", ids: [id] }]);
      setNotes((prev) => prev.filter((n) => n.id !== id));
    } catch (e) {
      console.error("delete_note failed", e);
    }
  }, [pushUndo]);

  // v1.3-④: コメントを実ファイル化（以後はファイルリンク付箋として編集が書き戻る）。
  const handleCommitNoteToFile = useCallback(async (id: string) => {
    try {
      await invoke("commit_note_to_file", { id });
      await refresh();
    } catch (e) {
      console.error("commit_note_to_file failed", e);
    }
  }, [refresh]);

  const handleContextMenu = useCallback(
    (info: { clientX: number; clientY: number; canvasX: number; canvasY: number }) => {
      const el = document.elementFromPoint(info.clientX, info.clientY);

      // App のハンドラ群をメニュービルダー（ui/menuBuilder.ts）への窓口にまとめる。
      const actions: MenuActions = {
        // サブメニューを同位置に開き直す（既存メニューのクローズ後に）。
        openSubmenu: (sub) =>
          window.setTimeout(() => setMenu({ x: info.clientX, y: info.clientY, items: sub }), 0),
        contextPlace: handleContextPlace,
        addNote: handleAddNote,
        paste: handlePaste,
        shapeAdd: handleShapeAdd,
        addUrl: handleAddUrl,
        addFilter: handleAddFilter,
        runFlow: handleRunFlow,
        editFilter: handleEditFilter,
        feedDrop: handleFeedDrop,
        runDrop: runDownstreamFlows,
        newSubproject: handleNewSubproject,
        linkProject: handleLinkProject,
        autoLayout: handleAutoLayout,
        commitAll: handleCommitAll,
        setTheme,
        switchProject: () => setShowChooser(true),
        focusSelection: handleFocusSelection,
        alignSelected,
        bulkCommit: handleBulkCommit,
        bulkUncommit: handleBulkUncommit,
        bulkDelete: handleBulkDelete,
        openObject: handleOpenObject,
        openDefault: handleOpenDefault,
        openWith: handleOpenWith,
        commitReveal: handleCommitReveal,
        editInCanvas: handleEditInCanvas,
        expandView: handleExpandView,
        collapse: handleCollapse,
        viewTemp: handleViewTemp,
        recommit: handleRecommit,
        uncommit: handleUncommit,
        duplicate: handleDuplicate,
        renameObject: handleRenameObject,
        setMemo: handleSetMemo,
        changeUrl: handleChangeUrl,
        deleteObject: handleDeleteObject,
        requestDelete: handleRequestDelete,
        bringToFront: handleBringToFront,
        sendToBack: handleSendToBack,
        noteStyle: handleNoteStyle,
        commitNoteToFile: handleCommitNoteToFile,
        deleteNote: handleDeleteNote,
        shapeClickSelect: handleShapeClickSelect,
        shapeStyle: handleShapeStyle,
        shapeRotate: handleShapeRotate,
        shapeScale: handleShapeScale,
        shapeFitToCards: handleShapeFitToCards,
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
            (o) => !o.committed_path && !o.project_ref && !o.url_ref && !o.folder_ref
          ).length,
          uncommitable: selObjs.filter(
            (o) => !!o.committed_path && !(o.origin_path && o.origin_path === o.committed_path)
          ).length,
          total: selObjs.length,
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
      } else if (el?.closest<HTMLElement>("[data-shape-id]")) {
        // v1.3-②③: shape menu (shapes sit behind cards)
        const shapeId = el.closest<HTMLElement>("[data-shape-id]")!.dataset.shapeId!;
        items = buildShapeMenu(shapeId, shapesRef.current.find((s) => s.id === shapeId), actions);
      } else {
        items = buildCanvasMenu(
          info,
          { selectionCount: selectedRef.current.size, theme, alignItems, bulkItems },
          actions
        );
      }
      setMenu({ x: info.clientX, y: info.clientY, items });
    },
    [
      handleContextPlace,
      handleDeleteObject,
      handleRenameObject,
      handleOpenObject,
      handleOpenDefault,
      handleOpenWith,
      handleRequestDelete,
      handleRecommit,
      handleUncommit,
      handleDuplicate,
      handleViewTemp,
      handleNewSubproject,
      handleLinkProject,
      handleCommitAll,
      handleAddNote,
      handleDeleteNote,
      handleAddUrl,
      handleChangeUrl,
      handleSetMemo,
      handleNoteStyle,
      handleEditInCanvas,
      handleExpandView,
      handleCollapse,
      handleBringToFront,
      handleSendToBack,
      handleAutoLayout,
      handleCommitReveal,
      handleFocusSelection,
      handlePaste,
      handleShapeAdd,
      handleShapeClickSelect,
      handleShapeStyle,
      handleShapeRotate,
      handleShapeScale,
      handleShapeFitToCards,
      handleShapeLabel,
      handleShapeZ,
      handleShapeDelete,
      alignSelected,
      handleBulkCommit,
      handleBulkUncommit,
      handleBulkDelete,
      handleCommitNoteToFile,
      handleAddFilter,
      handleRunFlow,
      handleEditFilter,
      handleFeedDrop,
      runDownstreamFlows,
      theme,
      expanded,
      notes,
    ]
  );

  // Esc closes all expanded panels (and persists) and clears the selection.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        setExpanded((prev) => {
          prev.forEach((id) => invoke("set_expanded", { id, expanded: false }).catch(() => {}));
          return new Set();
        });
        setSelected(new Set());
        setSelectedNotes(new Set());
        setSelectedShapes(new Set());
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

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
          invalidIds={invalidFlowIds}
        />
        {objects.map((obj) => (
          <ObjectCard
            key={obj.id}
            id={obj.id}
            realId={obj.real_id}
            label={obj.label}
            mimeType={obj.mime_type}
            x={obj.pos_x}
            y={obj.pos_y}
            committed={!!obj.committed_path}
            active={touched.has(obj.id)}
            lost={lostOf(obj)}
            isProject={obj.mime_type === PROJECT_MIME || !!obj.project_ref}
            isUrl={!!obj.url_ref}
            isFolder={!!obj.folder_ref}
            isFilter={obj.kind === "filter" || !!obj.filter_ref}
            isDrop={obj.kind === "drop"}
            memo={obj.memo}
            ext={obj.ext}
            expanded={expanded.has(obj.id)}
            editPath={obj.committed_path}
            panelW={obj.panel_w}
            onPanelResize={handlePanelResize}
            zIndex={cardZ(obj.id)}
            selected={selected.has(obj.id)}
            onToggleSelect={handleCardToggleSelect}
            onGroupDragStart={handleGroupDragStart}
            onMoveEnd={handleMoveEnd}
            onStartLink={handleStartLink}
            onOpen={handleOpenObject}
            onCollapse={handleCollapse}
            onAfterSave={refresh}
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
            fileRef={n.file_ref}
            startEditing={n.id === newNoteId}
            zIndex={objects.length + 10}
            selected={selectedNotes.has(n.id)}
            onToggleSelect={handleNoteToggleSelect}
            onGroupDragStart={handleGroupDragStart}
            onStartLink={handleStartLink}
            onMeasure={handleNoteMeasure}
            onMoveEnd={handleNoteMove}
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
        <ContextMenu x={menu.x} y={menu.y} items={menu.items} onClose={() => setMenu(null)} />
      )}
      {prompt && (
        <PromptDialog
          title={prompt.title}
          initial={prompt.initial}
          onConfirm={(v) => {
            // Close first, THEN run the callback — so a callback that opens
            // another prompt (e.g. URL → title) isn't immediately cleared.
            const cb = prompt.onConfirm;
            setPrompt(null);
            cb(v);
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
          onConfirm={confirm.onConfirm}
          onCancel={() => setConfirm(null)}
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
