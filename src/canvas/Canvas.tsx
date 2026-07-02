import { useRef, useState, useCallback, useEffect } from "react";
import type { MouseEvent, WheelEvent } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";

export interface Transform {
  x: number;
  y: number;
  scale: number;
}

interface CanvasProps {
  children?: React.ReactNode;
  onDropFiles?: (files: string[], canvasX: number, canvasY: number) => void;
  /** Dropped text (e.g. a URL dragged from a browser) — Windows only */
  onDropText?: (text: string, canvasX: number, canvasY: number) => void;
  /** Right-click → report screen + canvas coords; parent decides the menu */
  onContextMenu?: (info: {
    clientX: number;
    clientY: number;
    canvasX: number;
    canvasY: number;
  }) => void;
  /** Notifies parent of transform changes so it can convert cursor↔canvas coords */
  onTransformChange?: (t: Transform) => void;
  /** Initial pan/zoom to apply; re-applied whenever `viewKey` changes. */
  initialTransform?: Transform;
  /** Changing this (e.g. project path) resets the view to initialTransform. */
  viewKey?: string;
  /** Rubber-band selection finished (canvas coords). mode: replace=置換 / add=追加 / toggle=範囲トグル(Ctrl)。 */
  onSelectRect?: (
    rect: { x: number; y: number; w: number; h: number },
    mode: "replace" | "add" | "toggle"
  ) => void;
  /** Plain left-click on empty canvas (clear selection etc.). */
  onEmptyClick?: () => void;
  /** Bounding box of all content (canvas coords) — for 「全体を表示」. */
  getContentBounds?: () => { x: number; y: number; w: number; h: number } | null;
  /** Bounding box of the current selection — for 「選択範囲にフォーカス」. */
  getSelectionBounds?: () => { x: number; y: number; w: number; h: number } | null;
  /** Imperative API (fitTo) exposed to the parent. */
  apiRef?: React.MutableRefObject<CanvasApi | null>;
}

export interface CanvasApi {
  /** Pan/zoom so the given canvas-coords rect fills the viewport (padded). */
  fitTo: (b: { x: number; y: number; w: number; h: number } | null) => void;
  /** v1.4.20 ナビゲーションショートカット用: 全体を表示（Home）。 */
  fitAll: () => void;
  /** 選択範囲にフォーカス（F）。 */
  fitSelection: () => void;
  /** ズーム%を直接指定（Ctrl+0 = 100%）。 */
  zoomTo: (pct: number) => void;
}

export function screenToCanvas(transform: Transform, sx: number, sy: number) {
  return {
    x: (sx - transform.x) / transform.scale,
    y: (sy - transform.y) / transform.scale,
  };
}

export function Canvas({ children, onDropFiles, onDropText, onContextMenu, onTransformChange, initialTransform, viewKey, onSelectRect, onEmptyClick, getContentBounds, getSelectionBounds, apiRef }: CanvasProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [transform, setTransform] = useState<Transform>({ x: 0, y: 0, scale: 1 });
  const transformRef = useRef(transform);
  const panDragging = useRef(false);
  const lastPanPos = useRef({ x: 0, y: 0 });
  const [dragOver, setDragOver] = useState(false);
  // Track cursor in CSS (logical) pixels during OS drag-over
  const cursorRef = useRef({ x: 0, y: 0 });
  // Rubber-band selection rectangle in client coords (while dragging)
  const [marquee, setMarquee] = useState<{ x1: number; y1: number; x2: number; y2: number } | null>(null);
  const [zoomMenu, setZoomMenu] = useState(false);
  const zoomMenuRef = useRef<HTMLDivElement>(null);
  const onSelectRectRef = useRef(onSelectRect);
  useEffect(() => { onSelectRectRef.current = onSelectRect; }, [onSelectRect]);
  const onEmptyClickRef = useRef(onEmptyClick);
  useEffect(() => { onEmptyClickRef.current = onEmptyClick; }, [onEmptyClick]);
  const getContentBoundsRef = useRef(getContentBounds);
  useEffect(() => { getContentBoundsRef.current = getContentBounds; }, [getContentBounds]);
  const getSelectionBoundsRef = useRef(getSelectionBounds);
  useEffect(() => { getSelectionBoundsRef.current = getSelectionBounds; }, [getSelectionBounds]);

  useEffect(() => {
    transformRef.current = transform;
    onTransformChange?.(transform);
  }, [transform, onTransformChange]);

  // Apply saved view when the project (viewKey) changes.
  useEffect(() => {
    if (viewKey !== undefined) {
      // 進行中のカメラアニメーションは破棄（前プロジェクトの座標を書き続けないように）
      if (animRef.current != null) {
        cancelAnimationFrame(animRef.current);
        animRef.current = null;
      }
      setTransform(initialTransform ?? { x: 0, y: 0, scale: 1 });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [viewKey]);

  // Keep onDropFiles in a ref so the Tauri listener never needs to re-register
  const onDropFilesRef = useRef(onDropFiles);
  useEffect(() => { onDropFilesRef.current = onDropFiles; }, [onDropFiles]);
  const onDropTextRef = useRef(onDropText);
  useEffect(() => { onDropTextRef.current = onDropText; }, [onDropText]);

  // Windows: events from our custom OLE drop target (files + URL/text).
  // It replaces wry's handler, so the built-in listener below goes quiet there.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    listen<{ kind: string; paths?: string[] | null; text?: string | null; x: number; y: number }>(
      "texos://drag",
      (event) => {
        const p = event.payload;
        const dpr = window.devicePixelRatio || 1;
        if (p.kind === "enter" || p.kind === "over") {
          setDragOver(true);
          cursorRef.current = { x: p.x / dpr, y: p.y / dpr };
        } else if (p.kind === "leave") {
          setDragOver(false);
        } else if (p.kind === "drop" && p.paths?.length) {
          setDragOver(false);
          cursorRef.current = { x: p.x / dpr, y: p.y / dpr };
          const canvas = screenToCanvas(transformRef.current, cursorRef.current.x, cursorRef.current.y);
          onDropFilesRef.current?.(p.paths, canvas.x, canvas.y);
        } else if (p.kind === "drop-text" && p.text) {
          setDragOver(false);
          cursorRef.current = { x: p.x / dpr, y: p.y / dpr };
          const canvas = screenToCanvas(transformRef.current, cursorRef.current.x, cursorRef.current.y);
          onDropTextRef.current?.(p.text, canvas.x, canvas.y);
        }
      }
    ).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });
    return () => { cancelled = true; unlisten?.(); };
  }, []);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    getCurrentWindow()
      .onDragDropEvent((event) => {
        const payload = event.payload as {
          type: string;
          paths?: string[];
          position?: { x: number; y: number };
        };
        if (payload.type === "over") {
          setDragOver(true);
          if (payload.position) {
            const dpr = window.devicePixelRatio || 1;
            cursorRef.current = { x: payload.position.x / dpr, y: payload.position.y / dpr };
          }
        } else if (payload.type === "leave" || payload.type === "cancelled") {
          setDragOver(false);
        } else if (payload.type === "drop" && payload.paths) {
          setDragOver(false);
          if (payload.position) {
            const dpr = window.devicePixelRatio || 1;
            cursorRef.current = { x: payload.position.x / dpr, y: payload.position.y / dpr };
          }
          const canvas = screenToCanvas(transformRef.current, cursorRef.current.x, cursorRef.current.y);
          onDropFilesRef.current?.(payload.paths, canvas.x, canvas.y);
        }
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      });
    return () => { cancelled = true; unlisten?.(); };
  }, []);

  const onMouseDown = useCallback((e: MouseEvent) => {
    // v1.3.8 マウスモデル: パン = 中ボタン、または Shift+空キャンバスドラッグ。
    // （Alt+LMB パンは廃止。手のひらカーソルは onMouseMove でドラッグ開始後に出す。）
    if (e.button === 1 || (e.button === 0 && e.shiftKey)) {
      e.preventDefault();
      // アニメーション中のパン開始は即座に主導権を渡す（v1.4.20）
      if (animRef.current != null) {
        cancelAnimationFrame(animRef.current);
        animRef.current = null;
      }
      panDragging.current = true;
      lastPanPos.current = { x: e.clientX, y: e.clientY };
      return;
    }
    // 空キャンバスの左ドラッグ → 範囲選択（カードは伝播を止めるので、ここに来る＝空き）。
    //   素ドラッグ = 置換、Ctrl/Meta = 範囲トグル。Shift は上でパンに割り当て済み。
    if (e.button === 0) {
      const selectMode: "replace" | "toggle" = e.ctrlKey || e.metaKey ? "toggle" : "replace";
      const sx = e.clientX;
      const sy = e.clientY;
      let moved = false;
      const onMove = (me: globalThis.MouseEvent) => {
        if (Math.abs(me.clientX - sx) > 4 || Math.abs(me.clientY - sy) > 4) moved = true;
        setMarquee({ x1: sx, y1: sy, x2: me.clientX, y2: me.clientY });
      };
      const onUp = (me: globalThis.MouseEvent) => {
        window.removeEventListener("mousemove", onMove);
        window.removeEventListener("mouseup", onUp);
        setMarquee(null);
        if (!moved) {
          onEmptyClickRef.current?.();
          return;
        }
        const rect = containerRef.current!.getBoundingClientRect();
        const p1 = screenToCanvas(
          transformRef.current,
          Math.min(sx, me.clientX) - rect.left,
          Math.min(sy, me.clientY) - rect.top
        );
        const p2 = screenToCanvas(
          transformRef.current,
          Math.max(sx, me.clientX) - rect.left,
          Math.max(sy, me.clientY) - rect.top
        );
        onSelectRectRef.current?.(
          { x: p1.x, y: p1.y, w: p2.x - p1.x, h: p2.y - p1.y },
          selectMode
        );
      };
      window.addEventListener("mousemove", onMove);
      window.addEventListener("mouseup", onUp);
    }
  }, []);

  const onMouseMove = useCallback((e: MouseEvent) => {
    if (!panDragging.current) return;
    document.body.style.cursor = "grabbing"; // 手のひらはパン開始後（G5）
    const dx = e.clientX - lastPanPos.current.x;
    const dy = e.clientY - lastPanPos.current.y;
    lastPanPos.current = { x: e.clientX, y: e.clientY };
    setTransform((t) => ({ ...t, x: t.x + dx, y: t.y + dy }));
  }, []);

  const onMouseUp = useCallback(() => {
    if (panDragging.current) document.body.style.cursor = "";
    panDragging.current = false;
  }, []);

  // Right-click → convert to canvas coords (clean: clientX/Y only, no DPR/window math)
  const handleContextMenu = useCallback((e: MouseEvent) => {
    e.preventDefault();
    const rect = containerRef.current!.getBoundingClientRect();
    const sx = e.clientX - rect.left;
    const sy = e.clientY - rect.top;
    const pos = screenToCanvas(transformRef.current, sx, sy);
    onContextMenu?.({ clientX: e.clientX, clientY: e.clientY, canvasX: pos.x, canvasY: pos.y });
  }, [onContextMenu]);

  const onWheel = useCallback((e: WheelEvent) => {
    e.preventDefault();
    // アニメーション中のホイールは即座に主導権を渡す（v1.4.20）
    if (animRef.current != null) {
      cancelAnimationFrame(animRef.current);
      animRef.current = null;
    }
    const factor = e.deltaY < 0 ? 1.1 : 0.9;
    const rect = containerRef.current!.getBoundingClientRect();
    const cx = e.clientX - rect.left;
    const cy = e.clientY - rect.top;
    setTransform((t) => {
      const newScale = Math.min(Math.max(t.scale * factor, 0.05), 20);
      const ratio = newScale / t.scale;
      return { scale: newScale, x: cx - ratio * (cx - t.x), y: cy - ratio * (cy - t.y) };
    });
  }, []);

  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const handler = (e: globalThis.WheelEvent) => e.preventDefault();
    el.addEventListener("wheel", handler, { passive: false });
    return () => el.removeEventListener("wheel", handler);
  }, []);

  // --- v1.4.20 カメラアニメーション（プレゼン向け）--------------------------------
  // fitTo / zoomTo をイージング付きで滑らかに移動する。補間は「ビュー中心（キャンバス
  // 座標）＝線形」「スケール＝対数（等比）」— ズームを伴う移動が自然に見える定石。
  // ユーザーのパン/ホイール操作で即キャンセル（操作を奪わない）。時間は右下ズーム
  // メニューで選択（即時/高速/中速/低速）・localStorage 永続。
  const animRef = useRef<number | null>(null);
  const cancelAnim = useCallback(() => {
    if (animRef.current != null) {
      cancelAnimationFrame(animRef.current);
      animRef.current = null;
    }
  }, []);
  const [navDuration, setNavDuration] = useState<number>(() => {
    const v = parseInt(localStorage.getItem("texos-nav-anim") ?? "500", 10);
    return isNaN(v) ? 500 : v;
  });
  const navDurationRef = useRef(navDuration);
  navDurationRef.current = navDuration;
  const setNavAnim = useCallback((ms: number) => {
    setNavDuration(ms);
    localStorage.setItem("texos-nav-anim", String(ms));
  }, []);

  const animateTo = useCallback(
    (target: Transform) => {
      cancelAnim();
      const dur = navDurationRef.current;
      if (dur <= 0) {
        setTransform(target);
        return;
      }
      const from = transformRef.current;
      const rect = containerRef.current!.getBoundingClientRect();
      const vw = rect.width / 2;
      const vh = rect.height / 2;
      // ビュー中心のキャンバス座標（from / target それぞれ）
      const c0 = { x: (vw - from.x) / from.scale, y: (vh - from.y) / from.scale };
      const c1 = { x: (vw - target.x) / target.scale, y: (vh - target.y) / target.scale };
      const s0 = from.scale;
      const s1 = target.scale;
      const t0 = performance.now();
      // easeInOutCubic: 加速→巡航→減速
      const ease = (t: number) => (t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2);
      const step = (now: number) => {
        const k = Math.min(1, (now - t0) / dur);
        const e = ease(k);
        const s = s0 * Math.pow(s1 / s0, e);
        const cx = c0.x + (c1.x - c0.x) * e;
        const cy = c0.y + (c1.y - c0.y) * e;
        setTransform({ scale: s, x: vw - cx * s, y: vh - cy * s });
        if (k < 1) animRef.current = requestAnimationFrame(step);
        else animRef.current = null;
      };
      animRef.current = requestAnimationFrame(step);
    },
    [cancelAnim]
  );
  useEffect(() => cancelAnim, [cancelAnim]); // アンマウント時に停止

  // Set an exact zoom ratio, keeping the viewport center fixed.
  const applyZoom = useCallback((pct: number) => {
    const s = Math.min(Math.max(pct, 5), 2000) / 100;
    const rect = containerRef.current!.getBoundingClientRect();
    const cx = rect.width / 2;
    const cy = rect.height / 2;
    const t = transformRef.current;
    const ratio = s / t.scale;
    animateTo({ scale: s, x: cx - ratio * (cx - t.x), y: cy - ratio * (cy - t.y) });
    setZoomMenu(false);
  }, [animateTo]);

  // Fit a canvas-coords rect into the viewport (centered, padded).
  const fitTo = useCallback(
    (b: { x: number; y: number; w: number; h: number } | null) => {
      if (!b || b.w <= 0 || b.h <= 0) return;
      const rect = containerRef.current!.getBoundingClientRect();
      const pad = 60;
      const raw = Math.min((rect.width - pad * 2) / b.w, (rect.height - pad * 2) / b.h);
      const s = Math.min(Math.max(raw, 0.05), 1.5); // don't zoom in absurdly for tiny content
      animateTo({
        scale: s,
        x: (rect.width - b.w * s) / 2 - b.x * s,
        y: (rect.height - b.h * s) / 2 - b.y * s,
      });
    },
    [animateTo]
  );

  // 全体を表示: fit every card/note into the viewport.
  const fitAll = useCallback(() => {
    setZoomMenu(false);
    fitTo(getContentBoundsRef.current?.() ?? null);
  }, [fitTo]);

  // 選択範囲にフォーカス: fit the selected cards into the viewport.
  const fitSelection = useCallback(() => {
    setZoomMenu(false);
    fitTo(getSelectionBoundsRef.current?.() ?? null);
  }, [fitTo]);

  // Expose the imperative API to the parent (選択範囲にフォーカス・ナビゲーションキー等).
  useEffect(() => {
    if (!apiRef) return;
    apiRef.current = { fitTo, fitAll, fitSelection, zoomTo: applyZoom };
    return () => { apiRef.current = null; };
  }, [apiRef, fitTo, fitAll, fitSelection, applyZoom]);

  // Close the zoom menu on outside click.
  useEffect(() => {
    if (!zoomMenu) return;
    const onDown = (e: globalThis.MouseEvent) => {
      if (zoomMenuRef.current && !zoomMenuRef.current.contains(e.target as Node)) {
        setZoomMenu(false);
      }
    };
    window.addEventListener("mousedown", onDown, true);
    return () => window.removeEventListener("mousedown", onDown, true);
  }, [zoomMenu]);

  return (
    <div
      ref={containerRef}
      style={{
        width: "100vw",
        height: "100vh",
        overflow: "hidden",
        background: dragOver ? "var(--canvas-bg-drag, #1e2a40)" : "var(--canvas-bg, #1a1a2e)",
        cursor: "default", // パン中の手のひらは document.body.style.cursor で制御（G5）
        userSelect: "none",
        outline: dragOver ? "2px dashed var(--accent, #4a90d9)" : "none",
        transition: "background 0.15s",
      }}
      onMouseDown={onMouseDown}
      onMouseMove={onMouseMove}
      onMouseUp={onMouseUp}
      onMouseLeave={onMouseUp}
      onWheel={onWheel}
      onContextMenu={handleContextMenu}
    >
      {/* pass scale via CSS var so ObjectCard can read it without prop drilling */}
      <div
        data-canvas-layer
        style={{
          position: "absolute",
          transformOrigin: "0 0",
          transform: `translate(${transform.x}px, ${transform.y}px) scale(${transform.scale})`,
          ["--canvas-scale" as string]: String(transform.scale),
        }}
      >
        {children}
      </div>
      <div
        style={{
          position: "fixed", bottom: 12, right: 16, display: "flex", gap: 6,
          color: "var(--chrome-text, #ffffff44)", opacity: 0.75,
          fontSize: 11, fontFamily: "monospace", userSelect: "none",
        }}
        onMouseDown={(e) => e.stopPropagation()}
      >
        <span
          title="クリックで拡大率を選択"
          style={{ cursor: "pointer" }}
          onClick={() => setZoomMenu((v) => !v)}
        >
          {Math.round(transform.scale * 100)}%
        </span>
        <span>·</span>
        <span
          title="クリックで原点 (0,0) に戻る"
          style={{ cursor: "pointer" }}
          onClick={() => setTransform((t) => ({ ...t, x: 0, y: 0 }))}
        >
          ({Math.round(-transform.x / transform.scale)},{" "}
          {Math.round(-transform.y / transform.scale)})
        </span>
      </div>
      {zoomMenu && (
        <div
          ref={zoomMenuRef}
          onMouseDown={(e) => e.stopPropagation()}
          style={{
            position: "fixed", bottom: 34, right: 16,
            background: "#1c2130", border: "1px solid #38445e", borderRadius: 8,
            boxShadow: "0 6px 24px #000000aa", padding: 8,
            display: "flex", flexDirection: "column", gap: 4,
            fontFamily: "sans-serif", fontSize: 12, color: "#d6dded", zIndex: 1000,
          }}
        >
          <div
            onClick={fitAll}
            style={{ padding: "4px 14px", borderRadius: 5, cursor: "pointer", textAlign: "center", whiteSpace: "nowrap" }}
            onMouseEnter={(e) => ((e.currentTarget as HTMLElement).style.background = "#2a3346")}
            onMouseLeave={(e) => ((e.currentTarget as HTMLElement).style.background = "transparent")}
          >
            全体を表示<span style={{ color: "#6c7587", fontSize: 10, marginLeft: 8 }}>Home</span>
          </div>
          {getSelectionBoundsRef.current?.() && (
            <div
              onClick={fitSelection}
              style={{ padding: "4px 14px", borderRadius: 5, cursor: "pointer", textAlign: "center", whiteSpace: "nowrap" }}
              onMouseEnter={(e) => ((e.currentTarget as HTMLElement).style.background = "#2a3346")}
              onMouseLeave={(e) => ((e.currentTarget as HTMLElement).style.background = "transparent")}
            >
              選択範囲にフォーカス<span style={{ color: "#6c7587", fontSize: 10, marginLeft: 8 }}>F</span>
            </div>
          )}
          <div style={{ height: 1, background: "#38445e", margin: "2px 4px" }} />
          {[50, 100, 150, 200, 400].map((pct) => (
            <div
              key={pct}
              onClick={() => applyZoom(pct)}
              style={{ padding: "4px 14px", borderRadius: 5, cursor: "pointer", textAlign: "center" }}
              onMouseEnter={(e) => ((e.currentTarget as HTMLElement).style.background = "#2a3346")}
              onMouseLeave={(e) => ((e.currentTarget as HTMLElement).style.background = "transparent")}
            >
              {pct}%
            </div>
          ))}
          <form
            onSubmit={(e) => {
              e.preventDefault();
              const v = Number((e.currentTarget.elements.namedItem("pct") as HTMLInputElement).value);
              if (v > 0) applyZoom(v);
            }}
            style={{ display: "flex", gap: 4, marginTop: 2 }}
          >
            <input
              name="pct"
              type="number"
              min={5}
              max={2000}
              placeholder="任意%"
              style={{
                width: 60, background: "#0f1420", color: "#d6dded",
                border: "1px solid #38445e", borderRadius: 5, padding: "3px 6px", fontSize: 12,
              }}
            />
            <button
              type="submit"
              style={{
                background: "#2a3346", color: "#d6dded", border: "none",
                borderRadius: 5, padding: "3px 8px", cursor: "pointer", fontSize: 12,
              }}
            >
              適用
            </button>
          </form>
          {/* v1.4.20 フォーカス移動の速度（プレゼン向けカメラアニメーション）。 */}
          <div style={{ height: 1, background: "#38445e", margin: "6px 4px 4px" }} />
          <div style={{ fontSize: 10, color: "#6c7587", padding: "0 4px 3px" }}>フォーカス移動</div>
          <div style={{ display: "flex", gap: 3 }}>
            {([["即時", 0], ["高速", 250], ["中速", 500], ["低速", 900]] as [string, number][]).map(
              ([label, ms]) => (
                <div
                  key={ms}
                  onClick={() => setNavAnim(ms)}
                  title="F・Home・検索フォーカスなどの画面移動をこの速さでアニメーションします"
                  style={{
                    flex: 1,
                    padding: "3px 0",
                    borderRadius: 5,
                    cursor: "pointer",
                    textAlign: "center",
                    fontSize: 11,
                    background: navDuration === ms ? "#3a6fb0" : "transparent",
                    color: navDuration === ms ? "#fff" : "#d6dded",
                  }}
                  onMouseEnter={(e) => {
                    if (navDuration !== ms) (e.currentTarget as HTMLElement).style.background = "#2a3346";
                  }}
                  onMouseLeave={(e) => {
                    if (navDuration !== ms) (e.currentTarget as HTMLElement).style.background = "transparent";
                  }}
                >
                  {label}
                </div>
              )
            )}
          </div>
        </div>
      )}
      {marquee && (
        <div
          style={{
            position: "fixed",
            left: Math.min(marquee.x1, marquee.x2),
            top: Math.min(marquee.y1, marquee.y2),
            width: Math.abs(marquee.x2 - marquee.x1),
            height: Math.abs(marquee.y2 - marquee.y1),
            border: "1px solid var(--accent, #4a90d9)",
            background: "#4a90d922",
            pointerEvents: "none",
            zIndex: 999,
          }}
        />
      )}
      {dragOver && (
        <div style={{
          position: "fixed", inset: 0, display: "flex",
          alignItems: "center", justifyContent: "center",
          color: "#4a90d9", fontSize: 18, pointerEvents: "none",
        }}>
          ファイル / URL をドロップしてオブジェクトとして登録
        </div>
      )}
    </div>
  );
}
