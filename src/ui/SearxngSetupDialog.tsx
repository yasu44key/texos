import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

// Web検索 P1: 検索エンジン（SearXNG）の導入・管理ダイアログ（docs/Web検索_SearXNG計画.md §2）。
// Docker なし。TexoS 管理の Python 3.13 / venv で SearXNG を直接動かす（t2i と同じ流儀）。
// フルオート: 未導入なら「導入する」1回（ダウンロードを伴うので同意だけは残す）→ 導入完了後に
// 自動で起動＋試験検索まで進む。以後は web_search の実行時に遅延起動・10分無使用で停止。

export interface SearxngStatus {
  installed: boolean;
  pin: string;
  running: boolean;
  port: number;
  python: string;
  root: string;
}

interface SearxngTest {
  port: number;
  results: number;
  unresponsive: string[];
  titles: string[];
}

interface Props {
  onCancel: () => void;
  /** 渡されたときだけ「Web検索カードを作成」を出す（P2 のカード作成導線）。 */
  onReady?: (status: SearxngStatus) => void;
}

export function SearxngSetupDialog({ onCancel, onReady }: Props) {
  const [st, setSt] = useState<SearxngStatus | null>(null);
  const [busy, setBusy] = useState<null | "install" | "test" | "stop">(null);
  const [progress, setProgress] = useState("");
  const [percent, setPercent] = useState(0);
  const [error, setError] = useState("");
  const [test, setTest] = useState<SearxngTest | null>(null);

  const reload = useCallback(async () => {
    try {
      setSt(await invoke<SearxngStatus>("searxng_status"));
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  // 導入進捗（python-env のうち env="searxng" の分だけ）。
  useEffect(() => {
    let un: (() => void) | undefined;
    void listen<{ env?: string; status?: string; percent?: number }>("python-env", (e) => {
      if (e.payload.env !== "searxng") return;
      setProgress(e.payload.status ?? "導入中");
      setPercent(e.payload.percent ?? 0);
    }).then((u) => (un = u));
    return () => un?.();
  }, []);

  const runTest = async () => {
    setBusy("test");
    setError("");
    setTest(null);
    setProgress("起動して試しに検索しています…");
    try {
      setTest(await invoke<SearxngTest>("searxng_test"));
      setProgress("");
    } catch (e) {
      setError(String(e));
      setProgress("");
    } finally {
      setBusy(null);
      void reload();
    }
  };

  const install = async (force = false) => {
    setBusy("install");
    setError("");
    setTest(null);
    setPercent(0);
    setProgress(force ? "入れ直しています…" : "導入を始めています…");
    try {
      await invoke("searxng_install", { force });
      await reload();
    } catch (e) {
      setError(String(e));
      setProgress("");
      setBusy(null);
      return;
    }
    setBusy(null);
    // フルオート: 導入が済んだらそのまま起動＋試験検索まで進める。
    await runTest();
  };

  const stop = async () => {
    setBusy("stop");
    try {
      await invoke("searxng_stop");
    } finally {
      setBusy(null);
      setTest(null);
      void reload();
    }
  };

  const ok = !!st?.installed;

  return (
    <div style={overlay} onMouseDown={onCancel}>
      <div onMouseDown={(e) => e.stopPropagation()} style={panel}>
        <div style={{ fontSize: 15, marginBottom: 4, textAlign: "center" }}>Web検索エンジン（SearXNG）</div>
        <div style={{ ...note, marginBottom: 14, textAlign: "center" }}>
          複数の検索エンジンの結果をまとめるオープンソースの検索エンジンを、この PC の中だけで動かします。
          API キーも Docker も要りません。
        </div>

        {!st ? (
          <div style={note}>状態を確認中…</div>
        ) : (
          <>
            <div style={sec}>
              <div style={secTitle}>
                {dot(ok)} 導入{ok ? "済み" : "されていません"}
              </div>
              {ok ? (
                <div style={note}>
                  版: {st.pin}（Python {st.python}）
                  <br />
                  置き場所: {st.root}
                </div>
              ) : (
                <div style={{ ...note, lineHeight: 1.7 }}>
                  「導入する」を押すと、次をダウンロードして自動で組み立てます（合計 約200MB・数分）。
                  <br />・Python {st.python}（TexoS 専用。システムの Python には触れません）
                  <br />・SearXNG 本体（{st.pin}・GitHub から取得し、中身が固定版と一致するか検証）
                  <br />・SearXNG が使う部品
                  <br />
                  導入後はそのまま試しに1回検索して、動くことを確かめます。
                </div>
              )}
              <div style={{ display: "flex", gap: 8, marginTop: 8 }}>
                {!ok ? (
                  <button style={btn} disabled={busy !== null} onClick={() => void install(false)}>
                    {busy === "install" ? "導入中…" : "導入する"}
                  </button>
                ) : (
                  <button
                    style={btnGhost}
                    disabled={busy !== null}
                    onClick={() => void install(true)}
                    title="検索が壊れたときなどに、固定版を消して入れ直します"
                  >
                    {busy === "install" ? "入れ直し中…" : "入れ直す"}
                  </button>
                )}
              </div>
              {busy === "install" && (
                <div style={{ marginTop: 8 }}>
                  <div style={{ height: 6, background: "#2a3145", borderRadius: 3, overflow: "hidden" }}>
                    <div style={{ width: `${percent}%`, height: "100%", background: "#3a6ea5", transition: "width .3s" }} />
                  </div>
                </div>
              )}
            </div>

            {ok && (
              <div style={sec}>
                <div style={secTitle}>
                  {dot(st.running)} {st.running ? `稼働中（127.0.0.1:${st.port}）` : "停止中"}
                </div>
                <div style={note}>
                  検索するときに自動で起動し、10分使わなければ自動で止まります。TexoS を閉じると一緒に終わります。
                </div>
                <div style={{ display: "flex", gap: 8, marginTop: 8 }}>
                  <button style={btn} disabled={busy !== null} onClick={() => void runTest()}>
                    {busy === "test" ? "検索中…" : "試しに検索する"}
                  </button>
                  {st.running && (
                    <button style={btnGhost} disabled={busy !== null} onClick={() => void stop()}>
                      停止
                    </button>
                  )}
                </div>
                {test && (
                  <div style={{ ...note, marginTop: 8, lineHeight: 1.7 }}>
                    {test.results > 0 ? (
                      <span style={{ color: "#5fbf77" }}>✓ 検索できました（{test.results} 件）</span>
                    ) : (
                      <span style={{ color: "#d9a06a" }}>検索結果が0件でした（ネットワークを確認してください）</span>
                    )}
                    {test.titles.map((t, i) => (
                      <div key={i}>・{t}</div>
                    ))}
                    {test.unresponsive.length > 0 && (
                      <div style={{ color: "#d9a06a" }}>
                        応答しなかった検索元（一時的に休止されることがあります）: {test.unresponsive.join(" ／ ")}
                      </div>
                    )}
                  </div>
                )}
              </div>
            )}

            {progress && <div style={{ ...note, color: "#7fb0d8", marginTop: 8, wordBreak: "break-all" }}>{progress}</div>}
            {error && (
              <div style={{ ...note, color: "#d96a6a", marginTop: 8, wordBreak: "break-all", whiteSpace: "pre-wrap" }}>
                {error}
              </div>
            )}
          </>
        )}

        <div style={{ display: "flex", justifyContent: "flex-end", gap: 8, marginTop: 18 }}>
          <button style={btnGhost} onClick={onCancel}>
            閉じる
          </button>
          {onReady && (
            <button
              style={{ ...btn, opacity: ok ? 1 : 0.5 }}
              disabled={!ok || busy !== null}
              onClick={() => st && onReady(st)}
            >
              Web検索カードを作成
            </button>
          )}
        </div>
      </div>
    </div>
  );
}

const dot = (on: boolean) => <span style={{ color: on ? "#5fbf77" : "#8a93a8" }}>{on ? "●" : "○"}</span>;

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
