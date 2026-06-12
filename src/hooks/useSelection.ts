// 選択状態（カード・コメント・図形）の管理（App.tsx から分離 — v1.3.2 リファクタリング）。
// 矩形選択・Ctrl+クリックのトグル・全解除。ドラッグでのまとめ移動は App 側に残す
// （UNDO・座標適用と結合しているため）。
import { useCallback, useRef, useState } from "react";
import type { MutableRefObject } from "react";
import { CARD_W, CARD_H } from "../objects/ObjectCard";
import type { NoteInfo, VirtualBodyInfo } from "../model";

interface UseSelectionDeps {
  objectsRef: MutableRefObject<VirtualBodyInfo[]>;
  notesRef: MutableRefObject<NoteInfo[]>;
  // 実測サイズ（ResizeObserver）優先のコメント寸法。
  measuredNoteSize: (n: NoteInfo) => { w: number; h: number };
}

export function useSelection({ objectsRef, notesRef, measuredNoteSize }: UseSelectionDeps) {
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

  const handleSelectRect = useCallback(
    (rect: { x: number; y: number; w: number; h: number }, additive: boolean) => {
      const hit = objectsRef.current
        .filter(
          (o) =>
            o.pos_x < rect.x + rect.w &&
            o.pos_x + CARD_W > rect.x &&
            o.pos_y < rect.y + rect.h &&
            o.pos_y + CARD_H > rect.y
        )
        .map((o) => o.id);
      setSelected((prev) => (additive ? new Set([...prev, ...hit]) : new Set(hit)));
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
      setSelectedNotes((prev) => (additive ? new Set([...prev, ...hitNotes]) : new Set(hitNotes)));
    },
    [objectsRef, notesRef, measuredNoteSize]
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
  };
}
