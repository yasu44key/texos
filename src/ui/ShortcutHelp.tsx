// v1.4.12: キーボードショートカットのチートシート・オーバーレイ。
// `?`(Shift+/) / F1 で開閉、Esc・枠外クリック・× で閉じる（App 側で制御）。
// 設定ダイアログと同じく暗色固定（リファレンス用パネル）。
// 重要: ここには「実装済みのキー」だけを載せる。未実装（v1.5 の Ctrl+C/V 跨プロジェクト
// 仮身・Ctrl+B 実身化貼り付け等）は実装した号で行を足す（嘘を載せない）。

interface Section {
  title: string;
  rows: [keys: string, desc: string][];
}

// "  " 区切り = 代替キー（/）。" + " 区切り = 同時押し。
const SECTIONS: Section[] = [
  {
    title: "選択",
    rows: [
      ["左ドラッグ", "範囲選択"],
      ["Ctrl + 左クリック", "個別に選択／解除"],
      ["Alt + 左クリック", "つながりを選択（矢印でたどる）"],
      ["Ctrl + A", "すべて選択"],
      ["Esc", "選択を解除"],
    ],
  },
  {
    title: "移動・化身",
    rows: [
      ["左ドラッグ", "カードを移動"],
      ["Shift + 左ドラッグ", "1枚だけ／図形だけ移動"],
      ["Ctrl + 左ドラッグ", "その場に仮身（化身）を作る"],
      ["Ctrl + D", "選択カードの仮身を作成"],
    ],
  },
  {
    title: "クリップボード",
    rows: [
      ["Ctrl + C", "OSへファイルとしてコピー"],
      ["Ctrl + V", "貼り付け（画像／文字／URL／ファイル）"],
    ],
  },
  {
    title: "編集・整列",
    rows: [
      ["Ctrl + Z", "戻る"],
      ["Ctrl + Shift + Z  Ctrl + Y", "やり直し"],
      ["Delete", "削除"],
      ["Ctrl + G", "整列メニュー"],
    ],
  },
  {
    title: "探す",
    rows: [
      ["Ctrl + F", "カードを検索（クリック＝フォーカス）"],
    ],
  },
];

function Keys({ combo }: { combo: string }) {
  return (
    <span style={{ display: "flex", flexWrap: "wrap", gap: 3, alignItems: "center" }}>
      {combo.split("  ").map((alt, ai) => (
        <span key={ai} style={{ display: "flex", gap: 3, alignItems: "center" }}>
          {ai > 0 && <span style={{ color: "#6c7587", fontSize: 11, margin: "0 4px" }}>/</span>}
          {alt.split(" + ").map((key, ki) => (
            <span key={ki} style={{ display: "flex", gap: 3, alignItems: "center" }}>
              {ki > 0 && <span style={{ color: "#6c7587", fontSize: 11 }}>+</span>}
              <kbd
                style={{
                  fontFamily: "ui-monospace, monospace",
                  fontSize: 11,
                  background: "#0e121c",
                  border: "1px solid #38445e",
                  borderBottomWidth: 2,
                  borderRadius: 4,
                  padding: "1px 6px",
                  color: "#eef2f9",
                  whiteSpace: "nowrap",
                }}
              >
                {key}
              </kbd>
            </span>
          ))}
        </span>
      ))}
    </span>
  );
}

export function ShortcutHelp({ onClose }: { onClose: () => void }) {
  // Esc / ? / F1 での開閉は App 側のキーハンドラに一本化（二重発火を避ける）。
  // ここは枠外クリック・× ボタンでの close だけ面倒を見る。
  return (
    <div
      onMouseDown={onClose}
      style={{
        position: "fixed",
        inset: 0,
        background: "#00000066",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        zIndex: 2500,
      }}
    >
      <div
        onMouseDown={(e) => e.stopPropagation()}
        style={{
          background: "#1c2130",
          border: "1px solid #38445e",
          borderRadius: 10,
          padding: "18px 22px",
          width: 600,
          maxWidth: "92vw",
          maxHeight: "88vh",
          overflowY: "auto",
          boxShadow: "0 8px 32px #000000aa",
          fontFamily: "sans-serif",
          color: "#d6dded",
          fontSize: 13,
        }}
      >
        <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: 14 }}>
          <span style={{ fontSize: 15 }}>⌨ キーボードショートカット</span>
          <span style={{ display: "flex", alignItems: "center", gap: 8, color: "#8a93a8", fontSize: 12 }}>
            <span>
              <kbd
                style={{
                  fontFamily: "ui-monospace, monospace",
                  fontSize: 11,
                  background: "#0e121c",
                  border: "1px solid #38445e",
                  borderRadius: 4,
                  padding: "1px 6px",
                  color: "#cdd6e6",
                }}
              >
                ?
              </kbd>{" "}
              で開閉
            </span>
            <button
              onClick={onClose}
              title="閉じる"
              style={{
                background: "#2a3346",
                color: "#aeb8cc",
                border: "1px solid #38445e",
                borderRadius: 6,
                cursor: "pointer",
                fontSize: 13,
                padding: "2px 8px",
              }}
            >
              ✕
            </button>
          </span>
        </div>

        <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: "14px 22px" }}>
          {SECTIONS.map((sec) => (
            <div key={sec.title}>
              <div style={{ fontSize: 12, color: "#7da7d8", marginBottom: 6 }}>{sec.title}</div>
              <div style={{ display: "flex", flexDirection: "column", gap: 5 }}>
                {sec.rows.map(([keys, desc], i) => (
                  <div key={i} style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: 10 }}>
                    <Keys combo={keys} />
                    <span style={{ fontSize: 12, color: "#aeb8cc", textAlign: "right" }}>{desc}</span>
                  </div>
                ))}
              </div>
            </div>
          ))}
        </div>

        <div style={{ marginTop: 14, paddingTop: 10, borderTop: "1px solid #2a3346", fontSize: 12, color: "#8a93a8" }}>
          右クリック →「その他の機能」からも各操作にアクセスできます
        </div>
      </div>
    </div>
  );
}
