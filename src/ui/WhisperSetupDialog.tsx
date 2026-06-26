import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

// v1.4.8 ①: Whisper（音声→テキスト）の自動導入＆モデル管理ダイアログ。
// whisper.cpp 本体＋モデルは %APPDATA%\com.yoshi.texos\whisper\ に共有で置く（A案）。
// ffmpeg は前段の「Whisper用WAVに変換」プリセットで使う（m4a/動画→16kHz mono WAV）。

export interface WhisperStatus {
  ffmpeg: boolean;
  whisper: boolean;
  models: string[];
  current_model: string;
  current_ready: boolean;
  can_winget: boolean;
  cli_path: string;
  model_path: string;
}

// 既定で選べるモデル（日本語は small 既定・精度を上げるなら medium）。
const MODEL_CHOICES: { name: string; label: string }[] = [
  { name: "base", label: "base（約148MB・軽量）" },
  { name: "small", label: "small（約488MB・既定／日本語実用）" },
  { name: "medium", label: "medium（約1.5GB・高精度）" },
];

interface Props {
  onCancel: () => void;
  /** 全部そろったら呼ばれる。最新の status を渡す（カード作成に cli_path/model_path を使う）。 */
  onCreate: (status: WhisperStatus) => void;
}

export function WhisperSetupDialog({ onCancel, onCreate }: Props) {
  const [st, setSt] = useState<WhisperStatus | null>(null);
  const [model, setModel] = useState("small");
  const [busy, setBusy] = useState<null | "ffmpeg" | "whisper" | "model">(null);
  const [progress, setProgress] = useState<string>("");
  const [error, setError] = useState<string>("");

  const reload = useCallback(async () => {
    try {
      const s = await invoke<WhisperStatus>("whisper_status");
      setSt(s);
      setModel(s.current_model || "small");
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  // 進捗イベント（whisper-install / whisper-model）を購読。
  const reloadRef = useRef(reload);
  reloadRef.current = reload;
  useEffect(() => {
    const uns: Array<() => void> = [];
    void listen<{ status?: string; percent?: number }>("whisper-install", (e) => {
      setProgress(`${e.payload.status ?? ""} ${e.payload.percent ?? 0}%`);
    }).then((u) => uns.push(u));
    void listen<{ model?: string; percent?: number }>("whisper-model", (e) => {
      setProgress(`モデル取得 ${e.payload.model ?? ""}: ${e.payload.percent ?? 0}%`);
    }).then((u) => uns.push(u));
    return () => uns.forEach((u) => u());
  }, []);

  const installFfmpeg = async () => {
    setBusy("ffmpeg");
    setError("");
    setProgress("ffmpeg を winget で導入中…（数分かかることがあります）");
    try {
      await invoke("whisper_install_ffmpeg");
      setProgress("ffmpeg を導入しました。PATH 反映のため TexoS の再起動が必要な場合があります。");
      await reload();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  const installWhisper = async () => {
    setBusy("whisper");
    setError("");
    setProgress("whisper.cpp を取得中…");
    try {
      await invoke("whisper_install");
      await reload();
      setProgress("whisper.cpp を導入しました。");
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
      await invoke("whisper_download_model", { model });
      await reload();
      setProgress(`モデル ${model} を取得しました。`);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  // モデル切替（取得済みのものを選んだら現在モデルに）。
  const switchModel = async (name: string) => {
    setModel(name);
    if (st?.models.includes(name)) {
      try {
        await invoke("whisper_set_model", { model: name });
        await reload();
      } catch (e) {
        setError(String(e));
      }
    }
  };

  const selectedDownloaded = !!st?.models.includes(model);
  const ready = !!st && st.ffmpeg && st.whisper && selectedDownloaded;

  const dot = (ok: boolean) => (
    <span style={{ color: ok ? "#5fbf77" : "#d96a6a", marginRight: 6 }}>{ok ? "✓" : "✗"}</span>
  );

  return (
    <div
      style={{
        position: "fixed",
        inset: 0,
        background: "#00000066",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        zIndex: 2000,
      }}
      onMouseDown={onCancel}
    >
      <div
        onMouseDown={(e) => e.stopPropagation()}
        style={{
          background: "#1c2130",
          border: "1px solid #38445e",
          borderRadius: 10,
          padding: 20,
          width: 560,
          maxHeight: "85vh",
          overflowY: "auto",
          boxShadow: "0 8px 32px #000000aa",
          fontFamily: "sans-serif",
          color: "#d6dded",
          fontSize: 13,
          lineHeight: 1.6,
        }}
      >
        <div style={{ fontSize: 15, marginBottom: 4, textAlign: "center" }}>
          Whisper（音声→テキスト）のセットアップ
        </div>
        <div style={{ fontSize: 11, color: "#8a93a8", marginBottom: 14, textAlign: "center" }}>
          m4a など →（ffmpegで16kHz WAVに変換）→ Whisper → テキスト
        </div>

        {!st && <div>状態を確認中…</div>}

        {st && (
          <>
            {/* 1. ffmpeg */}
            <div style={sec}>
              <div style={secTitle}>
                {dot(st.ffmpeg)} ffmpeg（前段のWAV変換用）
              </div>
              {st.ffmpeg ? (
                <div style={note}>利用可能です。</div>
              ) : (
                <div>
                  <div style={note}>
                    未導入です。「Whisper用WAVに変換」プリセットに必要です。
                  </div>
                  {st.can_winget ? (
                    <button style={btn} disabled={busy !== null} onClick={() => void installFfmpeg()}>
                      winget で導入
                    </button>
                  ) : (
                    <div style={note}>
                      winget が無いため、ffmpeg.org から手動で導入してください。
                    </div>
                  )}
                </div>
              )}
            </div>

            {/* 2. whisper.cpp 本体 */}
            <div style={sec}>
              <div style={secTitle}>
                {dot(st.whisper)} whisper.cpp 本体
              </div>
              {st.whisper ? (
                <div style={note}>導入済み（{st.cli_path}）。</div>
              ) : (
                <div>
                  <div style={note}>
                    未導入です。安定版の Windows バイナリを取得して導入します（約8MB）。
                  </div>
                  <button style={btn} disabled={busy !== null} onClick={() => void installWhisper()}>
                    ダウンロードして導入
                  </button>
                </div>
              )}
            </div>

            {/* 3. モデル */}
            <div style={sec}>
              <div style={secTitle}>
                {dot(selectedDownloaded)} モデル（日本語は small 既定・精度は medium）
              </div>
              <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap" }}>
                <select
                  value={model}
                  onChange={(e) => void switchModel(e.target.value)}
                  style={select}
                >
                  {MODEL_CHOICES.map((m) => (
                    <option key={m.name} value={m.name}>
                      {st.models.includes(m.name) ? "✓ " : "　"}
                      {m.label}
                    </option>
                  ))}
                  {/* 既定以外で取得済みのもの */}
                  {st.models
                    .filter((m) => !MODEL_CHOICES.some((c) => c.name === m))
                    .map((m) => (
                      <option key={m} value={m}>
                        ✓ {m}
                      </option>
                    ))}
                </select>
                {selectedDownloaded ? (
                  <span style={{ color: "#5fbf77" }}>取得済み（選択中）</span>
                ) : (
                  <button style={btn} disabled={busy !== null} onClick={() => void downloadModel()}>
                    ダウンロード
                  </button>
                )}
              </div>
            </div>

            {progress && (
              <div style={{ ...note, color: "#7fb0d8", marginTop: 8 }}>{progress}</div>
            )}
            {error && (
              <div style={{ ...note, color: "#d96a6a", marginTop: 8, wordBreak: "break-all" }}>
                {error}
              </div>
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
            onClick={() => st && onCreate(st)}
          >
            Whisperカードを作成
          </button>
        </div>
      </div>
    </div>
  );
}

const sec: React.CSSProperties = {
  border: "1px solid #2c3550",
  borderRadius: 8,
  padding: "10px 12px",
  marginBottom: 10,
  background: "#161b28",
};
const secTitle: React.CSSProperties = { fontSize: 13, marginBottom: 6 };
const note: React.CSSProperties = { fontSize: 11, color: "#9aa3b8" };
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
  background: "#0f1420",
  color: "#d6dded",
  border: "1px solid #38445e",
  borderRadius: 6,
  padding: "6px 8px",
  fontSize: 12,
  flex: 1,
  minWidth: 220,
};
