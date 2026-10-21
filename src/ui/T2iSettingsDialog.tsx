import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { ComfyT2iForm } from "./ComfyT2iForm";

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
  // モディファイア（SD/SDXL 中心）。空は送らない＝モデル既定のまま。
  lora?: string;
  vae?: string;
  sampler?: string;
  clip_skip?: number;
  timeout_secs?: number;
  accept_mime?: string;
  output_ext?: string;
  /** v1.4.17 インラインパネルの追加プロンプト。編集はパネル側（ここではパススルー保持のみ）。 */
  prompt?: string;
  /** 生成エンジン。未指定＝内蔵 diffusers デーモン、"comfyui"＝外部 ComfyUI（ComfyT2iForm）。 */
  backend?: string;
}

// サンプラー選択肢（t2i_server.py の SAMPLERS と対応）。空＝モデル既定(DPM++)。
const SAMPLERS = [
  "dpm++_2m", "dpm++_2m_karras", "dpm++_sde_karras",
  "euler_a", "euler", "unipc", "ddim", "heun", "lms",
];

interface Props {
  initialLabel: string;
  initialSpec: string; // JSON（builtin:"t2i" を含む）
  /** 取得済みモデル名（DL済み）。先頭が既定候補。 */
  models: string[];
  /** 既知モデル名（DL候補。取得済みでなくても選択肢に出す）。 */
  known?: string[];
  /** モディファイア用アセット（home/loras・embeddings・vae の名前）。セレクトの候補。 */
  assets?: { loras: string[]; embeddings: string[]; vae: string[] };
  onCancel: () => void;
  onSave: (label: string, spec: string) => void;
  /** モデル取得などのセットアップを開く（未取得時の導線）。 */
  onOpenSetup?: () => void;
  /** 新規作成で最初から ComfyUI を選んでおく（セットアップの「ComfyUI で作る」から）。 */
  initialBackend?: "comfyui";
}

export function T2iSettingsDialog({ initialLabel, initialSpec, models, known = [], assets = { loras: [], embeddings: [], vae: [] }, onCancel, onSave, onOpenSetup, initialBackend }: Props) {
  const init: T2iSpec = (() => {
    try {
      return { ...(JSON.parse(initialSpec) as T2iSpec) };
    } catch {
      return { builtin: "t2i" };
    }
  })();

  const [label, setLabel] = useState(initialLabel);
  const [backend, setBackend] = useState(init.backend === "comfyui" || initialBackend === "comfyui" ? "comfyui" : "diffusers");
  const [model, setModel] = useState(init.model || models[0] || "sd1.5");
  const [negative, setNegative] = useState(init.negative ?? "");
  const [steps, setSteps] = useState(String(init.steps ?? 25));
  const [guidance, setGuidance] = useState(String(init.guidance ?? 7.5));
  const [width, setWidth] = useState(String(init.width ?? 512));
  const [height, setHeight] = useState(String(init.height ?? 512));
  const [seed, setSeed] = useState(init.seed === null || init.seed === undefined ? "" : String(init.seed));
  const [strength, setStrength] = useState(String(init.strength ?? 0.6));
  // モディファイア（SD/SDXL 中心）。フィルタ流儀＝手持ちアセットから「選ぶ」。埋め込みは自動ロード。
  // LoRA は "name:weight" の先頭1件を UI では「選択＋強度」に分解（複数指定はデーモンが受理可＝将来）。
  const initLora = (() => {
    const first = (init.lora ?? "").split(",")[0].trim();
    if (!first) return { name: "", weight: "0.8" };
    const i = first.lastIndexOf(":");
    if (i > 0) return { name: first.slice(0, i).trim(), weight: first.slice(i + 1).trim() || "0.8" };
    return { name: first, weight: "1.0" };
  })();
  const [loraName, setLoraName] = useState(initLora.name);
  const [loraWeight, setLoraWeight] = useState(initLora.weight);
  const [vae, setVae] = useState(init.vae ?? "");
  const [sampler, setSampler] = useState(init.sampler ?? "");
  const [clipSkip, setClipSkip] = useState(String(init.clip_skip ?? 1));
  // 制限時間（秒）。FLUX は初回モデルロードで数分かかることがあるので既定を長めに。
  const [timeoutSecs, setTimeoutSecs] = useState(String(init.timeout_secs ?? 600));

  // セレクト候補＝実在アセット ∪ 現在値（アセットが消えても保存済みの選択は選択肢に残す）。
  const loraOptions = Array.from(new Set([...assets.loras, ...(loraName ? [loraName] : [])]));
  const vaeOptions = Array.from(new Set([...assets.vae, ...(vae ? [vae] : [])]));

  // v1.5.13: モデル/LoRA/VAE の種別（sd/sdxl/flux）を診断し、不適合を薄く警告（厳密ではない目安）。
  const [insp, setInsp] = useState<{
    models: { name: string; kind: string }[];
    loras: { name: string; kind: string }[];
    vae: { name: string; kind: string }[];
  } | null>(null);
  useEffect(() => {
    void invoke<typeof insp>("t2i_inspect").then(setInsp).catch(() => setInsp(null));
  }, []);
  // モデルの系統。診断に無い名前（未取得の既知モデル等）は名前から目安推定。
  const familyOf = (name: string): string => {
    const found = insp?.models.find((m) => m.name === name);
    if (found && found.kind !== "unknown") return found.kind;
    const n = name.toLowerCase();
    if (n.includes("flux")) return "flux";
    if (n.includes("xl")) return "sdxl";
    return "sd";
  };
  const modelFamily = familyOf(model);
  const loraKind = insp?.loras.find((l) => l.name === loraName)?.kind;
  const knownFamily = (k?: string) => k === "sd" || k === "sdxl" || k === "flux";
  // LoRA はモデルと同系統でのみ有効（SD1.5↔SD1.5 / SDXL↔SDXL / FLUX↔FLUX）。
  // 双方の系統が確定しているときだけ警告（"unknown" では誤警告しない）。
  const loraMismatch =
    !!loraName && knownFamily(loraKind) && knownFamily(modelFamily) && loraKind !== modelFamily;
  // VAE（外部）は SD/SDXL 用。FLUX は内蔵 VAE 推奨。
  const vaeOnFlux = !!vae && modelFamily === "flux";
  const kindJp = (k: string) => ({ sd: "SD1.5系", sdxl: "SDXL", flux: "FLUX" } as Record<string, string>)[k] || k;

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
      // LoRA は「選択 → name:weight」で送る（未選択なら送らない）。
      lora: loraName ? `${loraName}:${parseFloat(loraWeight) || 1.0}` : undefined,
      vae: vae.trim() || undefined,
      sampler: sampler || undefined,
      // Clip skip は A1111 流。1（スキップなし）は既定なので送らない。
      clip_skip: (() => {
        const n = parseInt(clipSkip, 10);
        return !n || n <= 1 ? undefined : n;
      })(),
      timeout_secs: isNaN(to) ? undefined : to,
      // */* ＝ テキスト（プロンプト）に加えて画像（img2img の元画像）も接続できる。
      accept_mime: "*/*",
      output_ext: "png",
      // インラインパネルが保存した追加プロンプトを消さない（編集はパネル側）。
      prompt: init.prompt || undefined,
    };
    // v1.8.7 途中出力スイッチ（右クリックで切り替える）はこのダイアログの外の設定なので温存。
    if ((init as unknown as Record<string, unknown>).emit_mid === "on") (spec as unknown as Record<string, unknown>).emit_mid = "on";
    onSave(label.trim() || "画像生成", JSON.stringify(spec));
  };

  if (backend === "comfyui") {
    return (
      <div style={overlay} onMouseDown={onCancel}>
        <div onMouseDown={(e) => e.stopPropagation()} style={{ ...panel, width: 620 }}>
          <div style={{ fontSize: 15, marginBottom: 4, textAlign: "center" }}>画像生成（ComfyUI）の設定</div>
          <div style={{ fontSize: 11, color: "var(--panel-muted, #8a93a8)", marginBottom: 14, textAlign: "center" }}>
            接続したテキスト（プロンプト）・画像 → ComfyUI のワークフロー → 画像（PNG実身）。
          </div>
          <ComfyT2iForm
            init={init as unknown as Record<string, unknown>}
            label={label}
            setLabel={setLabel}
            onCancel={onCancel}
            onSave={onSave}
            onSwitchBackend={() => setBackend("diffusers")}
            styles={{ row, lab, inp, cell, cellRow, cellLab, cellInp, note, btn, btnGhost, linkBtn }}
          />
        </div>
      </div>
    );
  }

  return (
    <div style={overlay} onMouseDown={onCancel}>
      <div onMouseDown={(e) => e.stopPropagation()} style={panel}>
        <div style={{ fontSize: 15, marginBottom: 4, textAlign: "center" }}>
          画像生成（Text-To-Image）の設定
        </div>
        <div style={{ fontSize: 11, color: "var(--panel-muted, #8a93a8)", marginBottom: 14, textAlign: "center" }}>
          接続したテキストカード（プロンプト）→ 画像（PNG実身）。プロンプト本体は実行時に渡ります。
        </div>

        <label style={row}>
          <span style={lab}>表示名</span>
          <input style={inp} value={label} onChange={(e) => setLabel(e.target.value)} />
        </label>

        <label style={row}>
          <span style={lab}>生成エンジン</span>
          <select style={inp} value="diffusers" onChange={(e) => e.target.value === "comfyui" && setBackend("comfyui")}>
            <option value="diffusers">TexoS 内蔵（diffusers デーモン）</option>
            <option value="comfyui">ComfyUI（外部サーバ）</option>
          </select>
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
              placeholder="空欄=無制限"
              onChange={(e) => setTimeoutSecs(e.target.value)}
            />
          </label>
        </div>
        <div style={{ ...note, marginBottom: 4 }}>
          制限時間: 生成がこれを超えると中断（GPUも解放）。FLUXは初回ロードで数分かかるため長め推奨・空欄／0=無制限（中断ボタンで停止）。
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

        {/* --- モディファイア（SD/SDXL 中心）。フィルタ流儀＝手持ちアセットから「選ぶ」。--- */}
        <div style={{ fontSize: 12, color: "var(--panel-muted, #8a93a8)", margin: "12px 0 6px", borderTop: "1px solid var(--panel-border, #2a3145)", paddingTop: 10 }}>
          拡張（SD/SDXL）
        </div>

        {/* LoRA は「選択（何を）＋強度（どれだけ）」のセット。候補は home/loras の実在ファイル。 */}
        <div style={cellRow}>
          <label style={{ ...cell, flex: 2 }}>
            <span style={cellLab}>LoRA</span>
            <select style={cellInp} value={loraName} onChange={(e) => setLoraName(e.target.value)}>
              <option value="">なし</option>
              {loraOptions.map((n) => (
                <option key={n} value={n}>
                  {assets.loras.includes(n) ? "✓ " : "　"}
                  {n}
                </option>
              ))}
            </select>
          </label>
          <label style={cell}>
            <span style={cellLab}>強度</span>
            <input
              style={cellInp}
              value={loraWeight}
              placeholder="0.8"
              disabled={!loraName}
              onChange={(e) => setLoraWeight(e.target.value)}
            />
          </label>
        </div>

        <div style={cellRow}>
          <label style={cell}>
            <span style={cellLab}>サンプラー</span>
            <select style={cellInp} value={sampler} onChange={(e) => setSampler(e.target.value)}>
              <option value="">既定(DPM++)</option>
              {SAMPLERS.map((s) => (
                <option key={s} value={s}>
                  {s}
                </option>
              ))}
            </select>
          </label>
          <label style={cell}>
            <span style={cellLab}>Clip skip</span>
            <input style={cellInp} value={clipSkip} placeholder="1" onChange={(e) => setClipSkip(e.target.value)} />
          </label>
          <label style={cell}>
            <span style={cellLab}>VAE</span>
            <select style={cellInp} value={vae} onChange={(e) => setVae(e.target.value)}>
              <option value="">既定</option>
              {vaeOptions.map((n) => (
                <option key={n} value={n}>
                  {assets.vae.includes(n) ? "✓ " : "　"}
                  {n}
                </option>
              ))}
            </select>
          </label>
        </div>
        <div style={{ ...note, marginBottom: 4 }}>
          LoRA/VAE は home/loras・home/vae に置いたファイルから選択（無ければ空）。ネガ埋め込み(Textual Inversion)は
          home/embeddings に置けば、その名前をプロンプト/負プロンプトに書くだけで有効
          {assets.embeddings.length > 0 && <>（利用可: {assets.embeddings.join(", ")}）</>}。
          Clip skip はアニメ系 SD1.5 で 2 が定番。※SD/SDXL 向け。
        </div>

        {/* v1.5.13: 種別不一致の軽い警告（保存は妨げない＝目安）。 */}
        {loraMismatch && (
          <div style={{ ...note, color: "#d9a06a", marginBottom: 4 }}>
            ⚠ LoRA「{loraName}」は{kindJp(loraKind!)}用ですが、モデルは{kindJp(modelFamily)}系です。
            系統が違う LoRA は効かない／崩れることがあります。
          </div>
        )}
        {vaeOnFlux && (
          <div style={{ ...note, color: "#d9a06a", marginBottom: 4 }}>
            ⚠ FLUX は内蔵 VAE を使うため、外部 VAE「{vae}」は無視されます。
          </div>
        )}

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
// 2026-07-06: テーマ追従（--panel-* 体系）。暗色固定（2026-06-26決定）は改訂。
const panel: React.CSSProperties = {
  background: "var(--panel-bg, #1c2130)",
  border: "1px solid var(--panel-border, #38445e)",
  borderRadius: 10,
  padding: 20,
  width: 520,
  maxHeight: "85vh",
  overflowY: "auto",
  boxShadow: "0 8px 32px #000000aa",
  fontFamily: "sans-serif",
  color: "var(--panel-text, #d6dded)",
  fontSize: 13,
  lineHeight: 1.6,
};
const row: React.CSSProperties = { display: "flex", alignItems: "center", gap: 8, marginBottom: 8 };
const lab: React.CSSProperties = { width: 84, color: "var(--panel-muted, #9aa3b8)", flexShrink: 0 };
const inp: React.CSSProperties = {
  flex: 1,
  background: "var(--panel-input-bg, #0f1420)",
  color: "var(--panel-text, #d6dded)",
  border: "1px solid var(--panel-border, #38445e)",
  borderRadius: 6,
  padding: "6px 8px",
  fontSize: 12,
  minWidth: 0,
  boxSizing: "border-box",
};
// 数値セル（ラベル上・入力下）。flex:1 + minWidth:0 で等幅に縮み、横あふれしない。
const cellRow: React.CSSProperties = { display: "flex", gap: 8, marginBottom: 8 };
const cell: React.CSSProperties = { display: "flex", flexDirection: "column", gap: 3, flex: 1, minWidth: 0 };
const cellLab: React.CSSProperties = { fontSize: 11, color: "var(--panel-muted, #9aa3b8)" };
const cellInp: React.CSSProperties = {
  background: "var(--panel-input-bg, #0f1420)",
  color: "var(--panel-text, #d6dded)",
  border: "1px solid var(--panel-border, #38445e)",
  borderRadius: 6,
  padding: "6px 8px",
  fontSize: 12,
  width: "100%",
  minWidth: 0,
  boxSizing: "border-box",
};
const note: React.CSSProperties = { fontSize: 11, color: "var(--panel-muted, #9aa3b8)" };
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
