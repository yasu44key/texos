import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

// v1.5 Text-To-Image のセットアップ＆モデル管理ダイアログ（Whisper/Ollama と同じ流儀）。
// diffusers 常駐デーモン（t2i_server.py）を Python で起動し、モデルを共有 app-data
// （%APPDATA%\com.yoshi.texos\t2i\models）へダウンロードする。
// v1.4.27 WS2: Python は TexoS 管理 env（uv 自動構築）→ システム Python の順で解決。
// 未整備なら「環境を構築する」ボタンでワンクリック構築（進捗は "python-env" イベント）。

export interface T2iKnownModel {
  name: string;
  label: string;
  gated: boolean;
  hf_url: string;
}

// v1.4.28 WS3: PCスペック検出と推奨構成（detect_spec）。推奨の根拠をユーザーに見せる。
export interface GpuInfo {
  name: string;
  vram_mb: number;
}
export interface SpecInfo {
  gpu: GpuInfo | null;
  ram_mb: number;
  tier: string;
  torch_index: string;
  cuda: boolean;
  recommended_model: string;
  note: string;
  ollama_hint: string;
}

export interface T2iStatus {
  python_ok: boolean;
  python_path: string;
  deps_ok: boolean;
  /** Python の解決元: "managed"（TexoS 構築 env）| "system" | "none" */
  python_kind: string;
  /** 明示切替の設定値: "auto" | "managed" | "system" */
  env_mode: string;
  server_running: boolean;
  gpu: boolean;
  models: string[];
  known: T2iKnownModel[];
  current_model: string;
  hf_token_set: boolean;
  partial: string[];
  // モディファイア用アセット（home/loras・embeddings・vae の名前一覧）。設定UIのセレクト用。
  loras: string[];
  embeddings: string[];
  vae: string[];
}

interface Props {
  onCancel: () => void;
  /** 準備完了（依存OK＋モデル1つ以上＋サーバー稼働）で押せる。最新 status を渡す。 */
  onReady: (status: T2iStatus) => void;
}

export function T2iSetupDialog({ onCancel, onReady }: Props) {
  const [st, setSt] = useState<T2iStatus | null>(null);
  const [spec, setSpec] = useState<SpecInfo | null>(null);
  const [model, setModel] = useState("sd1.5");
  const [busy, setBusy] = useState<null | "server" | "model" | "token" | "env">(null);
  const [progress, setProgress] = useState("");
  const [error, setError] = useState("");
  const [token, setToken] = useState("");
  const [copied, setCopied] = useState(false);
  const [showToken, setShowToken] = useState(false); // 設定済みでも再設定パネルを開く

  const reload = useCallback(async () => {
    try {
      const s = await invoke<T2iStatus>("t2i_status");
      setSt(s);
      setModel((m) => (s.models.includes(m) ? m : s.current_model || s.known[0]?.name || "sd1.5"));
    } catch (e) {
      setError(String(e));
    }
  }, []);

  // PCスペック（GPU/VRAM/RAM）と推奨構成を（再）検出。ドライバ／グラボ入替後に押し直せる。
  const refreshSpec = useCallback(async () => {
    try {
      setSpec(await invoke<SpecInfo>("detect_spec"));
    } catch {
      setSpec(null);
    }
  }, []);

  useEffect(() => {
    void reload();
    void refreshSpec();
  }, [reload, refreshSpec]);

  // ダウンロード進捗（t2i-model）と環境構築進捗（python-env）を購読。
  useEffect(() => {
    const uns: Array<() => void> = [];
    void listen<{ model?: string; status?: string; percent?: number }>("t2i-model", (e) => {
      setProgress(`${e.payload.status ?? "取得中"} ${e.payload.percent ?? 0}%`);
    }).then((u) => uns.push(u));
    void listen<{ env?: string; status?: string; percent?: number }>("python-env", (e) => {
      setProgress(`${e.payload.status ?? "構築中"}（全体 ${e.payload.percent ?? 0}%）`);
    }).then((u) => uns.push(u));
    return () => uns.forEach((u) => u());
  }, []);

  // Python 環境のワンクリック構築（uv → CPython → venv → torch/diffusers）。
  // rebuild=true は既存 env を消して作り直す（GPU 入替後などに CPU 版→GPU 版 torch へ切替）。
  const buildEnv = async (rebuild = false) => {
    setBusy("env");
    setError("");
    setProgress(
      rebuild
        ? "Python 実行環境を再構築中…（GPU 版 torch を入れ直します。数GBのダウンロードがあります）"
        : "Python 実行環境を構築中…（初回は数GBのダウンロードがあります）"
    );
    try {
      // 稼働中デーモンは古い torch を掴んだままなので、再構築時は先に止めて反映させる。
      if (rebuild && st?.server_running) {
        await invoke("t2i_stop_server").catch(() => {});
      }
      await invoke("python_build_env", { env: "t2i", force: rebuild });
      await Promise.all([reload(), refreshSpec()]);
      setProgress(
        rebuild
          ? "GPU 版で再構築しました。次の生成から反映されます。"
          : "Python 実行環境を構築しました。"
      );
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  // 実行環境の明示切替（auto / managed / system）。
  const setEnvMode = async (mode: string) => {
    setError("");
    try {
      await invoke("python_set_env_mode", { mode });
      await reload();
    } catch (e) {
      setError(String(e));
    }
  };

  const startServer = async () => {
    setBusy("server");
    setError("");
    setProgress("画像生成サーバーを起動中…（初回はモデル読込で時間がかかります）");
    try {
      await invoke("t2i_start_server");
      await reload();
      setProgress("サーバーを起動しました。");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  const stopServer = async () => {
    setBusy("server");
    setError("");
    setProgress("生成サーバーを停止中…（VRAM を解放）");
    try {
      await invoke("t2i_stop_server");
      await reload();
      setProgress("サーバーを停止しました。");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  const downloadModel = async () => {
    setBusy("model");
    setError("");
    setProgress(`モデル ${model} を取得中…`);
    try {
      await invoke("t2i_download_model", { model });
      await reload();
      setProgress(`モデル ${model} を取得しました。`);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  const saveToken = async () => {
    setBusy("token");
    setError("");
    setProgress("HF トークンを保存してサーバーを再起動中…");
    try {
      await invoke("t2i_set_hf_token", { token });
      setToken("");
      setShowToken(false);
      await reload();
      setProgress("HF トークンを保存しました。ゲート付きモデルを取得できます。");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  const openFolder = async () => {
    try {
      await invoke<string>("t2i_reveal_model_dir", { model });
    } catch (e) {
      setError(String(e));
    }
  };

  const discardModel = async () => {
    setBusy("model");
    setError("");
    setProgress(`${model} のローカルデータを破棄中…`);
    try {
      await invoke("t2i_delete_model", { model });
      await reload();
      setProgress(`${model} を破棄しました。もう一度ダウンロードできます。`);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  const selectedDownloaded = !!st?.models.includes(model);
  const isPartial = !!st?.partial.includes(model);
  const selKnown = st?.known.find((k) => k.name === model);
  const isGated = !!selKnown?.gated;
  // ゲート付き（FLUX 公式）でトークン未設定なら、まずトークン入力を促す。
  const needToken = isGated && !st?.hf_token_set;
  // 設定済みでも「再設定」で開ける。
  const tokenPanelOpen = isGated && !selectedDownloaded && (needToken || showToken);
  const ready = !!st && st.deps_ok && st.models.length > 0;
  // GPU を検出したのに、構築済み環境が CPU 版 torch のまま（GPU 入替後などの不一致）。
  // このとき「環境を GPU 版で再構築する」導線を出す（再構築で cu124 版 torch に入れ直す）。
  const gpuButCpuEnv = !!spec?.gpu && !!spec?.cuda && !!st?.deps_ok && !st?.gpu;

  const dot = (ok: boolean) => (
    <span style={{ color: ok ? "#5fbf77" : "#d96a6a", marginRight: 6 }}>{ok ? "✓" : "✗"}</span>
  );

  return (
    <div style={overlay} onMouseDown={onCancel}>
      <div onMouseDown={(e) => e.stopPropagation()} style={panel}>
        <div style={{ fontSize: 15, marginBottom: 4, textAlign: "center" }}>
          画像生成（Text-To-Image）のセットアップ
        </div>
        <div style={{ fontSize: 11, color: "var(--panel-muted, #8a93a8)", marginBottom: 14, textAlign: "center" }}>
          TexoS 管理の Python 環境（自動構築）の diffusers でローカル生成（Ollama 流の常駐デーモン）。
          モデルは PC に保存。
        </div>

        {/* この PC の構成と推奨（WS3）。推奨の根拠をユーザーが見られるように。 */}
        {spec && (
          <div style={{ ...sec, background: "#5fbf7714", borderColor: "#5fbf7750" }}>
            <div style={secTitle}>🖥️ この PC の構成 — {spec.tier}</div>
            <div style={note}>
              GPU: {spec.gpu ? `${spec.gpu.name}（VRAM ${(spec.gpu.vram_mb / 1024).toFixed(1)}GB）` : "対応 GPU 未検出（CPU 動作）"}
              {spec.ram_mb > 0 && <> ／ RAM: {(spec.ram_mb / 1024).toFixed(1)}GB</>}
            </div>
            <div style={{ ...note, color: "#5fbf77", marginTop: 4 }}>
              推奨: {spec.cuda ? "GPU 版 torch（cu124）" : "CPU 版 torch"} ／ 既定モデル {spec.recommended_model}
            </div>
            <div style={{ ...note, marginTop: 4 }}>{spec.note}</div>
            <button
              style={{ ...btnGhost, marginTop: 8, fontSize: 11, padding: "4px 10px" }}
              disabled={busy !== null}
              onClick={() => void refreshSpec()}
              title="ドライバやグラフィックボードを入れ替えたあとに押すと再検出します"
            >
              🔄 スペックを再検出
            </button>
          </div>
        )}

        {!st && <div>状態を確認中…</div>}

        {st && (
          <>
            {/* 1. Python + 依存（WS2: 管理 env → システムの順で解決。未整備ならワンクリック構築） */}
            <div style={sec}>
              <div style={secTitle}>{dot(st.python_ok && st.deps_ok)} Python ＋ 生成ライブラリ</div>
              <div style={{ display: "flex", gap: 8, alignItems: "center", marginBottom: 6, flexWrap: "wrap" }}>
                <span style={note}>実行環境:</span>
                <select
                  value={st.env_mode}
                  onChange={(e) => void setEnvMode(e.target.value)}
                  style={{ ...select, flex: "none", minWidth: 170 }}
                  disabled={busy !== null}
                  title="自動: TexoS 管理環境 → システム Python の順で解決"
                >
                  <option value="auto">自動（推奨）</option>
                  <option value="managed">TexoS 管理環境</option>
                  <option value="system">システム Python</option>
                </select>
                {st.python_ok && st.deps_ok && (
                  <span style={note}>
                    使用中: {st.python_kind === "managed" ? "TexoS 管理環境" : "システム Python"}
                  </span>
                )}
              </div>
              {st.python_ok && st.deps_ok ? (
                <>
                  <div style={note}>
                    Python: {st.python_path || "(検出)"}
                    <br />
                    torch / diffusers: 利用可能{st.gpu ? "（GPU 有効）" : "（CPU・低速）"}
                  </div>
                  {/* GPU 検出済みなのに環境が CPU 版 torch → 再構築で GPU 版に切り替える導線。 */}
                  {gpuButCpuEnv && (
                    <div
                      style={{
                        marginTop: 8,
                        border: "1px solid #5fbf7750",
                        borderRadius: 6,
                        padding: "8px 10px",
                        background: "#5fbf7714",
                      }}
                    >
                      <div style={{ ...note, color: "#5fbf77", marginBottom: 6, lineHeight: 1.6 }}>
                        ✨ <b>GPU（{spec?.gpu?.name}）が使えるようになりました。</b>
                        <br />
                        今の環境は CPU 版で構築されているため、GPU 版に再構築すると画像生成が
                        大幅に速くなります。下のボタンで切り替えることをお勧めします
                        <span style={{ color: "#d9a06a" }}>（再構築には時間がかかります：GPU 版 torch を数GB入れ直します）</span>。
                      </div>
                      {st.python_kind === "system" && (
                        <div style={{ ...note, color: "#d9a06a", marginBottom: 6 }}>
                          ※ 現在はシステム Python を使用中です。再構築後は上の「実行環境」を
                          「TexoS 管理環境」にすると GPU 版が使われます。
                        </div>
                      )}
                      <button style={btn} disabled={busy !== null} onClick={() => void buildEnv(true)}>
                        {busy === "env" ? "再構築中…" : "GPU 版で環境を再構築する"}
                      </button>
                    </div>
                  )}
                </>
              ) : (
                <div>
                  <div style={{ ...note, color: "#d9a06a", marginBottom: 6 }}>
                    {st.python_ok
                      ? "生成ライブラリ（torch / diffusers）が未導入です。"
                      : "使える Python 環境がありません。"}
                    「環境を構築する」で TexoS が専用の Python 環境を自動構築します
                    （uv 使用・torch など数GBのダウンロード。既存の Python には触れません）。
                  </div>
                  <button style={btn} disabled={busy !== null} onClick={() => void buildEnv()}>
                    {busy === "env" ? "構築中…" : "環境を構築する"}
                  </button>
                </div>
              )}
            </div>

            {/* 2. モデル */}
            <div style={sec}>
              <div style={secTitle}>{dot(st.models.length > 0)} モデル（共有 app-data に保存）</div>
              <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap" }}>
                <select value={model} onChange={(e) => setModel(e.target.value)} style={select}>
                  {st.known.map((m) => (
                    <option key={m.name} value={m.name}>
                      {st.models.includes(m.name) ? "✓ " : "　"}
                      {m.label}
                    </option>
                  ))}
                  {st.models
                    .filter((m) => !st.known.some((k) => k.name === m))
                    .map((m) => (
                      <option key={m} value={m}>
                        ✓ {m}
                      </option>
                    ))}
                </select>
                {selectedDownloaded ? (
                  <>
                    <span style={{ color: "#5fbf77" }}>取得済み</span>
                    <button
                      style={btnGhost}
                      disabled={busy !== null || !st.deps_ok}
                      onClick={() => void downloadModel()}
                      title="不足ファイルがあれば取り直します"
                    >
                      再取得
                    </button>
                  </>
                ) : isPartial ? (
                  // 中断/失敗で不完全: 続きから再取得（HFが自動レジューム）／破棄して取り直す。
                  <>
                    <span style={{ color: "#d9a06a" }}>⚠ 不完全</span>
                    <button
                      style={btn}
                      disabled={busy !== null || !st.deps_ok}
                      onClick={() => void downloadModel()}
                      title="中断した続きから取得します（HFが自動で再開）"
                    >
                      続きから再取得
                    </button>
                    <button
                      style={btnGhost}
                      disabled={busy !== null}
                      onClick={() => void discardModel()}
                      title="壊れている場合に全削除してやり直す"
                    >
                      破棄して取り直す
                    </button>
                  </>
                ) : isGated ? (
                  // ゲート付き: トークン設定済みなら DL ボタン＋「トークンを再設定」、未設定なら下のパネルで入力。
                  <>
                    {st.hf_token_set ? (
                      <>
                        <button
                          style={btn}
                          disabled={busy !== null || !st.deps_ok}
                          onClick={() => void downloadModel()}
                        >
                          ダウンロード
                        </button>
                        <span style={{ ...note, color: "#5fbf77" }}>🔑 トークン設定済み</span>
                        <button style={btnGhost} onClick={() => setShowToken((v) => !v)}>
                          {showToken ? "閉じる" : "トークンを再設定"}
                        </button>
                      </>
                    ) : (
                      <span style={{ ...note, color: "#d9a06a" }}>↓ HF トークンが必要です</span>
                    )}
                  </>
                ) : (
                  <button
                    style={btn}
                    disabled={busy !== null || !st.deps_ok}
                    onClick={() => void downloadModel()}
                  >
                    ダウンロード
                  </button>
                )}
              </div>

              {/* ゲート付きモデル: トークン入力（未設定で自動表示／設定済みでも「再設定」で開く・URLコピー可） */}
              {tokenPanelOpen && (
                <div style={{ marginTop: 10, border: "1px solid #d9a06a50", borderRadius: 6, padding: "8px 10px", background: "#d9a06a14" }}>
                  <div style={{ ...note, color: "#d9a06a", marginBottom: 6 }}>
                    このモデルは Hugging Face の認証が必要です。①下記ページでライセンスに同意 →
                    ②アクセストークンを発行して貼り付けてください。
                  </div>
                  <div style={{ display: "flex", gap: 6, alignItems: "center", marginBottom: 6 }}>
                    <input style={{ ...select, flex: 1 }} readOnly value={selKnown?.hf_url ?? ""} onFocus={(e) => e.currentTarget.select()} />
                    <button
                      style={btnGhost}
                      onClick={() => {
                        void navigator.clipboard.writeText(selKnown?.hf_url ?? "");
                        setCopied(true);
                        window.setTimeout(() => setCopied(false), 1500);
                      }}
                    >
                      {copied ? "コピー済" : "URLをコピー"}
                    </button>
                  </div>
                  <div style={{ ...note, marginBottom: 6 }}>
                    トークン発行: <span style={{ color: "#7fb0d8" }}>https://huggingface.co/settings/tokens</span>（read 権限でOK）
                  </div>
                  <div style={{ display: "flex", gap: 6, alignItems: "center" }}>
                    <input
                      style={{ ...select, flex: 1 }}
                      type="password"
                      placeholder="hf_xxx... トークンを貼り付け"
                      value={token}
                      onChange={(e) => setToken(e.target.value)}
                    />
                    <button style={btn} disabled={busy !== null || !token.trim()} onClick={() => void saveToken()}>
                      保存
                    </button>
                  </div>
                </div>
              )}

              {/* 手動配置（保険）: HF から diffusers 構造を手で置く。デーモンが自動検出する。 */}
              <div style={{ ...note, marginTop: 10, borderTop: "1px solid var(--panel-border, #2c3550)", paddingTop: 8 }}>
                <div style={{ marginBottom: 4 }}>
                  手動で置く場合: 下のフォルダに <b>diffusers 形式</b>（model_index.json ＋ transformer/
                  text_encoder/ vae/ などのサブフォルダ）を入れてください。
                  {selKnown?.hf_url ? (
                    <>
                      {" "}取得元 →{" "}
                      <span style={{ color: "#7fb0d8", wordBreak: "break-all" }}>{selKnown.hf_url}</span>
                    </>
                  ) : null}
                </div>
                <div style={{ display: "flex", gap: 6, alignItems: "center", flexWrap: "wrap" }}>
                  <button style={btnGhost} onClick={() => void openFolder()}>
                    📁 配置先フォルダを開く
                  </button>
                  <button style={btnGhost} disabled={busy !== null} onClick={() => void reload()}>
                    🔄 更新（再スキャン）
                  </button>
                  <span style={{ color: "var(--panel-muted, #6b7488)" }}>置いたら「更新」で認識されます</span>
                </div>
              </div>
            </div>

            {/* 3. サーバー */}
            <div style={sec}>
              <div style={secTitle}>{dot(st.server_running)} 生成サーバー（常駐デーモン）</div>
              {st.server_running ? (
                <div>
                  <div style={note}>稼働中（127.0.0.1）。生成できます。</div>
                  <button style={btnGhost} disabled={busy !== null} onClick={() => void stopServer()} title="生成を強制終了しVRAMを解放します">
                    停止（VRAM解放）
                  </button>
                </div>
              ) : (
                <div>
                  <div style={note}>停止中。生成時に自動で起動しますが、ここで先に起動もできます。</div>
                  <button
                    style={btn}
                    disabled={busy !== null || !st.deps_ok}
                    onClick={() => void startServer()}
                  >
                    今すぐ起動
                  </button>
                </div>
              )}
            </div>

            {progress && <div style={{ ...note, color: "#7fb0d8", marginTop: 8 }}>{progress}</div>}
            {error && (
              <div style={{ ...note, color: "#d96a6a", marginTop: 8, wordBreak: "break-all" }}>{error}</div>
            )}
          </>
        )}

        <div style={{ display: "flex", justifyContent: "flex-end", gap: 8, marginTop: 18 }}>
          <button style={btnGhost} onClick={onCancel}>
            閉じる
          </button>
          <button
            style={{ ...btn, opacity: ready ? 1 : 0.5 }}
            disabled={!ready || busy !== null}
            onClick={() => st && onReady(st)}
          >
            画像生成カードを作成
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
// 成功/警告の色付きボックスはアクセント色の半透明ティントにし、ライトテーマでも沈まない。
const panel: React.CSSProperties = {
  background: "var(--panel-bg, #1c2130)",
  border: "1px solid var(--panel-border, #38445e)",
  borderRadius: 10,
  padding: 20,
  width: 560,
  maxHeight: "85vh",
  overflowY: "auto",
  boxShadow: "0 8px 32px #000000aa",
  fontFamily: "sans-serif",
  color: "var(--panel-text, #d6dded)",
  fontSize: 13,
  lineHeight: 1.6,
};
const sec: React.CSSProperties = {
  border: "1px solid var(--panel-border, #2c3550)",
  borderRadius: 8,
  padding: "10px 12px",
  marginBottom: 10,
  background: "var(--panel-header-bg, #161b28)",
};
const secTitle: React.CSSProperties = { fontSize: 13, marginBottom: 6 };
const note: React.CSSProperties = { fontSize: 11, color: "var(--panel-muted, #9aa3b8)" };
const btn: React.CSSProperties = {
  background: "#3a6ea5",
  color: "#fff",
  border: "none",
  borderRadius: 6,
  padding: "6px 12px",
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
const select: React.CSSProperties = {
  background: "var(--panel-input-bg, #0f1420)",
  color: "var(--panel-text, #d6dded)",
  border: "1px solid var(--panel-border, #38445e)",
  borderRadius: 6,
  padding: "6px 8px",
  fontSize: 12,
  flex: 1,
  minWidth: 240,
};
