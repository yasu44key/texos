import { useState } from "react";

// シェルカード統合（2026-07-06）: コマンドプロンプト/PowerShell カードの専用設定ダイアログ。
// 従来はランチャー版（開くだけ）と対話フィルタ版（一行目プロンプト入力）が別メニューに分かれ、
// 編集は外部プログラム用ダイアログ流用（NAME/PATH 検証が cmd/powershell で赤エラー）だった。
// このダイアログが作成・編集の両方を受け持つ。spec のキーは flow.rs FilterSpec と一致させること:
//   shell: "powershell" | "cmd" ／ cmd: 一行目コマンド（{in} 可・空=開くだけ）
//   window: "keep" | "close" | "hidden" ／ timeout_secs ／ note
// 既存カードの JSON は「元 spec を土台に既知キーだけ上書き」で後方互換（未知キーは温存）。

interface Props {
  /** 編集時はカードの現在の表示名、作成時は既定名。 */
  initialLabel?: string;
  /** 編集時は filter_ref の JSON、作成時は undefined。 */
  initialSpec?: string;
  onSave: (label: string, specJson: string) => void;
  onCancel: () => void;
}

const WINDOW_MODES = [
  {
    value: "keep",
    label: "開いたまま（対話を続ける）",
    hint: "シェルウィンドウが開き、コマンド実行後もそのまま対話を続けられます（従来の動作）。",
  },
  {
    value: "close",
    label: "実行して閉じる",
    hint: "シェルウィンドウで実行し、終わるとウィンドウが閉じます。",
  },
  {
    value: "hidden",
    label: "ウィンドウを出さない（結果をパネルに表示）",
    hint: "ウィンドウなしで実行し、stdout/stderr と終了コードを結果パネルに表示します。",
  },
] as const;

export function ShellSettingsDialog({ initialLabel, initialSpec, onSave, onCancel }: Props) {
  const base: Record<string, unknown> = (() => {
    try {
      return initialSpec ? (JSON.parse(initialSpec) as Record<string, unknown>) : {};
    } catch {
      return {};
    }
  })();

  const [title, setTitle] = useState(initialLabel ?? "");
  const [shell, setShell] = useState<string>(
    typeof base.shell === "string" && base.shell === "cmd" ? "cmd" : "powershell"
  );
  const [cmd, setCmd] = useState<string>(typeof base.cmd === "string" ? base.cmd : "");
  const [windowMode, setWindowMode] = useState<string>(
    typeof base.window === "string" && ["keep", "close", "hidden"].includes(base.window)
      ? base.window
      : "keep"
  );
  const [timeout_, setTimeout_] = useState<string>(
    typeof base.timeout_secs === "number" && windowModeInit(base) === "hidden"
      ? String(base.timeout_secs)
      : "60"
  );
  const [note, setNote] = useState<string>(typeof base.note === "string" ? base.note : "");

  const save = () => {
    const label =
      title.trim() || initialLabel || (shell === "cmd" ? "コマンドプロンプト" : "PowerShell");
    // 元 spec を土台に既知キーを上書き（未知キーは温存）。ウィンドウあり実行は
    // detached で待たないため制限時間は 0（無制限）に固定する。
    const spec: Record<string, unknown> = {
      ...base,
      shell,
      cmd,
      window: windowMode,
      accept_mime: "*/*",
      timeout_secs: windowMode === "hidden" ? Math.max(0, parseInt(timeout_, 10) || 0) : 0,
    };
    const n = note.trim();
    if (n) spec.note = n;
    else delete spec.note;
    onSave(label, JSON.stringify(spec));
  };

  const modeHint = WINDOW_MODES.find((m) => m.value === windowMode)?.hint ?? "";

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
        }}
      >
        <div style={{ fontSize: 15, marginBottom: 4 }}>コマンドプロンプトの設定</div>
        <div style={{ fontSize: 12, marginBottom: 16, color: "var(--panel-muted, #8a93a8)" }}>
          シェルでコマンドを実行するカードです。▶実行のたびにプロジェクトフォルダで実行します。
        </div>

        {row(
          "カード名",
          <input
            style={inputStyle()}
            value={title}
            onChange={(e) => setTitle(e.target.value)}
            placeholder={shell === "cmd" ? "コマンドプロンプト" : "PowerShell"}
          />
        )}

        {row(
          "シェル",
          <div style={{ display: "flex", gap: 16, alignItems: "center" }}>
            {(["powershell", "cmd"] as const).map((s) => (
              <label key={s} style={{ display: "flex", alignItems: "center", gap: 5, cursor: "pointer" }}>
                <input type="radio" checked={shell === s} onChange={() => setShell(s)} />
                {s === "powershell" ? "PowerShell" : "cmd（コマンドプロンプト）"}
              </label>
            ))}
          </div>
        )}

        {row(
          "コマンド",
          <input
            style={{ ...inputStyle(), fontFamily: "Consolas, monospace" }}
            value={cmd}
            onChange={(e) => setCmd(e.target.value)}
            placeholder="空欄=シェルを開くだけ。{in} は接続ファイルのパスに置換"
            spellCheck={false}
          />
        )}
        <div style={hintStyle()}>
          最初に実行するコマンド。矢印で繋いだカードのファイルを {"{in}"} で渡せます（例:{" "}
          <span style={{ fontFamily: "Consolas, monospace" }}>{"a.exe < {in}"}</span>）。
        </div>

        {row(
          "ウィンドウ",
          <select style={inputStyle()} value={windowMode} onChange={(e) => setWindowMode(e.target.value)}>
            {WINDOW_MODES.map((m) => (
              <option key={m.value} value={m.value}>
                {m.label}
              </option>
            ))}
          </select>
        )}
        <div style={hintStyle()}>{modeHint}</div>
        {windowMode === "hidden" && !cmd.trim() && (
          <div style={{ ...hintStyle(), color: "#d9a76a" }}>
            ⚠ コマンドが空欄です。ウィンドウなしでは何も実行されません。
          </div>
        )}

        {windowMode === "hidden" &&
          row(
            "制限時間",
            <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
              <input
                style={{ ...inputStyle(), width: 90 }}
                type="number"
                min={0}
                value={timeout_}
                onChange={(e) => setTimeout_(e.target.value)}
              />
              <span style={{ fontSize: 12, color: "var(--panel-muted, #8a93a8)" }}>
                秒で強制終了（0=無制限・実行中パネルの中断で停止）
              </span>
            </div>
          )}

        {row(
          "NOTE",
          <input
            style={inputStyle()}
            value={note}
            onChange={(e) => setNote(e.target.value)}
            placeholder="ユーザーメモ"
          />
        )}

        <div style={{ display: "flex", justifyContent: "flex-end", gap: 8, marginTop: 16 }}>
          <button onClick={onCancel} style={btnStyle("#2a3346", "#aeb8cc")}>
            キャンセル
          </button>
          <button onClick={save} style={btnStyle("#3a6fb0", "#ffffff")}>
            保存
          </button>
        </div>
      </div>
    </div>
  );
}

/** 初期化時に base の window モードを読む（timeout の初期値判定に使う）。 */
function windowModeInit(base: Record<string, unknown>): string {
  return typeof base.window === "string" ? base.window : "keep";
}

function row(label: string, field: React.ReactNode) {
  return (
    <div style={{ display: "flex", alignItems: "center", gap: 10, marginBottom: 10 }}>
      <div style={{ width: 82, flex: "none", textAlign: "right", color: "var(--panel-muted, #aeb8cc)", fontSize: 12 }}>
        {label}
      </div>
      <div style={{ flex: 1, minWidth: 0 }}>{field}</div>
    </div>
  );
}

function hintStyle(): React.CSSProperties {
  return { fontSize: 11, color: "var(--panel-muted, #8a93a8)", margin: "-4px 0 10px 92px" };
}

function inputStyle(): React.CSSProperties {
  return {
    width: "100%",
    padding: "6px 8px",
    background: "var(--panel-input-bg, #0e121c)",
    border: "1px solid var(--panel-border, #38445e)",
    borderRadius: 6,
    color: "var(--panel-text, #eef2f9)",
    fontSize: 13,
    outline: "none",
    boxSizing: "border-box",
  };
}

function btnStyle(bg: string, color: string): React.CSSProperties {
  return {
    padding: "7px 16px",
    background: bg,
    color,
    border: "none",
    borderRadius: 6,
    cursor: "pointer",
    fontSize: 13,
    fontFamily: "sans-serif",
  };
}
