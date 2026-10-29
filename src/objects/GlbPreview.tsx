import { useEffect, useRef, useState } from "react";
import { invoke, convertFileSrc } from "@tauri-apps/api/core";
import * as THREE from "three";
import { GLTFLoader } from "three/addons/loaders/GLTFLoader.js";
import { OrbitControls } from "three/addons/controls/OrbitControls.js";
import { RoomEnvironment } from "three/addons/environments/RoomEnvironment.js";
import { usePanelResize, cornerHandleStyle, panelMouseDown } from "./panelResize";

interface Props {
  objId: string;
  label: string;
  /** vertical offset from the card origin (card height + gap) */
  top: number;
  /** panel size (v1.5.16 方式, user-resizable; persisted by the parent) */
  width?: number | null;
  height?: number | null;
  onResizeEnd?: (width: number, height?: number) => void;
  onClose: () => void;
  /** 「サムネイル更新」でカード面のサムネイルを保存した後（親が再取得する）。 */
  onThumbnailSaved?: () => void;
}

/**
 * v2.0 t23d Phase 2: glb/gltf の汎用インライン 3D ビューア（V2.0_t23d設計.md §5）。
 * three.js（GLTFLoader＋OrbitControls）のフロントエンド完結＝GPU デーモン不要。
 * t23d の出力に限らず、Blender 書き出し等どの glb 実身でも開ける。
 * 「サムネイル更新」= 今見えているアングルを canvas.toDataURL で PNG 化し、
 * 画像カードと同じ .texos/thumbs/<real_id>.jpg 機構（save_card_thumbnail）へ保存する。
 */
export function GlbPreview({
  objId, label, top, width = null, height = null, onResizeEnd, onClose, onThumbnailSaved,
}: Props) {
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [savedMark, setSavedMark] = useState(false);
  const mountRef = useRef<HTMLDivElement>(null);
  // renderer 一式（unmount 時の dispose と「サムネイル更新」から触る）。
  const glRef = useRef<{
    renderer: THREE.WebGLRenderer;
    scene: THREE.Scene;
    camera: THREE.PerspectiveCamera;
    controls: OrbitControls;
    render: () => void;
  } | null>(null);

  const { w, h, panelRef, startCornerDrag } = usePanelResize({
    width: width ?? 420,
    height: height ?? 360,
    minW: 260,
    minH: 220,
    onEnd: (rw, rh) => onResizeEnd?.(rw, rh),
  });

  useEffect(() => {
    const mount = mountRef.current;
    if (!mount) return;
    let disposed = false;

    const renderer = new THREE.WebGLRenderer({
      antialias: true,
      // toDataURL（サムネイル保存）を任意のタイミングで有効にするため描画バッファを保持。
      preserveDrawingBuffer: true,
    });
    renderer.setPixelRatio(window.devicePixelRatio || 1);
    // 背景はテーマに寄せた無彩色（白メッシュ・テクスチャどちらも映える明度）。
    const scene = new THREE.Scene();
    scene.background = new THREE.Color(0x363b46);
    const camera = new THREE.PerspectiveCamera(45, 1, 0.01, 100);
    camera.position.set(1.6, 1.2, 1.6);

    // 設計 §5: ライティングは環境光＋方向光の固定プリセット（HDRI 等は将来）。
    // ⚠️ 環境マップは必須: Hunyuan の glb は metallicFactor 未指定＝glTF 既定 1.0（完全金属）で、
    // 環境マップが無いと真っ黒に沈む（実測）。three 同梱の RoomEnvironment を使う
    // （<model-viewer> の既定と同じ流儀・外部アセット不要）。
    const pmrem = new THREE.PMREMGenerator(renderer);
    scene.environment = pmrem.fromScene(new RoomEnvironment()).texture;
    pmrem.dispose();
    scene.add(new THREE.HemisphereLight(0xffffff, 0x445066, 1.1));
    const dir = new THREE.DirectionalLight(0xffffff, 1.6);
    dir.position.set(2, 3, 2);
    scene.add(dir);

    const controls = new OrbitControls(camera, renderer.domElement);
    controls.enableDamping = false;
    const render = () => renderer.render(scene, camera);
    controls.addEventListener("change", render);

    mount.appendChild(renderer.domElement);
    renderer.domElement.style.display = "block";
    glRef.current = { renderer, scene, camera, controls, render };

    // パネルのライブリサイズ（--preview-body-h／幅 style 直書き）にも追従する。
    const ro = new ResizeObserver(() => {
      const cw = mount.clientWidth;
      const ch = mount.clientHeight;
      if (cw > 0 && ch > 0) {
        renderer.setSize(cw, ch);
        camera.aspect = cw / ch;
        camera.updateProjectionMatrix();
        render();
      }
    });
    ro.observe(mount);

    // 実身をプロジェクトへ実体化 → asset プロトコルで GLTFLoader に読ませる
    //（MediaPreview と同じ経路。バイト列の base64 往復を避ける）。
    void (async () => {
      try {
        const path = await invoke<string>("materialize_view", { id: objId });
        const gltf = await new GLTFLoader().loadAsync(convertFileSrc(path));
        if (disposed) return;
        const obj = gltf.scene;
        // 自動フレーミング: バウンディングボックス中心を注視点に、大きさから距離を決める。
        const box = new THREE.Box3().setFromObject(obj);
        const center = box.getCenter(new THREE.Vector3());
        const size = box.getSize(new THREE.Vector3());
        const maxDim = Math.max(size.x, size.y, size.z) || 1;
        scene.add(obj);
        controls.target.copy(center);
        camera.position.copy(center).add(new THREE.Vector3(0.85, 0.55, 0.85).multiplyScalar(maxDim * 1.35));
        camera.near = maxDim / 100;
        camera.far = maxDim * 100;
        camera.updateProjectionMatrix();
        controls.update();
        setLoading(false);
        render();
      } catch (e) {
        console.error("glb preview failed", objId, e);
        if (!disposed) {
          setLoading(false);
          setError(String(e));
        }
      }
    })();

    return () => {
      disposed = true;
      ro.disconnect();
      controls.dispose();
      // シーン内のジオメトリ/マテリアル/テクスチャを解放（WebGL コンテキストのリーク防止）。
      scene.traverse((o) => {
        const mesh = o as THREE.Mesh;
        if (mesh.geometry) mesh.geometry.dispose();
        const mats = Array.isArray(mesh.material) ? mesh.material : mesh.material ? [mesh.material] : [];
        for (const m of mats) {
          for (const v of Object.values(m)) {
            if (v instanceof THREE.Texture) v.dispose();
          }
          m.dispose();
        }
      });
      renderer.dispose();
      renderer.domElement.remove();
      glRef.current = null;
    };
  }, [objId]);

  // 今見えているアングルをカード面のサムネイルに（設計 §5 案1）。
  const saveThumbnail = async () => {
    const gl = glRef.current;
    if (!gl) return;
    try {
      gl.render(); // 直前に明示描画（バッファを確実に最新へ）
      const dataUrl = gl.renderer.domElement.toDataURL("image/png");
      await invoke("save_card_thumbnail", { id: objId, dataUrl });
      setSavedMark(true);
      window.setTimeout(() => setSavedMark(false), 1500);
      onThumbnailSaved?.();
    } catch (e) {
      console.error("save_card_thumbnail failed", objId, e);
      setError(`サムネイルを保存できませんでした: ${e}`);
    }
  };

  return (
    <div
      ref={panelRef}
      onMouseDown={panelMouseDown}
      onDoubleClick={(e) => e.stopPropagation()}
      style={{
        position: "absolute",
        left: 0,
        top,
        width: w,
        background: "var(--panel-bg, #0d0f16)",
        border: "1px solid var(--panel-border, #2a3a5a)",
        borderRadius: 8,
        boxShadow: "0 4px 20px #000000aa",
        overflow: "hidden",
        zIndex: 5,
        fontFamily: "sans-serif",
      }}
    >
      <div
        style={{
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          gap: 6,
          padding: "6px 8px",
          background: "var(--panel-header-bg, #161b26)",
          fontSize: 11,
          color: "var(--panel-header-text, #c7d2e6)",
        }}
      >
        <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
          🧊 {label}
        </span>
        <span style={{ display: "flex", gap: 6, flex: "none" }}>
          <button
            onClick={saveThumbnail}
            disabled={loading || !!error}
            title="今の角度をカードのサムネイルにします（ドラッグで回転・ホイールで拡縮してから押してください）"
            style={{
              padding: "3px 9px",
              background: savedMark ? "#3fae6a" : "var(--panel-btn-bg, #3a4658)",
              color: "#fff",
              border: "none",
              borderRadius: 5,
              cursor: "pointer",
              fontSize: 11,
            }}
          >
            {savedMark ? "✓ 保存済" : "📷 サムネイル更新"}
          </button>
          <button
            onClick={onClose}
            style={{
              padding: "3px 9px",
              background: "var(--panel-btn-bg, #3a4658)",
              color: "#fff",
              border: "none",
              borderRadius: 5,
              cursor: "pointer",
              fontSize: 11,
            }}
          >
            ✕
          </button>
        </span>
      </div>
      <div
        style={{
          position: "relative",
          width: "100%",
          height: `var(--preview-body-h, ${h}px)`,
          overflow: "hidden",
        }}
      >
        <div ref={mountRef} style={{ position: "absolute", inset: 0 }} />
        {loading && !error && (
          <div style={{ position: "absolute", inset: 0, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--panel-muted, #7a8aa6)", fontSize: 12 }}>
            読み込み中...
          </div>
        )}
        {error && (
          <div style={{ position: "absolute", inset: 0, display: "flex", alignItems: "center", justifyContent: "center", color: "var(--panel-danger, #ff7b7b)", fontSize: 12, padding: 16, textAlign: "center" }}>
            3Dモデルを表示できませんでした（{error}）
          </div>
        )}
      </div>
      <div onMouseDown={startCornerDrag} title="サイズを変更" style={cornerHandleStyle} />
    </div>
  );
}
