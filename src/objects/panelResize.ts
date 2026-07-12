import { useRef, useState } from "react";

/**
 * v1.5.16: プレビュー共通のリサイズ（InlineTextEditor v1.3.1 の幅ハンドル方式を一般化）。
 * - startWidthDrag = 右端ハンドル（画像・動画。高さは中身のアスペクト追従なので幅だけ）
 * - startCornerDrag = 右下ハンドル（PDF・Doc。幅と高さを独立に変える）
 * キャンバス倍率（--canvas-scale）を補正し、確定時のみ onEnd で親へ通知＝DB永続化。
 * ドラッグ中は iframe / video がマウスイベントを飲むため pointer-events を一時停止する。
 * 高さのライブ反映は CSS 変数 --preview-body-h（中身が var() で読む）。
 */
export function usePanelResize(opts: {
  width: number;
  height: number;
  minW?: number;
  minH?: number;
  onEnd?: (w: number, h?: number) => void;
}) {
  const { minW = 240, minH = 160 } = opts;
  const [w, setW] = useState(opts.width);
  const [h, setH] = useState(opts.height);
  const panelRef = useRef<HTMLDivElement>(null);

  const start = (mode: "width" | "corner") => (e: React.MouseEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    e.stopPropagation();
    e.preventDefault();
    const layer = (e.currentTarget as HTMLElement).closest<HTMLElement>("[data-canvas-layer]");
    const scale = parseFloat(layer?.style.getPropertyValue("--canvas-scale") || "1") || 1;
    const startX = e.clientX;
    const startY = e.clientY;
    const startW = w;
    const startH = h;
    let curW = startW;
    let curH = startH;
    const media = Array.from(
      panelRef.current?.querySelectorAll<HTMLElement>("iframe, video") ?? []
    );
    media.forEach((el) => (el.style.pointerEvents = "none"));
    const onMove = (me: globalThis.MouseEvent) => {
      curW = Math.max(minW, startW + (me.clientX - startX) / scale);
      if (panelRef.current) panelRef.current.style.width = `${curW}px`;
      if (mode === "corner") {
        curH = Math.max(minH, startH + (me.clientY - startY) / scale);
        panelRef.current?.style.setProperty("--preview-body-h", `${curH}px`);
      }
    };
    const onUp = () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
      media.forEach((el) => (el.style.pointerEvents = ""));
      const rw = Math.round(curW);
      const rh = Math.round(curH);
      setW(rw);
      if (mode === "corner") setH(rh);
      if (rw !== startW || (mode === "corner" && rh !== startH)) {
        opts.onEnd?.(rw, mode === "corner" ? rh : undefined);
      }
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  };

  return {
    w,
    h,
    panelRef,
    startWidthDrag: start("width"),
    startCornerDrag: start("corner"),
  };
}

/** 右端の幅ハンドル（InlineTextEditor と同じ 7px 帯）。 */
export const widthHandleStyle: React.CSSProperties = {
  position: "absolute",
  right: 0,
  top: 0,
  bottom: 0,
  width: 7,
  cursor: "ew-resize",
  zIndex: 2,
};

/** 右下のコーナーハンドル（幅＋高さ）。 */
export const cornerHandleStyle: React.CSSProperties = {
  position: "absolute",
  right: 0,
  bottom: 0,
  width: 16,
  height: 16,
  cursor: "nwse-resize",
  zIndex: 2,
};
