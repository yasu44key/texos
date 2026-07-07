export interface RelationInfo {
  id: number;
  from_id: string;
  to_id: string;
  kind: string;
  /// v1.5.3 WS-P1: 矢印の制御属性 JSON（{when, carry, port}・null=従来）。
  attrs?: string | null;
}

/// v1.5.3 WS-P1: 矢印の中点ラベル（通過条件・運ぶもの）。null=無条件（表示なし）。
function edgeLabel(attrs?: string | null): string | null {
  if (!attrs) return null;
  try {
    const a = JSON.parse(attrs);
    const parts: string[] = [];
    if (a.when === "success") parts.push("成功時");
    else if (a.when === "failure") parts.push("失敗時");
    if (a.carry === "none") parts.push("信号のみ");
    else if (a.carry === "input") parts.push("入力を運ぶ");
    return parts.length ? parts.join("・") : null;
  } catch {
    return null;
  }
}

/** Anything an arrow can attach to: object cards AND comment notes (v1.3.1). */
export interface NodeBox {
  id: string;
  x: number;
  y: number;
  w: number;
  h: number;
}

interface LinkLayerProps {
  nodes: NodeBox[];
  relations: RelationInfo[];
  /** Temporary line while dragging a new link (canvas coords). */
  tempLine?: { x1: number; y1: number; x2: number; y2: number } | null;
  onDeleteRelation?: (id: number) => void;
  /** v1.5.3 WS-P1: flow 矢印のクリック（条件メニューを開く）。未指定なら削除にフォールバック。 */
  onFlowRelationClick?: (r: RelationInfo, clientX: number, clientY: number) => void;
  /** v1.4 オートフロー: 型チェック不合格のフロー矢印（赤で警告表示）。 */
  invalidIds?: Set<number>;
}

// v1.4.17: ドラッグ中の矢印追従（App.redrawArrowsFor）が同じ端点計算を使うため export。
export function center(n: NodeBox) {
  return { x: n.x + n.w / 2, y: n.y + n.h / 2 };
}

/** Point on the node rect edge (centered at c) along the direction toward `toward`. */
export function edgePoint(n: NodeBox, toward: { x: number; y: number }) {
  const c = center(n);
  const dx = toward.x - c.x;
  const dy = toward.y - c.y;
  if (dx === 0 && dy === 0) return c;
  const tx = dx !== 0 ? n.w / 2 / Math.abs(dx) : Infinity;
  const ty = dy !== 0 ? n.h / 2 / Math.abs(dy) : Infinity;
  const t = Math.min(tx, ty);
  return { x: c.x + dx * t, y: c.y + dy * t };
}

export function LinkLayer({
  nodes,
  relations,
  tempLine,
  onDeleteRelation,
  onFlowRelationClick,
  invalidIds,
}: LinkLayerProps) {
  const byId = new Map(nodes.map((n) => [n.id, n]));

  // Large symmetric viewBox so canvas coords (incl. negatives) map 1:1 and
  // are never clipped by the SVG viewport.
  const B = 100000;
  return (
    <svg
      width={2 * B}
      height={2 * B}
      viewBox={`${-B} ${-B} ${2 * B} ${2 * B}`}
      style={{
        position: "absolute",
        left: -B,
        top: -B,
        overflow: "visible",
        pointerEvents: "none",
        zIndex: 0,
      }}
    >
      <defs>
        <marker
          id="arrowhead"
          markerWidth="8"
          markerHeight="8"
          refX="6"
          refY="3"
          orient="auto"
          markerUnits="strokeWidth"
        >
          <path d="M0,0 L6,3 L0,6 Z" style={{ fill: "var(--arrow-color, #5a9fe0)" }} />
        </marker>
        {/* v1.4: フロー矢印（パイプ）と型不一致警告のマーカー */}
        <marker
          id="arrowhead-flow"
          markerWidth="8"
          markerHeight="8"
          refX="6"
          refY="3"
          orient="auto"
          markerUnits="strokeWidth"
        >
          <path d="M0,0 L6,3 L0,6 Z" style={{ fill: "var(--arrow-flow, #57c7a3)" }} />
        </marker>
        <marker
          id="arrowhead-bad"
          markerWidth="8"
          markerHeight="8"
          refX="6"
          refY="3"
          orient="auto"
          markerUnits="strokeWidth"
        >
          <path d="M0,0 L6,3 L0,6 Z" style={{ fill: "var(--arrow-bad, #d96a6a)" }} />
        </marker>
      </defs>

      {relations.map((r) => {
        const from = byId.get(r.from_id);
        const to = byId.get(r.to_id);
        if (!from || !to) return null;
        const p1 = edgePoint(from, center(to));
        const p2 = edgePoint(to, center(from));
        // v1.4: flow矢印はパイプの見た目（破線・緑系）、型不一致は赤で実行前に分かる。
        const isFlow = r.kind === "flow";
        const bad = invalidIds?.has(r.id) ?? false;
        const stroke = bad
          ? "var(--arrow-bad, #d96a6a)"
          : isFlow
          ? "var(--arrow-flow, #57c7a3)"
          : "var(--arrow-color, #5a9fe0)";
        const markerId = bad ? "arrowhead-bad" : isFlow ? "arrowhead-flow" : "arrowhead";
        // v1.5.3 WS-P1: 条件付き矢印は中点にラベル（成功時/失敗時・信号のみ 等）。
        const label = isFlow ? edgeLabel(r.attrs) : null;
        const mid = { x: (p1.x + p2.x) / 2, y: (p1.y + p2.y) / 2 };
        return (
          <g key={r.id}>
            {/* invisible fat line for easier clicking */}
            {/* data-rel-id: ドラッグ中はカードと同じ命令的DOM更新で端点を追従させる（v1.4.17） */}
            <line
              data-rel-id={r.id}
              x1={p1.x}
              y1={p1.y}
              x2={p2.x}
              y2={p2.y}
              stroke="transparent"
              strokeWidth={12}
              style={{ pointerEvents: "stroke", cursor: "pointer" }}
              onClick={(e) => {
                // v1.5.3 WS-P1: flow 矢印は条件メニュー（削除/通過条件/運ぶもの）、
                // リンク矢印は従来どおりクリック=削除。
                if (isFlow && onFlowRelationClick) onFlowRelationClick(r, e.clientX, e.clientY);
                else onDeleteRelation?.(r.id);
              }}
            />
            <line
              data-rel-id={r.id}
              x1={p1.x}
              y1={p1.y}
              x2={p2.x}
              y2={p2.y}
              style={{
                stroke,
                strokeWidth: "var(--arrow-width, 2)",
              }}
              strokeDasharray={isFlow ? "9 5" : undefined}
              markerEnd={`url(#${markerId})`}
            />
            {label && (
              <text
                x={mid.x}
                y={mid.y - 6}
                textAnchor="middle"
                style={{
                  fill: stroke,
                  fontSize: 11,
                  fontFamily: "sans-serif",
                  pointerEvents: "none",
                  paintOrder: "stroke",
                  stroke: "var(--canvas-bg, #10131a)",
                  strokeWidth: 3,
                }}
              >
                {label}
              </text>
            )}
          </g>
        );
      })}

      {tempLine && (
        <line
          x1={tempLine.x1}
          y1={tempLine.y1}
          x2={tempLine.x2}
          y2={tempLine.y2}
          style={{
            stroke: "var(--arrow-temp, #7fbfff)",
            strokeWidth: "var(--arrow-width, 2)",
          }}
          strokeDasharray="6 4"
          markerEnd="url(#arrowhead)"
        />
      )}
    </svg>
  );
}
