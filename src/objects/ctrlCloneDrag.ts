// Ctrl+左ドラッグ＝この場にコピーを作る（カードとコメントで共用）。
// クリックとドラッグは移動量で区別する（4px 超でコピー、未満で onClick＝選択トグル）。
// v1.4.29 ①: ドラッグ中は半透明のゴーストをポインタに追従表示する（「この場に
// コピーしている」実感）。実物を cloneNode して見た目そのままのゴーストにし、掴んだ
// 相対位置（grab）とキャンバス倍率（scale）を保って画面上のサイズ・位置を一致させる。
// onDrop には新しい物の左上の画面座標（＝ゴーストの左上）を渡す。ポインタ位置を左上に
// すると、掴んだ分だけ右下へずれて見える。
export function startCtrlCloneDrag(
  e: React.MouseEvent<HTMLElement>,
  el: HTMLElement,
  onDrop: (clientX: number, clientY: number) => void,
  onClick: () => void
) {
  const sx = e.clientX;
  const sy = e.clientY;
  const rect = el.getBoundingClientRect();
  const grabX = sx - rect.left;
  const grabY = sy - rect.top;
  const layer = el.closest<HTMLElement>("[data-canvas-layer]");
  const scale = parseFloat(layer?.style.getPropertyValue("--canvas-scale") || "1") || 1;
  let dragged = false;
  let ghost: HTMLElement | null = null;
  const moveGhost = (mx: number, my: number) => {
    if (ghost) ghost.style.transform = `translate(${mx - grabX}px, ${my - grabY}px) scale(${scale})`;
  };
  const onMove = (me: globalThis.MouseEvent) => {
    if (!dragged && Math.hypot(me.clientX - sx, me.clientY - sy) > 4) {
      dragged = true;
      document.body.style.cursor = "copy";
      const g = el.cloneNode(true) as HTMLElement;
      g.style.position = "fixed";
      g.style.left = "0";
      g.style.top = "0";
      g.style.margin = "0";
      g.style.transformOrigin = "0 0";
      g.style.opacity = "0.55";
      g.style.pointerEvents = "none";
      g.style.transition = "none";
      g.style.zIndex = "2147483647";
      document.body.appendChild(g);
      ghost = g;
      moveGhost(me.clientX, me.clientY);
    } else if (dragged) {
      moveGhost(me.clientX, me.clientY);
    }
  };
  const onUp = (me: globalThis.MouseEvent) => {
    window.removeEventListener("mousemove", onMove);
    window.removeEventListener("mouseup", onUp);
    document.body.style.cursor = "";
    ghost?.remove();
    ghost = null;
    if (dragged) onDrop(me.clientX - grabX, me.clientY - grabY);
    else onClick();
  };
  window.addEventListener("mousemove", onMove);
  window.addEventListener("mouseup", onUp);
}
