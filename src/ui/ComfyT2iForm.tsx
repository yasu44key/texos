import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

// t2i カードの ComfyUI バックエンド設定（docs/ComfyUIバックエンド設計.md Phase 1）。
// カードが持つのは「API 形式ワークフロー JSON」＋「差し込み口（slot → ノード/入力）」＋出力ノード。
// 取り込み時に Rust（comfy_analyze）が口を自動検出し、/object_info でノード名・入力名・モデルを検証する。

export interface ComfyBinding {
  slot: string;
  node: string;
  input: string;
  index?: number;
}

interface Analysis {
  workflow: Record<string, unknown>;
  bindings: ComfyBinding[];
  output: string | null;
  nodes: [string, string, string][]; // [id, class_type, title]
  errors: string[];
  notes: string[];
}

interface Check {
  url: string;
  version: string;
  devices: [string, number, number][];
}

const SLOTS: { key: string; label: string }[] = [
  { key: "prompt", label: "プロンプト" },
  { key: "negative", label: "負プロンプト" },
  { key: "seed", label: "シード" },
  { key: "steps", label: "ステップ" },
  { key: "guidance", label: "ガイダンス(cfg)" },
  { key: "width", label: "幅" },
  { key: "height", label: "高さ" },
  { key: "strength", label: "変化度(denoise)" },
  { key: "image", label: "入力画像" },
];
const slotLabel = (k: string) => SLOTS.find((s) => s.key === k)?.label ?? k;

interface Props {
  init: Record<string, unknown>;
  label: string;
  setLabel: (s: string) => void;
  onCancel: () => void;
  onSave: (label: string, spec: string) => void;
  /** 自前（diffusers）へ切り替える。 */
  onSwitchBackend: () => void;
  styles: Record<string, React.CSSProperties>;
}

export function ComfyT2iForm({ init, label, setLabel, onCancel, onSave, onSwitchBackend, styles: S }: Props) {
  const wasComfy = init.backend === "comfyui";
  const numStr = (k: string) => (wasComfy && init[k] !== undefined && init[k] !== null ? String(init[k]) : "");
  const [endpoint, setEndpoint] = useState(wasComfy ? String(init.endpoint ?? "") : "");
  const [wfText, setWfText] = useState(
    init.comfy_workflow ? JSON.stringify(init.comfy_workflow, null, 1) : "",
  );
  const [analysis, setAnalysis] = useState<Analysis | null>(null);
  const [bindings, setBindings] = useState<ComfyBinding[]>((init.comfy_bindings as ComfyBinding[]) ?? []);
  const [output, setOutput] = useState(String(init.comfy_output ?? ""));
  const [negative, setNegative] = useState(wasComfy ? String(init.negative ?? "") : "");
  const [steps, setSteps] = useState(numStr("steps"));
  const [guidance, setGuidance] = useState(numStr("guidance"));
  const [width, setWidth] = useState(numStr("width"));
  const [height, setHeight] = useState(numStr("height"));
  const [seed, setSeed] = useState(numStr("seed"));
  const [strength, setStrength] = useState(numStr("strength"));
  const [timeoutSecs, setTimeoutSecs] = useState(String(init.timeout_secs ?? 900));
  const [busy, setBusy] = useState<"" | "check" | "analyze">("");
  const [check, setCheck] = useState<Check | null>(null);
  const [checkErr, setCheckErr] = useState("");
  const [err, setErr] = useState("");

  const runCheck = async () => {
    setBusy("check");
    setCheckErr("");
    try {
      setCheck(await invoke<Check>("comfy_check", { endpoint }));
    } catch (e) {
      setCheck(null);
      setCheckErr(String(e));
    } finally {
      setBusy("");
    }
  };

  // keep=true: 今の口と出力を検証だけする（編集時）。false: 取り込み直して自動検出。
  const analyze = async (keep: boolean) => {
    if (!wfText.trim()) return;
    setBusy("analyze");
    setErr("");
    try {
      const a = await invoke<Analysis>("comfy_analyze", {
        endpoint,
        workflow: wfText,
        bindings: keep ? bindings : null,
        output: keep ? output || null : null,
      });
      setAnalysis(a);
      setBindings(a.bindings);
      setOutput(a.output ?? "");
      if (!keep) setWfText(JSON.stringify(a.workflow, null, 1));
    } catch (e) {
      setAnalysis(null);
      setErr(String(e));
    } finally {
      setBusy("");
    }
  };

  useEffect(() => {
    void runCheck();
    if (wfText.trim() && bindings.length > 0) void analyze(true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const has = (slot: string) => bindings.some((b) => b.slot === slot);
  const nodes = analysis?.nodes ?? [];
  const nodeName = (id: string) => {
    const n = nodes.find((x) => x[0] === id);
    return n ? `${id}  ${n[2] || n[1]}` : id;
  };
  const updateB = (i: number, patch: Partial<ComfyBinding>) =>
    setBindings(bindings.map((b, j) => (j === i ? { ...b, ...patch } : b)));

  const num = (s: string, f: (x: string) => number) => {
    if (s.trim() === "") return undefined;
    const n = f(s);
    return isNaN(n) ? undefined : n;
  };

  const save = () => {
    let workflow: unknown;
    try {
      workflow = analysis?.workflow ?? JSON.parse(wfText);
    } catch {
      setErr("ワークフローが JSON として読めません。「取り込み・解析」を押してください");
      return;
    }
    if (!wfText.trim()) {
      setErr("ワークフロー（API 形式 JSON）を貼ってください");
      return;
    }
    const to = parseInt(timeoutSecs, 10);
    const spec: Record<string, unknown> = {
      builtin: "t2i",
      backend: "comfyui",
      endpoint: endpoint.trim() || undefined,
      comfy_workflow: workflow,
      comfy_bindings: bindings,
      comfy_output: output || undefined,
      // 空欄＝ワークフローに入っている値のまま（差し込まない）。
      negative: negative.trim() || undefined,
      steps: num(steps, (x) => parseInt(x, 10)),
      guidance: num(guidance, parseFloat),
      width: num(width, (x) => parseInt(x, 10)),
      height: num(height, (x) => parseInt(x, 10)),
      seed: seed.trim() === "" ? null : parseInt(seed, 10),
      strength: num(strength, parseFloat),
      timeout_secs: isNaN(to) ? undefined : to,
      accept_mime: "*/*",
      output_ext: "png",
      prompt: init.prompt || undefined,
    };
    if (init.emit_mid === "on") spec.emit_mid = "on";
    onSave(label.trim() || "画像生成", JSON.stringify(spec));
  };

  // 口が無い欄は「ワークフローの値のまま」＝入力不可にして誤解を防ぐ。
  const field = (slot: string, title: string, value: string, set: (s: string) => void, ph = "") => (
    <label style={S.cell}>
      <span style={S.cellLab}>{title}</span>
      <input
        style={{ ...S.cellInp, opacity: has(slot) ? 1 : 0.45 }}
        value={value}
        disabled={!has(slot)}
        placeholder={has(slot) ? ph || "空=ワークフローの値" : "口なし"}
        onChange={(e) => set(e.target.value)}
      />
    </label>
  );

  return (
    <>
      <label style={S.row}>
        <span style={S.lab}>表示名</span>
        <input style={S.inp} value={label} onChange={(e) => setLabel(e.target.value)} />
      </label>
      <label style={S.row}>
        <span style={S.lab}>生成エンジン</span>
        <select style={S.inp} value="comfyui" onChange={(e) => e.target.value === "diffusers" && onSwitchBackend()}>
          <option value="diffusers">TexoS 内蔵（diffusers デーモン）</option>
          <option value="comfyui">ComfyUI（外部サーバ）</option>
        </select>
      </label>

      <div style={S.row}>
        <span style={S.lab}>接続先</span>
        <input
          style={S.inp}
          value={endpoint}
          placeholder="http://127.0.0.1:8188（空欄＝既定）"
          onChange={(e) => setEndpoint(e.target.value)}
        />
        <button style={S.btnGhost} disabled={busy !== ""} onClick={() => void runCheck()}>
          {busy === "check" ? "確認中…" : "接続テスト"}
        </button>
      </div>
      <div style={{ ...S.note, marginBottom: 8 }}>
        {check && (
          <span style={{ color: "#7fc89a" }}>
            ✓ ComfyUI {check.version}
            {check.devices.map(([n, total, free]) => ` ／ ${n.replace(/^cuda:\d+\s*/, "")} VRAM 空き ${(free / 1024).toFixed(1)}/${(total / 1024).toFixed(1)} GB`)}
          </span>
        )}
        {checkErr && <span style={{ color: "#d96a6a" }}>{checkErr}</span>}
      </div>

      <div style={{ ...S.cellLab, marginBottom: 3 }}>ワークフロー（API 形式 JSON）</div>
      <textarea
        style={{ ...S.inp, width: "100%", height: 90, fontFamily: "monospace", fontSize: 11, resize: "vertical" }}
        value={wfText}
        placeholder={'ComfyUI のメニュー「ワークフロー → エクスポート（API）」で書き出した JSON を貼る\n（UI 形式の保存ファイル・テンプレートはそのままでは使えません）'}
        onChange={(e) => {
          setWfText(e.target.value);
          setAnalysis(null);
        }}
      />
      <div style={{ display: "flex", gap: 8, alignItems: "center", margin: "4px 0 8px" }}>
        <button style={S.btn} disabled={busy !== "" || !wfText.trim()} onClick={() => void analyze(false)}>
          {busy === "analyze" ? "解析中…" : "取り込み・解析（口を自動検出）"}
        </button>
        {bindings.length > 0 && (
          <button style={S.btnGhost} disabled={busy !== ""} onClick={() => void analyze(true)}>
            今の設定で検証
          </button>
        )}
      </div>
      {err && <div style={{ ...S.note, color: "#d96a6a", marginBottom: 6, wordBreak: "break-all" }}>{err}</div>}
      {analysis?.errors.map((e, i) => (
        <div key={"e" + i} style={{ ...S.note, color: "#d96a6a" }}>✗ {e}</div>
      ))}
      {analysis?.notes.map((n, i) => (
        <div key={"n" + i} style={{ ...S.note, color: "#d9a06a" }}>・{n}</div>
      ))}
      {analysis && analysis.errors.length === 0 && (
        <div style={{ ...S.note, color: "#7fc89a" }}>✓ ノード {analysis.nodes.length} 個・ComfyUI 上で検証済み（モデル・入力名）</div>
      )}

      {(bindings.length > 0 || analysis) && (
        <>
          <div style={{ ...S.cellLab, margin: "10px 0 4px" }}>差し込み口（TexoS の値 → ワークフローのノード・入力）</div>
          {bindings.map((b, i) => (
            <div key={i} style={{ display: "flex", gap: 4, marginBottom: 4 }}>
              <select style={{ ...S.cellInp, width: 110, flex: "none" }} value={b.slot} onChange={(e) => updateB(i, { slot: e.target.value })}>
                {SLOTS.map((s) => (
                  <option key={s.key} value={s.key}>{s.label}</option>
                ))}
              </select>
              <select style={{ ...S.cellInp, flex: 2 }} value={b.node} onChange={(e) => updateB(i, { node: e.target.value })}>
                {!nodes.some((n) => n[0] === b.node) && <option value={b.node}>{b.node}</option>}
                {nodes.map((n) => (
                  <option key={n[0]} value={n[0]}>{nodeName(n[0])}</option>
                ))}
              </select>
              <input style={{ ...S.cellInp, flex: 1 }} value={b.input} onChange={(e) => updateB(i, { input: e.target.value })} />
              {b.slot === "image" && (
                <input
                  style={{ ...S.cellInp, width: 36, flex: "none" }}
                  title="何枚目の入力画像か（矢印を作った順）"
                  value={b.index ?? 1}
                  onChange={(e) => updateB(i, { index: parseInt(e.target.value, 10) || 1 })}
                />
              )}
              <button style={{ ...S.btnGhost, padding: "2px 8px" }} title="この口を外す" onClick={() => setBindings(bindings.filter((_, j) => j !== i))}>
                ×
              </button>
            </div>
          ))}
          <button
            style={{ ...S.linkBtn, marginLeft: 0 }}
            onClick={() => setBindings([...bindings, { slot: "prompt", node: nodes[0]?.[0] ?? "", input: "" }])}
          >
            ＋ 口を追加
          </button>
          <label style={{ ...S.row, marginTop: 6 }}>
            <span style={S.lab}>出力ノード</span>
            <select style={S.inp} value={output} onChange={(e) => setOutput(e.target.value)}>
              <option value="">（最初の画像）</option>
              {!nodes.some((n) => n[0] === output) && output && <option value={output}>{output}</option>}
              {nodes.map((n) => (
                <option key={n[0]} value={n[0]}>{nodeName(n[0])}</option>
              ))}
            </select>
          </label>
          <div style={{ ...S.note, marginBottom: 6 }}>
            口: {bindings.length ? bindings.map((b) => slotLabel(b.slot) + (b.slot === "image" ? `#${b.index ?? 1}` : "")).join("・") : "なし"}。
            プロンプトは接続したテキストカード（＋パネルの追加プロンプト）が入ります。画像カードは矢印を作った順に入力画像 #1, #2 … へ。
          </div>
        </>
      )}

      <div style={{ ...S.cellLab, margin: "10px 0 4px", borderTop: "1px solid var(--panel-border, #2a3145)", paddingTop: 8 }}>
        生成パラメータ（空欄＝ワークフローの値のまま）
      </div>
      <label style={S.row}>
        <span style={S.lab}>負プロンプト</span>
        <input
          style={{ ...S.inp, opacity: has("negative") ? 1 : 0.45 }}
          disabled={!has("negative")}
          value={negative}
          placeholder={has("negative") ? "空=ワークフローの値" : "口なし"}
          onChange={(e) => setNegative(e.target.value)}
        />
      </label>
      <div style={S.cellRow}>
        {field("steps", "ステップ", steps, setSteps)}
        {field("guidance", "ガイダンス", guidance, setGuidance)}
        {field("seed", "シード", seed, setSeed, "空=毎回ランダム")}
        {field("strength", "変化度", strength, setStrength)}
      </div>
      <div style={S.cellRow}>
        {field("width", "幅", width, setWidth)}
        {field("height", "高さ", height, setHeight)}
        <label style={S.cell}>
          <span style={S.cellLab}>制限時間(秒)</span>
          <input style={S.cellInp} value={timeoutSecs} placeholder="空欄=無制限" onChange={(e) => setTimeoutSecs(e.target.value)} />
        </label>
      </div>
      <div style={{ ...S.note, marginBottom: 4 }}>
        ComfyUI がこの PC にあるときは、実行前に TexoS 内蔵の生成デーモンを止めて GPU を空けます（逆に内蔵で生成するときは ComfyUI のモデルを下ろします）。
      </div>

      <div style={{ display: "flex", justifyContent: "flex-end", gap: 8, marginTop: 18 }}>
        <button style={S.btnGhost} onClick={onCancel}>
          キャンセル
        </button>
        <button style={S.btn} onClick={save}>
          保存
        </button>
      </div>
    </>
  );
}
