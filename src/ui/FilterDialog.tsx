import { useState, useEffect, useMemo, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";

// v1.5 フィルタ設定ダイアログ: JSON直編集の置き換え。
// NAME/PATH をコマンドラインから分離し、CMD は「< {in} > {out} -option」だけ書く。

type FilterType = "filter" | "command" | "generator";

/** Type ごとの CMD 既定書式。未編集ならタイプ切替で差し替える。 */
const CMD_DEFAULTS: Record<FilterType, string> = {
  filter: '< "{in}" > "{out}"',
  command: '"{in}" "{out}"',
  // 標準出力に吐くジェネレータが大半 — リダイレクトを最初から付けて書き忘れを防ぐ
  generator: '> "{out}"',
};

const HINT_OPTIONS: { label: string; mime: string }[] = [
  { label: "すべて", mime: "*/*" },
  { label: "テキスト", mime: "text/*" },
  { label: "画像", mime: "image/*" },
  { label: "動画", mime: "video/*" },
  { label: "音声", mime: "audio/*" },
];

interface FilterDialogProps {
  /** 編集時はカードの現在の表示名、作成時は undefined */
  initialLabel?: string;
  /** 編集時は filter_ref の JSON、作成時は undefined */
  initialSpec?: string;
  onSave: (label: string, specJson: string) => void;
  onCancel: () => void;
}

export function FilterDialog({ initialLabel, initialSpec, onSave, onCancel }: FilterDialogProps) {
  const init = useMemo(() => {
    try {
      return initialSpec ? (JSON.parse(initialSpec) as Record<string, unknown>) : {};
    } catch {
      return {};
    }
  }, [initialSpec]);
  const str = (k: string) => (typeof init[k] === "string" ? (init[k] as string) : "");

  const [title, setTitle] = useState(initialLabel ?? "");
  const [name, setName] = useState(str("name"));
  const [path, setPath] = useState(str("path"));
  const [ftype, setFtype] = useState<FilterType>(
    (str("type") as FilterType) || "filter"
  );
  const [cmd, setCmd] = useState(str("cmd") || CMD_DEFAULTS[ftype]);
  const [hint, setHint] = useState(str("accept_mime") || "*/*");
  const [inExts, setInExts] = useState(
    Array.isArray(init.in_exts) ? (init.in_exts as string[]).join(",") : ""
  );
  const [outputExt, setOutputExt] = useState(str("output_ext"));
  const [outDir, setOutDir] = useState(str("out_dir"));
  const [prefix, setPrefix] = useState(str("prefix"));
  // 連番は空欄を許す: 空欄=固定名（毎回同じファイルへ上書き — コンパイラ等）
  const [seq, setSeq] = useState(typeof init.seq === "number" ? String(init.seq) : "");
  const [note, setNote] = useState(str("note"));
  const [timeoutSecs, setTimeoutSecs] = useState(
    typeof init.timeout_secs === "number" ? (init.timeout_secs as number) : 60
  );

  // 事前チェック: NAME/PATH からプログラムを解決できるか（解決順は backend と同一）
  const [resolved, setResolved] = useState<string | null | "checking">(null);
  // .texos/filters に同梱済みのプログラム一覧（選ぶだけで可搬な設定になる）
  const [bundled, setBundled] = useState<string[]>([]);
  // 同梱解除の直後（Ctrl+Z / 元に戻す が効く間）に解除したファイル名を保持
  const [removedName, setRemovedName] = useState<string | null>(null);
  // 解除/復元の後に同じ name/path で再チェックさせるためのトリガー
  const [checkTick, setCheckTick] = useState(0);

  useEffect(() => {
    invoke<string[]>("list_filter_programs")
      .then(setBundled)
      .catch(() => setBundled([]));
  }, []);

  useEffect(() => {
    const n = name.trim();
    if (!n) {
      setResolved(null);
      return;
    }
    setResolved("checking");
    const t = setTimeout(() => {
      invoke<string | null>("check_filter_program", { name: n, path: path.trim() || null })
        .then((r) => setResolved(r))
        .catch(() => setResolved(null));
    }, 300);
    return () => clearTimeout(t);
  }, [name, path, checkTick]);

  // 同梱解除: 完全削除ではなく .removed へ退避（backend）→ Ctrl+Z で戻せる
  const unbundle = async () => {
    const n = name.trim();
    if (!n) return;
    try {
      await invoke("remove_filter_program", { name: n });
      setRemovedName(n);
      setBundled(await invoke<string[]>("list_filter_programs"));
      setCheckTick((t) => t + 1);
    } catch (e) {
      console.error("remove_filter_program failed", e);
    }
  };

  const restoreBundle = useCallback(async () => {
    if (!removedName) return;
    try {
      await invoke("restore_filter_program", { name: removedName });
      setRemovedName(null);
      setBundled(await invoke<string[]>("list_filter_programs"));
      setCheckTick((t) => t + 1);
    } catch (e) {
      console.error("restore_filter_program failed", e);
    }
  }, [removedName]);

  // 解除直後だけ Ctrl+Z を「同梱を元に戻す」に割り当てる
  // （capture でキャンバス側の UNDO より先に取り、伝播を止める）
  useEffect(() => {
    if (!removedName) return;
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "z" && !e.shiftKey) {
        e.preventDefault();
        e.stopPropagation();
        void restoreBundle();
      }
    };
    window.addEventListener("keydown", onKey, { capture: true });
    return () => window.removeEventListener("keydown", onKey, { capture: true });
  }, [removedName, restoreBundle]);

  const isGenerator = ftype === "generator";

  const changeType = (t: FilterType) => {
    // CMD が既定書式のまま（未編集）ならタイプに合わせて差し替える
    if (!cmd.trim() || Object.values(CMD_DEFAULTS).includes(cmd.trim())) {
      setCmd(CMD_DEFAULTS[t]);
    }
    setFtype(t);
  };

  const pickProgram = async () => {
    const exe = await open({
      multiple: false,
      title: "プログラムを選択",
      filters: [{ name: "プログラム", extensions: ["exe", "bat", "cmd", "com"] }],
    });
    if (!exe || Array.isArray(exe)) return;
    setPath(exe);
    setName(exe.split(/[\\/]/).pop() ?? "");
  };

  // 同梱ソース: PATH欄が空でも、NAME が .texos\filters / 環境変数PATH から解決できた
  // フルパス（resolved）を使う（バグ修正: PATH空だと同梱ボタンが押せなかった）。
  const resolvedPath =
    typeof resolved === "string" && resolved !== "checking" ? resolved : "";
  // すでに .texos\filters に同梱済みなら再同梱は無意味（その場合は「同梱解除」を出す）。
  const alreadyBundled = resolvedPath.includes("\\.texos\\filters\\");
  const bundleSrc = path.trim() || resolvedPath;

  const importProgram = async () => {
    const src = bundleSrc;
    if (!src) return;
    try {
      const n = await invoke<string>("import_filter_program", { src });
      setName(n);
      setPath(""); // 同梱したので絶対パスは不要 → filters から解決（可搬）
      setBundled(await invoke<string[]>("list_filter_programs"));
      setCheckTick((t) => t + 1); // 解決結果を .texos\filters 側に更新
    } catch (e) {
      console.error("import_filter_program failed", e);
    }
  };

  const pickOutDir = async () => {
    const dir = await open({ directory: true, title: "出力フォルダを選択" });
    if (typeof dir === "string") setOutDir(dir);
  };

  const save = () => {
    const spec: Record<string, unknown> = {};
    if (name.trim()) {
      spec.name = name.trim();
      spec.type = ftype;
      if (path.trim()) spec.path = path.trim();
    }
    if (cmd.trim()) spec.cmd = cmd.trim();
    if (!isGenerator) {
      spec.accept_mime = hint;
      const exts = inExts
        .split(",")
        .map((s) => s.trim().replace(/^\./, ""))
        .filter(Boolean);
      if (exts.length) spec.in_exts = exts;
    }
    if (outputExt.trim()) spec.output_ext = outputExt.trim().replace(/^\./, "");
    if (outDir.trim()) spec.out_dir = outDir.trim();
    if (prefix.trim()) {
      spec.prefix = prefix.trim();
      // 空欄なら seq を持たせない = 固定名上書きモード
      if (seq.trim() !== "") spec.seq = Math.max(0, parseInt(seq, 10) || 0);
    }
    if (note.trim()) spec.note = note.trim();
    // v1.3.11: 0 = 無制限（開発者モード）。負値は60にフォールバック。
    spec.timeout_secs = timeoutSecs >= 0 ? timeoutSecs : 60;
    onSave(title.trim() || name.trim() || "外部フィルタ", JSON.stringify(spec));
  };

  const canSave = Boolean(name.trim() || cmd.trim());

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
        }}
      >
        <div style={{ fontSize: 15, marginBottom: 14, textAlign: "center" }}>
          外部プログラム フィルタの設定
        </div>

        {row(
          "タイトル",
          <input
            style={inputStyle()}
            value={title}
            onChange={(e) => setTitle(e.target.value)}
            placeholder={name.trim() || "カード名（空欄=プログラム名）"}
          />
        )}

        {row(
          "NAME",
          <div style={{ display: "flex", gap: 6 }}>
            <input
              style={{ ...inputStyle(), flex: 1 }}
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="sp2tab.exe"
            />
            {bundled.length > 0 && (
              <select
                style={{ ...inputStyle(), width: 110, flex: "none" }}
                value=""
                onChange={(e) => {
                  if (e.target.value) {
                    setName(e.target.value);
                    setPath(""); // 同梱プログラムは filters から解決
                  }
                }}
              >
                <option value="">同梱から…</option>
                {bundled.map((b) => (
                  <option key={b} value={b}>
                    {b}
                  </option>
                ))}
              </select>
            )}
            <button style={smallBtn()} onClick={() => void pickProgram()}>
              参照…
            </button>
          </div>
        )}

        {row(
          "PATH",
          <div style={{ display: "flex", gap: 6, alignItems: "center" }}>
            <input
              style={{ ...inputStyle(), flex: 1 }}
              value={path}
              onChange={(e) => setPath(e.target.value)}
              placeholder="空欄 = .texos\filters → 環境変数PATH から検索"
            />
            <button
              style={smallBtn()}
              title={
                alreadyBundled
                  ? "すでに .texos\\filters に同梱済みです"
                  : "プログラムを .texos\\filters にコピーしてプロジェクトごと持ち運べるようにする（PATH空欄でも解決済みプログラムを同梱できます）"
              }
              disabled={!bundleSrc || alreadyBundled || resolved === "checking"}
              onClick={() => void importProgram()}
            >
              同梱⬇
            </button>
          </div>
        )}

        {/* 事前チェックの結果（検索順: 絶対パス → .texos\filters → 環境変数PATH） */}
        {name.trim() && (
          <div
            style={{
              margin: "2px 0 10px 92px",
              fontSize: 11,
              color: resolved === "checking" ? "#8a93a8" : resolved ? "#5fbf77" : "#d96a6a",
              wordBreak: "break-all",
              display: "flex",
              alignItems: "center",
              gap: 8,
              flexWrap: "wrap",
            }}
          >
            <span>
              {resolved === "checking"
                ? "確認中…"
                : resolved
                  ? `✓ ${resolved}`
                  : "✗ プログラムが見つかりません（実行時にエラーになります）"}
            </span>
            {/* 同梱コピーを指しているときだけ解除を出す（誤同梱からの復帰路） */}
            {typeof resolved === "string" &&
              resolved !== "checking" &&
              resolved.includes("\\.texos\\filters\\") && (
                <button
                  style={{ ...smallBtn(), padding: "2px 8px", fontSize: 11 }}
                  title=".texos\filters から退避します（Ctrl+Z で元に戻せます）"
                  onClick={() => void unbundle()}
                >
                  同梱解除
                </button>
              )}
          </div>
        )}
        {removedName && (
          <div
            style={{
              margin: "-4px 0 10px 92px",
              fontSize: 11,
              color: "#d8b87f",
              display: "flex",
              alignItems: "center",
              gap: 8,
            }}
          >
            <span>「{removedName}」の同梱を解除しました</span>
            <button
              style={{ ...smallBtn(), padding: "2px 8px", fontSize: 11 }}
              onClick={() => void restoreBundle()}
            >
              元に戻す（Ctrl+Z）
            </button>
          </div>
        )}

        {row(
          "Type",
          <div style={{ display: "flex", gap: 18 }}>
            {(
              [
                ["filter", "フィルタ（< 入力 > 出力）"],
                ["command", "コマンド（引数で指定）"],
                ["generator", "ジェネレータ（入力不要）"],
              ] as [FilterType, string][]
            ).map(([t, label]) => (
              <label key={t} style={{ display: "flex", alignItems: "center", gap: 5, cursor: "pointer" }}>
                <input type="radio" checked={ftype === t} onChange={() => changeType(t)} />
                {label}
              </label>
            ))}
          </div>
        )}

        {row(
          "CMD",
          <input
            style={{ ...inputStyle(), fontFamily: "Consolas, monospace" }}
            value={cmd}
            onChange={(e) => setCmd(e.target.value)}
            placeholder={CMD_DEFAULTS[ftype]}
            title={
              "プログラム名は NAME/PATH から自動で付く。{in}/{out} が入出力ファイルに置換される。\n" +
              "複数入力: {in*}=全入力一括（裸で書く・自動クォート）、{in1} {in2}…=接続順の位置指定。\n" +
              "{in*} か {in2} 以降を書くと「全入力を集めて1回実行」になる（gcc分割ソース・画像合成）"
            }
          />
        )}

        {row(
          "IN",
          <div style={{ display: "flex", gap: 6, alignItems: "center", opacity: isGenerator ? 0.4 : 1 }}>
            <select
              style={{ ...inputStyle(), width: 110, flex: "none" }}
              value={hint}
              disabled={isGenerator}
              onChange={(e) => setHint(e.target.value)}
            >
              {HINT_OPTIONS.map((o) => (
                <option key={o.mime} value={o.mime}>
                  {o.label}
                </option>
              ))}
            </select>
            <span style={{ fontSize: 12, color: "#8a93a8", flex: "none" }}>拡張子</span>
            <input
              style={{ ...inputStyle(), flex: 1 }}
              value={inExts}
              disabled={isGenerator}
              onChange={(e) => setInExts(e.target.value)}
              placeholder="txt,csv,prn（空欄=制限なし）"
            />
          </div>
        )}

        {row(
          "出力拡張子",
          <input
            style={{ ...inputStyle(), width: 120 }}
            value={outputExt}
            onChange={(e) => setOutputExt(e.target.value)}
            placeholder={isGenerator ? "txt（必須相当）" : "空欄=入力と同じ"}
          />
        )}

        {row(
          "OUT",
          <div style={{ display: "flex", gap: 6 }}>
            <input
              style={{ ...inputStyle(), flex: 1 }}
              value={outDir}
              onChange={(e) => setOutDir(e.target.value)}
              placeholder="空欄=プロジェクト直下（実身）／指定=窓（仮身）で配置"
            />
            <button style={smallBtn()} onClick={() => void pickOutDir()}>
              フォルダ…
            </button>
          </div>
        )}

        {row(
          "Prefix",
          <div style={{ display: "flex", gap: 6, alignItems: "center" }}>
            <input
              style={{ ...inputStyle(), width: 160 }}
              value={prefix}
              onChange={(e) => setPrefix(e.target.value)}
              placeholder="空欄=入力名_フィルタ名"
            />
            <span style={{ fontSize: 12, color: "#8a93a8", flex: "none" }}>連番</span>
            <input
              style={{ ...inputStyle(), width: 80 }}
              type="number"
              min={0}
              value={seq}
              disabled={!prefix.trim()}
              placeholder="固定名"
              onChange={(e) => setSeq(e.target.value)}
            />
            <span style={{ fontSize: 11, color: "#8a93a8" }}>
              →{" "}
              {prefix.trim()
                ? seq.trim() === ""
                  ? `${prefix.trim()}.${outputExt.trim() || "ext"}（毎回上書き）`
                  : `${prefix.trim()}${String(Math.max(0, parseInt(seq, 10) || 0)).padStart(3, "0")}.${outputExt.trim() || "ext"}`
                : "—"}
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

        {row(
          "制限時間",
          <div style={{ display: "flex", gap: 6, alignItems: "center" }}>
            <input
              style={{ ...inputStyle(), width: 80 }}
              type="number"
              min={0}
              value={timeoutSecs}
              onChange={(e) => setTimeoutSecs(Math.max(0, parseInt(e.target.value, 10) || 0))}
            />
            <span style={{ fontSize: 12, color: "#8a93a8" }}>
              {timeoutSecs === 0 ? "秒（0=無制限・中断ボタンで停止）" : "秒で強制終了（暴走対策・0=無制限）"}
            </span>
          </div>
        )}

        <div style={{ display: "flex", justifyContent: "flex-end", gap: 8, marginTop: 16 }}>
          <button onClick={onCancel} style={btnStyle("#2a3346", "#aeb8cc")}>
            キャンセル
          </button>
          <button onClick={save} disabled={!canSave} style={btnStyle(canSave ? "#3a6fb0" : "#2a3346", "#ffffff")}>
            保存
          </button>
        </div>
      </div>
    </div>
  );
}

function row(label: string, field: React.ReactNode) {
  return (
    <div style={{ display: "flex", alignItems: "center", gap: 10, marginBottom: 10 }}>
      <div style={{ width: 82, flex: "none", textAlign: "right", color: "#aeb8cc", fontSize: 12 }}>
        {label}
      </div>
      <div style={{ flex: 1, minWidth: 0 }}>{field}</div>
    </div>
  );
}

function inputStyle(): React.CSSProperties {
  return {
    width: "100%",
    padding: "6px 8px",
    background: "#0e121c",
    border: "1px solid #38445e",
    borderRadius: 6,
    color: "#eef2f9",
    fontSize: 13,
    outline: "none",
    boxSizing: "border-box",
  };
}

function smallBtn(): React.CSSProperties {
  return {
    padding: "6px 10px",
    background: "#2a3346",
    color: "#aeb8cc",
    border: "1px solid #38445e",
    borderRadius: 6,
    cursor: "pointer",
    fontSize: 12,
    fontFamily: "sans-serif",
    flex: "none",
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
