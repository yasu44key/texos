// 選択状態（カード・コメント・図形）の管理（App.tsx から分離 — v1.3.2 リファクタリング）。
// 矩形選択・Ctrl+クリックのトグル・全解除。ドラッグでのまとめ移動は App 側に残す
// （UNDO・座標適用と結合しているため）。
import { useCallback, useRef, useState } from "react";
import type { MutableRefObject } from "react";
import type { NoteInfo, VirtualBodyInfo } from "../model";
import type { ShapeInfo } from "../objects/ShapeCard";

interface UseSelectionDeps {
  objectsRef: MutableRefObject<VirtualBodyInfo[]>;
  notesRef: MutableRefObject<NoteInfo[]>;
  shapesRef: MutableRefObject<ShapeInfo[]>;
  // 実測サイズ（ResizeObserver）優先のコメント寸法。
  measuredNoteSize: (n: NoteInfo) => { w: number; h: number };
  // 開いたパネルを含むカードの実寸（パネルに掛かる矩形でも選択できるように）。
  measuredCardSize: (id: string) => { w: number; h: number };
}

export function useSelection({ objectsRef, notesRef, shapesRef, measuredNoteSize, measuredCardSize }: UseSelectionDeps) {
  // Rubber-band selection (group move).
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const selectedRef = useRef<Set<string>>(selected);
  selectedRef.current = selected;
  // v1.3.1: notes participate in selection (rubber-band / shape group).
  const [selectedNotes, setSelectedNotes] = useState<Set<string>>(new Set());
  const selectedNotesRef = useRef<Set<string>>(selectedNotes);
  selectedNotesRef.current = selectedNotes;
  // v1.3 メモ④: 図形はShift+クリックで複数選択できる（まとめて移動）。
  const [selectedShapes, setSelectedShapes] = useState<Set<string>>(new Set());
  const selectedShapesRef = useRef<Set<string>>(selectedShapes);
  selectedShapesRef.current = selectedShapes;

  // 範囲選択。mode: replace=置換（素LMBドラッグ）/ add=追加 / toggle=範囲トグル（Ctrl+ドラッグ）。
  const handleSelectRect = useCallback(
    (rect: { x: number; y: number; w: number; h: number }, mode: "replace" | "add" | "toggle") => {
      const hit = objectsRef.current
        .filter((o) => {
          const s = measuredCardSize(o.id);
          return (
            o.pos_x < rect.x + rect.w &&
            o.pos_x + s.w > rect.x &&
            o.pos_y < rect.y + rect.h &&
            o.pos_y + s.h > rect.y
          );
        })
        .map((o) => o.id);
      // v1.3.1: notes join the rubber-band selection too.
      const hitNotes = notesRef.current
        .filter((n) => {
          const s = measuredNoteSize(n);
          return (
            n.pos_x < rect.x + rect.w &&
            n.pos_x + s.w > rect.x &&
            n.pos_y < rect.y + rect.h &&
            n.pos_y + s.h > rect.y
          );
        })
        .map((n) => n.id);
      // v1.4.24 ④: 矩形に「完全に収まっている」図形だけ選択に含める。図形は背景の
      // 大物なので、カードと同じ交差判定にすると大図形の上でラバーバンドを引くたびに
      // 選択されてしまう。丸ごと囲んだ図形＝グループの一部として運ぶ意図、はみ出す
      // 図形＝背景、の区別。回転図形は無回転バウンディングボックスで近似判定。
      const hitShapes = shapesRef.current
        .filter(
          (s) =>
            s.pos_x >= rect.x &&
            s.pos_y >= rect.y &&
            s.pos_x + s.width <= rect.x + rect.w &&
            s.pos_y + s.height <= rect.y + rect.h
        )
        .map((s) => s.id);
      const apply = (prev: Set<string>, hits: string[]) => {
        if (mode === "replace") return new Set(hits);
        if (mode === "add") return new Set([...prev, ...hits]);
        const next = new Set(prev); // toggle: 範囲内の各IDの選択状態を反転
        for (const id of hits) {
          if (next.has(id)) next.delete(id);
          else next.add(id);
        }
        return next;
      };
      setSelected((prev) => apply(prev, hit));
      setSelectedNotes((prev) => apply(prev, hitNotes));
      setSelectedShapes((prev) => apply(prev, hitShapes));
    },
    [objectsRef, notesRef, shapesRef, measuredNoteSize, measuredCardSize]
  );

  // v1.3 メモ２追加: Ctrl+クリックで選択を個別にトグル（追加／解除）。
  const handleCardToggleSelect = useCallback((id: string) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);

  const handleNoteToggleSelect = useCallback((id: string) => {
    setSelectedNotes((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);

  const handleShapeToggleSelect = useCallback((id: string) => {
    setSelectedShapes((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);

  const handleClearSelection = useCallback(() => {
    setSelected((prev) => (prev.size ? new Set<string>() : prev));
    setSelectedNotes((prev) => (prev.size ? new Set<string>() : prev));
    setSelectedShapes((prev) => (prev.size ? new Set<string>() : prev));
  }, []);

  // 素クリック = そのオブジェクトだけを選択（他は全解除）。標準のウィンドウシステム流。
  const handleCardSelectOnly = useCallback((id: string) => {
    setSelected(new Set([id]));
    setSelectedNotes(new Set());
    setSelectedShapes(new Set());
  }, []);

  const handleNoteSelectOnly = useCallback((id: string) => {
    setSelectedNotes(new Set([id]));
    setSelected(new Set());
    setSelectedShapes(new Set());
  }, []);

  return {
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
  };
}
