import { useRef } from "react";

// v1.3-②③: background shape (角丸四角 / 楕円 / 太矢印).
// Shapes sit BEHIND cards and work as visual thought-groups: dragging a shape
// moves the cards on it together; a plain click selects those cards.
export interface ShapeInfo {
  id: string;
  kind: string; // "rect" | "ellipse" | "arrow"
  pos_x: number;
  pos_y: number;
  width: number;
  height: number;
  rotation: number; // degrees
  fill: string; // hex like #4a90d9
  opacity: number; // 0..1 — 元から半透明（重なりは透ける）
  z_order: number;
  label: string;
}

interface Props {
  shape: ShapeInfo;
  selected?: boolean;
  /** Drag finished: persist geometry. */
  onGeomEnd?: (id: string, x: number, y: number, w: number, h: number) => void;
  /** Plain click (no drag): select the cards on this shape. */
  onClickSelect?: (id: string) => void;
  /** Ctrl+click: toggle this shape in the multi-selection (v1.3.8 マウスモデル: 選択系=Ctrl)。 */
  onToggleSelect?: (id: string) => void;
  /** Live-move the cards riding on the shape. (id, dx, dy, done) */
  onDragCards?: (id: string, dx: number, dy: number, done: boolean) => void;
  /** v1.4.25 ⑤: グリッドスナップ格子(px)。指定時、移動・リサイズの対角がライブで吸着。 */
  snapGrid?: number;
}

export function hexToRgba(hex: string, alpha: number): string {
  const m = /^#?([0-9a-f]{6})$/i.exec(hex.trim());
  if (!m) return hex;
  const n = parseInt(m[1], 16);
  return `rgba(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255}, ${alpha})`;
}

export function ShapeCard({ shape, selected = false, onGeomEnd, onClickSelect, onToggleSelect, onDragCards, snapGrid }: Props) {
  const s = shape;
  const ref = useRef<HTMLDivElement>(null);

  const startDrag = (e: React.MouseEvent<HTMLElement>, mode: "move" | "resize" | "resize-tl") => {
    if (e.button !== 0 || e.altKey) return;
    e.stopPropagation();
    e.preventDefault();
    // v1.3.8 マウスモデル: Ctrl/Meta = 図形を多重選択にトグル（選択系）。ドラッグは始めない。
    if (mode === "move" && (e.ctrlKey || e.metaKey)) {
      onToggleSelect?.(s.id);
      return;
    }
    // Shift+ドラッグ = 図形だけ動かす（カードは置いていく）= 移動系（v1.3.8 で Shift に統一）。
    const solo = e.shiftKey;
    const layer = (e.currentTarget as HTMLElement).closest<HTMLElement>("[data-canvas-layer]");
    const scale = parseFloat(layer?.style.getPropertyValue("--canvas-scale") || "1") || 1;
    const el = ref.current!;
    const startMouseX = e.clientX;
    const startMouseY = e.clientY;
    const start = { x: s.pos_x, y: s.pos_y, w: s.width, h: s.height };
    let dx = 0, dy = 0, curX = start.x, curY = start.y, curW = start.w, curH = start.h;
    let moved = false;

    const onMove = (me: globalThis.MouseEvent) => {
      const rawDx = (me.clientX - startMouseX) / scale;
      const rawDy = (me.clientY - startMouseY) / scale;
      // moved 判定は生の移動量で（スナップで dx=0 に丸まってもドラッグはドラッグ）。
      if (Math.abs(rawDx) > 2 || Math.abs(rawDy) > 2) moved = true;
      dx = rawDx;
      dy = rawDy;
      // v1.4.25 ⑤: スナップ中は対角（左上・右下）を格子へライブ吸着。移動は左上基準の
      // 絶対スナップ（元が格子から外れていても、動かせば格子に乗る）。
      const g = snapGrid;
      if (mode === "move") {
        if (g) {
          dx = Math.round((start.x + rawDx) / g) * g - start.x;
          dy = Math.round((start.y + rawDy) / g) * g - start.y;
        }
        el.style.transform = `translate(${start.x + dx}px, ${start.y + dy}px)`;
        if (!solo) onDragCards?.(s.id, dx, dy, false);
      } else if (mode === "resize-tl") {
        // 左上ハンドル: 右下角を固定したまま左上方向へ拡縮（pos と size を同時に動かす）。
        if (g) {
          const left = Math.round((start.x + rawDx) / g) * g;
          const top = Math.round((start.y + rawDy) / g) * g;
          curW = Math.max(60, start.x + start.w - left);
          curH = Math.max(40, start.y + start.h - top);
        } else {
          curW = Math.max(60, start.w - rawDx);
          curH = Math.max(40, start.h - rawDy);
        }
        curX = start.x + start.w - curW;
        curY = start.y + start.h - curH;
        el.style.width = `${curW}px`;
        el.style.height = `${curH}px`;
        el.style.transform = `translate(${curX}px, ${curY}px)`;
      } else {
        if (g) {
          curW = Math.max(60, Math.round((start.x + start.w + rawDx) / g) * g - start.x);
          curH = Math.max(40, Math.round((start.y + start.h + rawDy) / g) * g - start.y);
        } else {
          curW = Math.max(60, start.w + rawDx);
          curH = Math.max(40, start.h + rawDy);
        }
        el.style.width = `${curW}px`;
        el.style.height = `${curH}px`;
      }
    };
    const onUp = () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
      if (!moved) {
        // 素クリックのみ図形グループを選択。Shift+クリック（動かさず）は何もしない（Shift=移動系）。
        if (mode === "move" && !solo) onClickSelect?.(s.id);
        return;
      }
      if (mode === "move") {
        if (!solo) onDragCards?.(s.id, dx, dy, true);
        onGeomEnd?.(s.id, start.x + dx, start.y + dy, start.w, start.h);
      } else if (mode === "resize-tl") {
        onGeomEnd?.(s.id, Math.round(curX), Math.round(curY), Math.round(curW), Math.round(curH));
      } else {
        onGeomEnd?.(s.id, start.x, start.y, Math.round(curW), Math.round(curH));
      }
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  };

  const bg = hexToRgba(s.fill, s.opacity);
  const borderColor = hexToRgba(s.fill, Math.min(s.opacity * 2 + 0.15, 0.9));

  return (
    <div
      ref={ref}
      data-shape-id={s.id}
      onMouseDown={(e) => startDrag(e, "move")}
      style={{
        position: "absolute",
        left: 0,
        top: 0,
        // v1.4.29 ④: 外枠（＋リサイズハンドル）は常に軸並行に保つ。回転は矢印グラフィック
        // （下の polygon）側だけに適用する。これでハンドルが画面上の実際の角に一致し、
        // 拡縮が直観的になる（旧: 枠ごと rotate してハンドルも回っていた）。ヒットテスト
        // （cardsOnShape）も pos/width/height の AABB なので、描画と判定域が一致する。
        transform: `translate(${s.pos_x}px, ${s.pos_y}px)`,
        width: s.width,
        height: s.height,
        zIndex: 0,
        cursor: "default", // ホバーは矢印のまま（G5）
        willChange: "transform",
      }}
      title={s.label || undefined}
    >
      {s.kind === "arrow" ? (
        <svg
          width="100%"
          height="100%"
          viewBox="0 0 100 100"
          preserveAspectRatio="none"
          style={{ display: "block", overflow: "visible" }}
        >
          {/* 面積のある矢印 (右向き; rotation で方向を変える)。v1.4.29 ④: 回転は枠では
              なく viewBox 内でポリゴンだけに掛ける（中心 50,50 まわり）。枠は軸並行のまま
              なので、縦長の箱＝縦矢印と自然にフィットし、リサイズハンドルも直観的。 */}
          <polygon
            points="0,28 62,28 62,6 100,50 62,94 62,72 0,72"
            fill={bg}
            stroke={borderColor}
            strokeWidth={selected ? 2 : 1}
            vectorEffect="non-scaling-stroke"
            transform={`rotate(${s.rotation} 50 50)`}
          />
        </svg>
      ) : (
        <div
          style={{
            width: "100%",
            height: "100%",
            background: bg,
            border: `${selected ? 2 : 1.5}px solid ${selected ? "var(--accent, #4a90d9)" : borderColor}`,
            borderRadius: s.kind === "ellipse" ? "50%" : 18,
          }}
        />
      )}

      {/* label (top-left, full opacity, not affected by shape alpha) */}
      {s.label && (
        <div
          style={{
            position: "absolute",
            left: s.kind === "ellipse" ? "12%" : 10,
            top: s.kind === "ellipse" ? "8%" : 8,
            fontSize: 12,
            fontFamily: "sans-serif",
            color: "var(--card-text, #d0d8e8)",
            opacity: 0.85,
            pointerEvents: "none",
            maxWidth: "80%",
            overflow: "hidden",
            textOverflow: "ellipsis",
            whiteSpace: "nowrap",
          }}
        >
          {s.label}
        </div>
      )}

      {/* resize handle (top-left) — 右下角を固定して左上方向へ拡縮 */}
      <div
        onMouseDown={(e) => startDrag(e, "resize-tl")}
        title="サイズを変更（左上）"
        style={{
          position: "absolute",
          left: -2,
          top: -2,
          width: 14,
          height: 14,
          cursor: "nwse-resize",
          borderLeft: `2px solid ${borderColor}`,
          borderTop: `2px solid ${borderColor}`,
          borderTopLeftRadius: 6,
        }}
      />

      {/* resize handle (bottom-right) */}
      <div
        onMouseDown={(e) => startDrag(e, "resize")}
        title="サイズを変更"
        style={{
          position: "absolute",
          right: -2,
          bottom: -2,
          width: 14,
          height: 14,
          cursor: "nwse-resize",
          borderRight: `2px solid ${borderColor}`,
          borderBottom: `2px solid ${borderColor}`,
          borderBottomRightRadius: 6,
        }}
      />
    </div>
  );
}
