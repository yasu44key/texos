// v1.3 メモ② UNDO/REDO（App.tsx から分離 — v1.3.2 リファクタリング）。
// アンドゥはシリアライズ可能な操作レコードで持ち、プロジェクトDB
// (.texos/texos.db の meta) に保存して再起動後も巻き戻せる（メモ技術③）。
import { useCallback, useEffect, useRef } from "react";
import type { Dispatch, MutableRefObject, SetStateAction } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { ShapeInfo } from "../objects/ShapeCard";
import type { NoteInfo, VirtualBodyInfo } from "../model";

export type UndoAction =
  | { kind: "objects"; moves: { id: string; x: number; y: number }[] }
  | { kind: "notes"; moves: { id: string; x: number; y: number }[] }
  | { kind: "shapes"; geoms: { id: string; x: number; y: number; w: number; h: number; rot: number }[] }
  | { kind: "shapeStyle"; id: string; fill: string; opacity: number }
  | { kind: "noteStyle"; id: string; width: number; fontSize: number; bold: boolean }
  // 復活の呪文（削除のUNDO）と再削除（そのREDO）— 全オブジェクト種別ぶん。
  | { kind: "restore"; ids: string[] }
  | { kind: "delete"; ids: string[] }
  | { kind: "restoreNotes"; ids: string[] }
  | { kind: "deleteNotes"; ids: string[] }
  | { kind: "restoreShapes"; ids: string[] }
  | { kind: "deleteShapes"; ids: string[] }
  | { kind: "restoreRelations"; ids: number[] }
  | { kind: "deleteRelations"; ids: number[] };

export interface UndoEntry {
  t: number; // Date.now() — 同一マウス操作から出た複数エントリのバッチ判定に使う
  actions: UndoAction[];
}

interface UseUndoDeps {
  objectsRef: MutableRefObject<VirtualBodyInfo[]>;
  notesRef: MutableRefObject<NoteInfo[]>;
  shapesRef: MutableRefObject<ShapeInfo[]>;
  setObjects: Dispatch<SetStateAction<VirtualBodyInfo[]>>;
  setNotes: Dispatch<SetStateAction<NoteInfo[]>>;
  setShapes: Dispatch<SetStateAction<ShapeInfo[]>>;
  refresh: () => Promise<void>;
}

export function useUndo({
  objectsRef,
  notesRef,
  shapesRef,
  setObjects,
  setNotes,
  setShapes,
  refresh,
}: UseUndoDeps) {
  // 位置・サイズ・整列・削除（復活の呪文）など「動かす系」操作を巻き戻す。
  // 1回のマウス操作から複数エントリが生まれる場合（図形＋上のカード等）は
  // 同時刻バッチとして1回のCtrl+Zでまとめて戻す。
  const undoStack = useRef<UndoEntry[]>([]);
  // REDO (Ctrl+Shift+Z / Ctrl+Y): UNDO直前の状態から作った逆レコードを積む。
  const redoStack = useRef<UndoEntry[]>([]);
  const undoSaveTimer = useRef<number | null>(null);
  const scheduleSaveUndo = useCallback(() => {
    if (undoSaveTimer.current) window.clearTimeout(undoSaveTimer.current);
    undoSaveTimer.current = window.setTimeout(() => {
      undoSaveTimer.current = null;
      invoke("set_undo_stack", {
        json: JSON.stringify({ undo: undoStack.current, redo: redoStack.current }),
      }).catch(() => {});
    }, 400);
  }, []);
  const pushUndo = useCallback((actions: UndoAction[]) => {
    if (!actions.length) return;
    undoStack.current.push({ t: Date.now(), actions });
    if (undoStack.current.length > 100) undoStack.current.shift();
    redoStack.current = []; // 新しい操作をしたら「やり直す未来」は消える
    scheduleSaveUndo();
  }, [scheduleSaveUndo]);

  // 位置・ジオメトリの一括適用（通常操作とアンドゥの両方から使う）。
  const applyObjectMoves = useCallback(async (moves: { id: string; x: number; y: number }[]) => {
    if (!moves.length) return;
    await invoke("move_objects", { moves });
    setObjects((prev) =>
      prev.map((o) => {
        const m = moves.find((mv) => mv.id === o.id);
        return m ? { ...o, pos_x: m.x, pos_y: m.y } : o;
      })
    );
  }, [setObjects]);

  const applyNoteMoves = useCallback(async (moves: { id: string; x: number; y: number }[]) => {
    if (!moves.length) return;
    for (const m of moves) await invoke("move_note", { id: m.id, x: m.x, y: m.y });
    setNotes((prev) =>
      prev.map((n) => {
        const m = moves.find((mv) => mv.id === n.id);
        return m ? { ...n, pos_x: m.x, pos_y: m.y } : n;
      })
    );
  }, [setNotes]);

  const applyShapeGeoms = useCallback(
    async (geoms: { id: string; x: number; y: number; w: number; h: number; rot: number }[]) => {
      if (!geoms.length) return;
      for (const g of geoms) {
        await invoke("update_shape_geom", {
          id: g.id, x: g.x, y: g.y, width: g.w, height: g.h, rotation: g.rot,
        });
      }
      setShapes((prev) =>
        prev.map((s) => {
          const g = geoms.find((gg) => gg.id === s.id);
          return g ? { ...s, pos_x: g.x, pos_y: g.y, width: g.w, height: g.h, rotation: g.rot } : s;
        })
      );
    },
    [setShapes]
  );

  // アンドゥレコードを1件実行する。再起動後の履歴では対象が既に消えている
  // こともあるため、各操作は欠けがあっても止まらずに進む（安全側）。
  const runUndoAction = useCallback(
    async (a: UndoAction) => {
      switch (a.kind) {
        case "objects":
          await applyObjectMoves(a.moves);
          break;
        case "notes":
          await applyNoteMoves(a.moves);
          break;
        case "shapes":
          await applyShapeGeoms(a.geoms);
          break;
        case "shapeStyle":
          await invoke("set_shape_style", { id: a.id, fill: a.fill, opacity: a.opacity });
          setShapes((prev) =>
            prev.map((s) => (s.id === a.id ? { ...s, fill: a.fill, opacity: a.opacity } : s))
          );
          break;
        case "noteStyle":
          await invoke("set_note_style", {
            id: a.id, width: a.width, fontSize: a.fontSize, bold: a.bold,
          });
          setNotes((prev) =>
            prev.map((n) =>
              n.id === a.id ? { ...n, width: a.width, font_size: a.fontSize, bold: a.bold } : n
            )
          );
          break;
        case "restore":
        case "delete":
        case "restoreNotes":
        case "deleteNotes":
        case "restoreShapes":
        case "deleteShapes":
        case "restoreRelations":
        case "deleteRelations": {
          const cmd = {
            restore: "restore_object",
            delete: "delete_object",
            restoreNotes: "restore_note",
            deleteNotes: "delete_note",
            restoreShapes: "restore_shape",
            deleteShapes: "delete_shape",
            restoreRelations: "restore_relation",
            deleteRelations: "delete_relation",
          }[a.kind];
          for (const id of a.ids) {
            try {
              await invoke(cmd, { id });
            } catch (e) {
              console.error(`${cmd} failed`, id, e);
            }
          }
          await refresh();
          break;
        }
      }
    },
    [applyObjectMoves, applyNoteMoves, applyShapeGeoms, setNotes, setShapes, refresh]
  );

  // あるレコードの「逆」を今の状態から作る（REDO/UNDO相互変換の素）。
  // 対象が既に存在しない場合はその分だけ欠ける（安全側）。
  const inverseOf = useCallback((a: UndoAction): UndoAction | null => {
    switch (a.kind) {
      case "objects": {
        const moves = a.moves.flatMap((m) => {
          const o = objectsRef.current.find((x) => x.id === m.id);
          return o ? [{ id: m.id, x: o.pos_x, y: o.pos_y }] : [];
        });
        return moves.length ? { kind: "objects", moves } : null;
      }
      case "notes": {
        const moves = a.moves.flatMap((m) => {
          const n = notesRef.current.find((x) => x.id === m.id);
          return n ? [{ id: m.id, x: n.pos_x, y: n.pos_y }] : [];
        });
        return moves.length ? { kind: "notes", moves } : null;
      }
      case "shapes": {
        const geoms = a.geoms.flatMap((g) => {
          const s = shapesRef.current.find((x) => x.id === g.id);
          return s
            ? [{ id: g.id, x: s.pos_x, y: s.pos_y, w: s.width, h: s.height, rot: s.rotation }]
            : [];
        });
        return geoms.length ? { kind: "shapes", geoms } : null;
      }
      case "shapeStyle": {
        const s = shapesRef.current.find((x) => x.id === a.id);
        return s ? { kind: "shapeStyle", id: a.id, fill: s.fill, opacity: s.opacity } : null;
      }
      case "noteStyle": {
        const n = notesRef.current.find((x) => x.id === a.id);
        return n
          ? { kind: "noteStyle", id: a.id, width: n.width, fontSize: n.font_size, bold: n.bold }
          : null;
      }
      case "restore":
        return { kind: "delete", ids: a.ids };
      case "delete":
        return { kind: "restore", ids: a.ids };
      case "restoreNotes":
        return { kind: "deleteNotes", ids: a.ids };
      case "deleteNotes":
        return { kind: "restoreNotes", ids: a.ids };
      case "restoreShapes":
        return { kind: "deleteShapes", ids: a.ids };
      case "deleteShapes":
        return { kind: "restoreShapes", ids: a.ids };
      case "restoreRelations":
        return { kind: "deleteRelations", ids: a.ids };
      case "deleteRelations":
        return { kind: "restoreRelations", ids: a.ids };
    }
  }, [objectsRef, notesRef, shapesRef]);

  // UNDO/REDOの共通実行: 片方のスタックからバッチを取り、適用前の状態から
  // 逆バッチを作ってもう片方へ積む。
  const doUndoRedo = useCallback(
    (redo: boolean) => {
      const from = redo ? redoStack.current : undoStack.current;
      const to = redo ? undoStack.current : redoStack.current;
      const last = from.pop();
      if (!last) return;
      const batch = [last];
      // 同一マウス操作（≒同時刻）のエントリだけをまとめる。REDOスタックは
      // 逆時系列なので、符号付き比較だと全部負になり一括適用されてしまう。
      while (from.length && Math.abs(last.t - from[from.length - 1].t) < 150) {
        batch.push(from.pop()!);
      }
      for (const e of batch) {
        const actions = e.actions
          .map(inverseOf)
          .filter((x): x is UndoAction => x !== null);
        if (actions.length) to.push({ t: e.t, actions });
      }
      if (to.length > 100) to.splice(0, to.length - 100);
      scheduleSaveUndo();
      (async () => {
        for (const e of batch) {
          for (const a of e.actions) await runUndoAction(a);
        }
      })().catch((err) => console.error(redo ? "redo failed" : "undo failed", err));
    },
    [inverseOf, runUndoAction, scheduleSaveUndo]
  );

  // v1.3 メモ②: Ctrl+Z = UNDO、Ctrl+Shift+Z / Ctrl+Y = REDO。
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey)) return;
      const k = e.key.toLowerCase();
      const isRedo = (k === "z" && e.shiftKey) || (k === "y" && !e.shiftKey);
      const isUndo = k === "z" && !e.shiftKey;
      if (!isRedo && !isUndo) return;
      const ae = document.activeElement as HTMLElement | null;
      if (ae && (ae.tagName === "INPUT" || ae.tagName === "TEXTAREA" || ae.isContentEditable)) return;
      e.preventDefault();
      doUndoRedo(isRedo);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [doUndoRedo]);

  // アンドゥはプロジェクト単位。切替時に前プロジェクトの履歴（と保存予約）を破棄する。
  const resetStacks = useCallback(() => {
    if (undoSaveTimer.current) {
      window.clearTimeout(undoSaveTimer.current);
      undoSaveTimer.current = null;
    }
    undoStack.current = [];
    redoStack.current = [];
  }, []);

  // v1.4.15 データの掃除A: 現プロジェクトの UNDO/REDO 履歴を消して DB にも空で永続化する
  // （resetStacks はメモリだけ・切替時用。こちらは明示クリアなので DB も空にする）。
  // 戻り値＝消去した履歴エントリ数（掃除ダイアログの表示用）。
  const clearHistory = useCallback(async () => {
    const n = undoStack.current.length + redoStack.current.length;
    if (undoSaveTimer.current) {
      window.clearTimeout(undoSaveTimer.current);
      undoSaveTimer.current = null;
    }
    undoStack.current = [];
    redoStack.current = [];
    await invoke("set_undo_stack", {
      json: JSON.stringify({ undo: [], redo: [] }),
    }).catch(() => {});
    return n;
  }, []);

  // このプロジェクトのDBに保存されている履歴を読み込む（再起動後も続く）。
  const loadStacks = useCallback(() => {
    invoke<string | null>("get_undo_stack")
      .then((j) => {
        if (!j) return;
        try {
          const v = JSON.parse(j);
          // ロード完了前にユーザーが操作していたら、その分を後ろに残す。
          // （旧形式＝配列のみ、新形式＝{undo, redo} の両方を受ける）
          if (Array.isArray(v)) {
            undoStack.current = [...v, ...undoStack.current];
          } else if (v && typeof v === "object") {
            // ロード前に新しい操作が入っていたらREDOは無効（通常の規則どおり）。
            const hadNewOps = undoStack.current.length > 0;
            if (Array.isArray(v.undo)) undoStack.current = [...v.undo, ...undoStack.current];
            if (Array.isArray(v.redo) && !hadNewOps) redoStack.current = v.redo;
          }
        } catch {
          /* 壊れた履歴は無視（次の保存で上書きされる） */
        }
      })
      .catch(() => {});
  }, []);

  return {
    pushUndo,
    applyObjectMoves,
    applyNoteMoves,
    applyShapeGeoms,
    resetStacks,
    loadStacks,
    clearHistory,
  };
}
