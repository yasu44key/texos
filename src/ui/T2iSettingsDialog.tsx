import { useState } from "react";

// v1.5 Text-To-Image（builtin:"t2i"）のパラメータ設定ダイアログ。
// AiSettingsDialog と同じ流儀＝新規作成・既存編集で共通。プロンプト本体は接続した
// テキストカードから実行時に渡るので、ここで設定するのは「生成パラメータ」のみ
// （モデル / 負プロンプト / ステップ / ガイダンス / 画像サイズ / シード）。

export interface T2iSpec {
  builtin: "t2i";
  model?: string;
  negative?: string;
  steps?: number;
  guidance?: number;
  width?: number;
  height?: number;
  seed?: number | null;
  strength?: number;
  timeout_secs?: number;
  accept_mime?: string;
  output_ext?: string;
}

interface Props {
  initialLabel: string;
  initialSpec: string; // JSON（builtin:"t2i" を含む）
  /** 取得済みモデル名（DL済み）。先頭が既定候補。 */
  models: string[];
  /** 既知モデル名（DL候補。取得済みでなくても選択肢に出す）。 */
  known?: string[];
  onCancel: () => void;
  onSave: (label: string, spec: string) => void;
  /** モデル取得などのセットアップを開く（未取得時の導線）。 */
  onOpenSetup?: () => void;
}

export function T2iSettingsDialog({ initialLabel, initialSpec, models, known = [], onCancel, onSave, onOpenSetup }: Props) {
  const init: T2iSpec = (() => {
    try {
      return { ...(JSON.parse(initialSpec) as T2iSpec) };
    } catch {
      return { builtin: "t2i" };
    }
  })();

  const [label, setLabel] = useState(initialLabel);
  const [model, setModel] = useState(init.model || models[0] || "sd1.5");
  const [negative, setNegative] = useState(init.negative ?? "");
  const [steps, setSteps] = useState(String(init.steps ?? 25));
  const [guidance, setGuidance] = useState(String(init.guidance ?? 7.5));
  const [width, setWidth] = useState(String(init.width ?? 512));
  const [height, setHeight] = useState(String(init.height ?? 512));
  const [seed, setSeed] = useState(init.seed === null || init.seed === undefined ? "" : String(init.seed));
  const [strength, setStrength] = useState(String(init.strength ?? 0.6));
  // 制限時間（秒）。FLUX は初回モデルロードで数分かかることがあるので既定を長めに。
  const [timeoutSecs, setTimeoutSecs] = useState(String(init.timeout_secs ?? 600));

  const save = () => {
    const to = parseInt(timeoutSecs, 10);
    const spec: T2iSpec = {
      builtin: "t2i",
      model: model.trim() || "sd1.5",
      negative: negative.trim() || undefined,
      steps: parseInt(steps, 10) || 25,
      guidance: parseFloat(guidance) || 7.5,
      width: parseInt(width, 10) || 512,
      height: parseInt(height, 10) || 512,
      seed: seed.trim() === "" ? null : parseInt(seed, 10),
      strength: parseFloat(strength) || 0.6,
      timeout_secs: isNaN(to) ? undefined : to,
      // */* ＝ テキスト（プロンプト）に加えて画像（img2img の元画像）も接続できる。
      accept_mime: "*/*",
      output_ext: "png",
    };
    onSave(label.trim() || "画像生成", JSON.stringify(spec));
  };

  return (
    <div style={overlay} onMouseDown={onCancel}>
      <div onMouseDown={(e) => e.stopPropagation()} style={panel}>
        <div style={{ fontSize: 15, marginBottom: 4, textAlign: "center" }}>
          画像生成（Text-To-Image）の設定
        </div>
        <div style={{ fontSize: 11, color: "#8a93a8", marginBottom: 14, textAlign: "center" }}>
          接続したテキストカード（プロンプト）→ 画像（PNG実身）。プロンプト本体は実行時に渡ります。
        </div>

        <label style={row}>
          <span style={lab}>表示名</span>
          <input style={inp} value={label} onChange={(e) => setLabel(e.target.value)} />
        </label>

        <label style={row}>
          <span style={lab}>モデル</span>
          {/* 既知モデル ∪ 取得済み ∪ 現在値 を重複なく並べる（取得済みは ✓）。
              `models` が一時的に欠けても既知モデルは常に選べる＝堅牢。 */}
          <select style={inp} value={model} onChange={(e) => setModel(e.target.value)}>
            {Array.from(new Set([...known, ...models, model])).map((m) => (
              <option key={m} value={m}>
                {models.includes(m) ? "✓ " : "　"}
                {m}
              </option>
            ))}
          </select>
        </label>
        {/* 常駐のセットアップ導線＋（未取得なら）警告。取得済みでもモデル追加/管理/サーバー制御へ行ける。 */}
        <div style={{ ...note, marginBottom: 8, display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap" }}>
          {!models.includes(model) && (
            <span style={{ color: "#d9a06a" }}>「{model}」は未取得です。</span>
          )}
          {onOpenSetup && (
            <button style={linkBtn} onClick={onOpenSetup}>
              ⚙ モデルの取得・管理（セットアップ）…
            </button>
          )}
        </div>

        <label style={row}>
          <span style={lab}>負プロンプト</span>
          <input
            style={inp}
            value={negative}
            placeholder="除外したい要素（任意）例: lowres, blurry"
            onChange={(e) => setNegative(e.target.value)}
          />
        </label>

        {/* 数値は「ラベルを上・入力を下」のコンパクトなセルにして横あふれを防ぐ。 */}
        <div style={cellRow}>
          <label style={cell}>
            <span style={cellLab}>ステップ</span>
            <input style={cellInp} value={steps} onChange={(e) => setSteps(e.target.value)} />
          </label>
          <label style={cell}>
            <span style={cellLab}>ガイダンス</span>
            <input style={cellInp} value={guidance} onChange={(e) => setGuidance(e.target.value)} />
          </label>
          <label style={cell}>
            <span style={cellLab}>制限時間(秒)</span>
            <input
              style={cellInp}
              value={timeoutSecs}
              placeholder="0=無制限"
              onChange={(e) => setTimeoutSecs(e.target.value)}
            />
          </label>
        </div>
        <div style={{ ...note, marginBottom: 4 }}>
          制限時間: 生成がこれを超えると中断（GPUも解放）。FLUXは初回ロードで数分かかるため長め推奨・0=無制限。
        </div>

        <div style={cellRow}>
          <label style={cell}>
            <span style={cellLab}>幅</span>
            <input style={cellInp} value={width} onChange={(e) => setWidth(e.target.value)} />
          </label>
          <label style={cell}>
            <span style={cellLab}>高さ</span>
            <input style={cellInp} value={height} onChange={(e) => setHeight(e.target.value)} />
          </label>
          <label style={cell}>
            <span style={cellLab}>シード</span>
            <input style={cellInp} value={seed} placeholder="空=毎回" onChange={(e) => setSeed(e.target.value)} />
          </label>
          <label style={cell}>
            <span style={cellLab}>変化度(img2img)</span>
            <input style={cellInp} value={strength} placeholder="0.6" onChange={(e) => setStrength(e.target.value)} />
          </label>
        </div>
        <div style={{ ...note, marginBottom: 4 }}>
          画像カードも接続すると img2img（元画像を変化度ぶん変える）。0=元画像のまま〜1=大きく変える。
        </div>

        <div style={{ display: "flex", justifyContent: "flex-end", gap: 8, marginTop: 18 }}>
          <button style={btnGhost} onClick={onCancel}>
            キャンセル
          </button>
          <button style={btn} onClick={save}>
            保存
          </button>
        </div>
      </div>
    </div>
  );
}

const overlay: React.CSSProperties = {
  position: "fixed",
  inset: 0,
  background: "#00000066",
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
  zIndex: 2000,
};
const panel: React.CSSProperties = {
  background: "#1c2130",
  border: "1px solid #38445e",
  borderRadius: 10,
  padding: 20,
  width: 520,
  maxHeight: "85vh",
  overflowY: "auto",
  boxShadow: "0 8px 32px #000000aa",
  fontFamily: "sans-serif",
  color: "#d6dded",
  fontSize: 13,
  lineHeight: 1.6,
};
const row: React.CSSProperties = { display: "flex", alignItems: "center", gap: 8, marginBottom: 8 };
const lab: React.CSSProperties = { width: 84, color: "#9aa3b8", flexShrink: 0 };
const inp: React.CSSProperties = {
  flex: 1,
  background: "#0f1420",
  color: "#d6dded",
  border: "1px solid #38445e",
  borderRadius: 6,
  padding: "6px 8px",
  fontSize: 12,
  minWidth: 0,
  boxSizing: "border-box",
};
// 数値セル（ラベル上・入力下）。flex:1 + minWidth:0 で等幅に縮み、横あふれしない。
const cellRow: React.CSSProperties = { display: "flex", gap: 8, marginBottom: 8 };
const cell: React.CSSProperties = { display: "flex", flexDirection: "column", gap: 3, flex: 1, minWidth: 0 };
const cellLab: React.CSSProperties = { fontSize: 11, color: "#9aa3b8" };
const cellInp: React.CSSProperties = {
  background: "#0f1420",
  color: "#d6dded",
  border: "1px solid #38445e",
  borderRadius: 6,
  padding: "6px 8px",
  fontSize: 12,
  width: "100%",
  minWidth: 0,
  boxSizing: "border-box",
};
const note: React.CSSProperties = { fontSize: 11, color: "#9aa3b8" };
const btn: React.CSSProperties = {
  background: "#3a6ea5",
  color: "#fff",
  border: "none",
  borderRadius: 6,
  padding: "6px 14px",
  cursor: "pointer",
  fontSize: 12,
};
const btnGhost: React.CSSProperties = {
  background: "#2a3145",
  color: "#d6dded",
  border: "1px solid #3a4660",
  borderRadius: 6,
  padding: "6px 12px",
  cursor: "pointer",
  fontSize: 12,
};
const linkBtn: React.CSSProperties = {
  background: "none",
  border: "none",
  color: "#7fb0d8",
  cursor: "pointer",
  fontSize: 11,
  textDecoration: "underline",
  padding: 0,
  marginLeft: 4,
};
