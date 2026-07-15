import { useState } from "react";

// v1.4.29 ⑤: ビルトインフィルタの設定ダイアログ。
// 従来の JSON 直編集（小さなプロンプト窓）を廃し、builtin 種別ごとのパラメータスキーマから
// フォームを自動生成する。backend の run_builtin（flow.rs）が読むキーと一致させること。
// 既存カードの JSON は「元の spec を土台に、既知フィールドだけ上書き」で後方互換を保つ
// （builtin / accept_mime 等は落とさず、知らないキーもそのまま残す）。

type FieldType = "text" | "number" | "select";

interface FieldSchema {
  key: string;
  label: string;
  type: FieldType;
  /** number の既定値／text・select の初期候補。 */
  default?: string | number;
  placeholder?: string;
  hint?: string;
  /** 空欄を許す（保存時にキーごと省く）。既定は必須（空でも空文字で保存）。 */
  optional?: boolean;
  /** select の選択肢。 */
  options?: { value: string; label: string }[];
  /** number の最小値。 */
  min?: number;
  /** v1.5.4 WS-P2: カンマ区切りの値を文字列配列として保存する（例 events）。 */
  asList?: boolean;
}

interface BuiltinSchema {
  /** 人間向けの種別名（ダイアログ見出し）。 */
  title: string;
  /** この builtin が常に持つ固定キー（accept_mime 等）。保存時に必ず付与する。 */
  fixed: Record<string, unknown>;
  fields: FieldSchema[];
  /** v1.5.4 WS-P2: op カード（kind='op'）のスキーマ。spec に builtin キーを入れない。 */
  opCard?: boolean;
  /** ダイアログ上部に出す説明（op カードの使い方案内）。 */
  note?: string;
}

// 対象は run_builtin の match アームと一致する内蔵フィルタ（ai / t2i は各専用ダイアログ）。
export const BUILTIN_SCHEMAS: Record<string, BuiltinSchema> = {
  text_grep: {
    title: "grep（正規表現で行を抽出）",
    fixed: { accept_mime: "text/*" },
    fields: [
      {
        key: "pattern",
        label: "正規表現",
        type: "text",
        placeholder: "抽出する行にマッチする正規表現（例: ^ERROR）",
        hint: "この正規表現にマッチした行だけを残します（Rust の regex 構文）。",
      },
    ],
  },
  text_replace: {
    title: "置換（正規表現）",
    fixed: { accept_mime: "text/*" },
    fields: [
      { key: "pattern", label: "検索", type: "text", placeholder: "置換対象の正規表現（例: \\s+）" },
      {
        key: "replacement",
        label: "置換後",
        type: "text",
        optional: true,
        placeholder: "置き換える文字列（空欄=削除・$1 で後方参照）",
        hint: "マッチ部分をこの文字列に置き換えます。$1, $2 で正規表現のグループを参照できます。",
      },
    ],
  },
  // ── v1.5.12 P1: Unix標準テキストフィルタ（標準フィルタ充実検討 §3）。
  text_head: {
    title: "先頭N行（head）",
    fixed: { accept_mime: "text/*" },
    fields: [
      { key: "lines", label: "行数", type: "number", default: 10, min: 1, hint: "先頭からこの行数だけを残します。" },
    ],
  },
  text_tail: {
    title: "末尾N行（tail）",
    fixed: { accept_mime: "text/*" },
    fields: [
      { key: "lines", label: "行数", type: "number", default: 10, min: 1, hint: "末尾からこの行数だけを残します。" },
    ],
  },
  text_sort: {
    title: "行の並べ替え（sort）",
    fixed: { accept_mime: "text/*" },
    fields: [
      {
        key: "order",
        label: "順序",
        type: "select",
        default: "asc",
        options: [
          { value: "asc", label: "昇順" },
          { value: "desc", label: "降順" },
        ],
      },
      {
        key: "compare",
        label: "比較",
        type: "select",
        default: "text",
        options: [
          { value: "text", label: "文字列（辞書順）" },
          { value: "number", label: "数値（行頭の数値で比較・sort -n）" },
        ],
      },
      {
        key: "unique",
        label: "重複行",
        type: "select",
        default: "keep",
        options: [
          { value: "keep", label: "残す" },
          { value: "remove", label: "除去する（sort -u）" },
        ],
      },
    ],
  },
  text_count: {
    title: "行数・単語数・文字数（wc）",
    fixed: { accept_mime: "text/*" },
    fields: [
      {
        key: "mode",
        label: "数えるもの",
        type: "select",
        default: "all",
        options: [
          { value: "all", label: "全部（行数・単語数・文字数）" },
          { value: "lines", label: "行数だけ" },
          { value: "words", label: "単語数だけ" },
          { value: "chars", label: "文字数だけ" },
        ],
        hint: "「〜だけ」は数値のみを出力します（後段の計算・比較に使えます）。",
      },
    ],
  },
  text_cut: {
    title: "列の取り出し（cut）",
    fixed: { accept_mime: "text/*" },
    fields: [
      {
        key: "delimiter",
        label: "区切り",
        type: "select",
        default: "tab",
        options: [
          { value: "tab", label: "タブ（TSV）" },
          { value: "comma", label: "カンマ（CSV）" },
          { value: "space", label: "空白（連続空白をまとめる）" },
        ],
        hint: "その他の区切り文字は「JSONで編集」から delimiter に直接指定できます。",
      },
      {
        key: "fields",
        label: "取り出す列",
        type: "text",
        placeholder: "例: 1,3-5 / 2-（2列目から末尾）",
        hint: "1始まり。カンマ区切りで複数、-で範囲。出力は指定した順に並びます。",
      },
    ],
  },
  // --- v1.5.17 P2: 集約系（複数入力を1回で受ける。入力順=矢印の接続順）---
  text_concat: {
    title: "連結（cat）",
    fixed: { accept_mime: "text/*" },
    fields: [
      {
        key: "separator",
        label: "入力の間",
        type: "select",
        default: "newline",
        options: [
          { value: "newline", label: "改行で区切る（行の融合を防ぐ）" },
          { value: "none", label: "そのまま連結（純粋な cat）" },
          { value: "blank", label: "空行を挟む" },
          { value: "rule", label: "区切り線 --- を挟む（Markdown）" },
        ],
      },
    ],
    note: "接続したすべてのテキスト入力を順に連結します。順序は矢印の接続順です。",
  },
  text_diff: {
    title: "差分（diff）",
    fixed: { accept_mime: "text/*" },
    fields: [
      {
        key: "lines",
        label: "前後の文脈行数",
        type: "number",
        default: 3,
        min: 0,
        hint: "unified diff（diff -u 相当）で、変更行の前後に見せる行数。",
      },
    ],
    note: "入力はちょうど2つ。接続順が 入力1=旧・入力2=新 になります。同一なら出力は空です。",
  },
  text_paste: {
    title: "行の横結合（paste）",
    fixed: { accept_mime: "text/*" },
    fields: [
      {
        key: "delimiter",
        label: "区切り",
        type: "select",
        default: "tab",
        options: [
          { value: "tab", label: "タブ（TSV）" },
          { value: "comma", label: "カンマ（CSV）" },
          { value: "space", label: "空白" },
        ],
        hint: "その他の区切り文字は「JSONで編集」から delimiter に直接指定できます。",
      },
    ],
    note: "各入力の同じ行番号を横に並べて結合します（列の組み立て）。順序は矢印の接続順、行数が違う入力は空欄で埋めます。",
  },
  speak: {
    // v1.5.15 L2 OSアダプタ・ビルトイン第1弾（標準フィルタ充実検討 §4）。
    title: "読み上げ（speak）",
    fixed: { accept_mime: "text/*" },
    fields: [],
    note: "テキストを OS の音声合成で読み上げます（Windows=SAPI / macOS=say / Linux=espeak）。読み上げるだけで完結し、出力カードは作りません。",
  },
  import_new: {
    // v1.6: フォルダの未登録ファイルをカードとして登録（ウォッチャとの対で「随時登録」）。
    title: "新規カードを登録",
    fixed: {},
    fields: [
      {
        key: "path",
        label: "対象フォルダ",
        type: "text",
        optional: true,
        placeholder: "空 = プロジェクトフォルダ直下",
        hint: "プロジェクト内のファイルは実身、外部フォルダのファイルは窓（仮身）として登録します。ウォッチャの監視パスと同じにすると観測→登録が繋がります。",
      },
    ],
    note: "フォルダ直下の未登録ファイルをカードとして登録します（登録済み・一時ファイルはスキップ・このカードの下に並びます）。ウォッチャのリクエスト矢印の先に置くと、新しいファイルが現れるたびに自動で取り込みます。出力カードは作りません。",
  },
  text_convert: {
    title: "文字コード変換（→ UTF-8）",
    fixed: { accept_mime: "text/*" },
    fields: [
      {
        key: "encoding",
        label: "入力の文字コード",
        type: "select",
        default: "auto",
        options: [
          { value: "auto", label: "自動判定" },
          { value: "shift_jis", label: "Shift_JIS（CP932）" },
          { value: "euc-jp", label: "EUC-JP" },
          { value: "iso-2022-jp", label: "JIS（ISO-2022-JP）" },
          { value: "utf-16le", label: "UTF-16LE" },
          { value: "utf-16be", label: "UTF-16BE" },
        ],
        hint: "出力は常に UTF-8。nkf 相当（外部プログラム不要）。",
      },
    ],
  },
  img_resize: {
    title: "画像リサイズ（幅指定）",
    fixed: { accept_mime: "image/*" },
    fields: [
      {
        key: "width",
        label: "幅 (px)",
        type: "number",
        default: 800,
        min: 1,
        hint: "指定した幅に合わせ、高さはアスペクト比を保って自動。出力は PNG。",
      },
    ],
  },
  img_convert: {
    title: "画像 形式変換",
    fixed: { accept_mime: "image/*" },
    fields: [
      {
        key: "format",
        label: "出力形式",
        type: "select",
        default: "png",
        options: [
          { value: "png", label: "PNG" },
          { value: "jpeg", label: "JPEG" },
          { value: "webp", label: "WebP" },
          { value: "bmp", label: "BMP" },
          { value: "gif", label: "GIF" },
        ],
      },
    ],
  },
  md_codeblock: {
    title: "コードブロック抽出（Markdown → ソース）",
    fixed: { accept_mime: "text/*" },
    fields: [
      {
        key: "lang",
        label: "言語",
        type: "text",
        optional: true,
        placeholder: "空欄=最初のブロック（例: c / py / rust）",
        hint: "指定した言語のコードフェンス(```lang)を取り出します。空欄なら最初のブロック。",
      },
      {
        key: "output_ext",
        label: "出力拡張子",
        type: "text",
        optional: true,
        placeholder: "空欄=言語から自動推定（c→.c 等）",
        hint: "出力ファイルの拡張子を明示します。空欄なら言語トークンから推定。",
      },
    ],
  },
  // v1.5.7 セレクタ設定パネル: 下流に送る情報（出力名・エラー時メッセージ）を宣言できる。
  // flow_run.rs のセレクタ分岐（NodeKind::Select）が prefix / fail_text を読む。
  select: {
    title: "セレクタ（最初の有データ枝を採用）",
    fixed: { accept_mime: "*/*" },
    note: "複数の入力枝のうち「最初にデータを返した枝」を採用して下流へ流します（実行はしません）。枝の順序は右クリック「入力の順序」で変えられます。",
    fields: [
      {
        key: "prefix",
        label: "出力名",
        type: "text",
        optional: true,
        placeholder: "空欄=入力の名前をそのまま通す（例: fibo.exe）",
        hint: "採用したデータをこの名前で下流へ送ります（拡張子付きも可）。このカード自身を▶実行すると、この名前の固定ファイルとして毎回上書きされます。後段に別の終端を置く場合は、その終端の命名規則が最後に適用されます。",
      },
      {
        key: "fail_text",
        label: "エラー時メッセージ",
        type: "text",
        optional: true,
        placeholder: "空欄=枝が運ぶエラーテキストをそのまま流す",
        hint: "失敗時矢印の枝を採用したとき、コンパイラ等のエラーテキストの代わりにこの文を下流へ送ります。",
      },
    ],
  },
  // ── v1.5.4 WS-P2 オペレーション（op カード・kind='op'）。builtin ではないが、
  //    「スキーマからフォームを自動生成して update_filter/create_filter で保存」という
  //    仕組みは同じなのでここに同居させる（キーは "op_<種別>"）。
  op_trigger: {
    title: "トリガ（設置型▶・一括発行）",
    opCard: true,
    fixed: { op: "trigger" },
    note: "▶を押すと、このカードから引いたリクエスト矢印（破線）の先のフィルタを実行します。複数につなげば一括発行。設定項目はありません（名前だけ変えられます）。",
    fields: [],
  },
  // v1.5.6 WS-M M1（§9）: IN/OUT バーチャルカード（設定項目なし・名前だけ変えられる）。
  op_in: {
    title: "IN（モジュールの入口）",
    opCard: true,
    fixed: { op: "in" },
    note: "データの入口（素通し）。外から来るフロー矢印をここに受け、中のフィルタへ流します。モジュールの差し替え可能な口です。設定項目はありません。",
    fields: [],
  },
  op_out: {
    title: "OUT（モジュールの出口・▶実行）",
    opCard: true,
    fixed: { op: "out" },
    note: "データの出口。▶で上流をさかのぼって実行し、結果カードが横に生まれます（何度でも押せる口）。設定項目はありません。",
    fields: [],
  },
  op_timer: {
    title: "タイマ（周期・時刻で発行）",
    opCard: true,
    fixed: { op: "timer" },
    note: "リクエスト矢印の先のフィルタを、周期または時刻で自動実行します。every か at のどちらかを設定してください。作成直後は一時停止の状態です — 配線が終わったらカードの「▶ 稼働開始」で動かします。",
    fields: [
      {
        key: "every",
        label: "周期（秒）",
        type: "number",
        optional: true,
        min: 1,
        placeholder: "例: 300（5分ごと）",
        hint: "この秒数ごとに発行します。時刻指定（at）だけを使う場合は空欄に。",
      },
      {
        key: "at",
        label: "時刻 (HH:MM)",
        type: "text",
        optional: true,
        placeholder: "例: 07:30（毎日その時刻に1回）",
        hint: "ローカル時刻。1日1回発行します。周期（every）と併用も可能。",
      },
      {
        key: "min_interval",
        label: "発火間隔の下限（秒）",
        type: "number",
        default: 5,
        min: 0,
        hint: "ヒューズ: これより短い間隔では発火しません。",
      },
      {
        key: "max_fires",
        label: "累計発火数の上限",
        type: "number",
        default: 100,
        min: 0,
        hint: "ヒューズ: 累計がこの回数に達したら止まります（0=無制限。無制限は明示設定のみ）。",
      },
    ],
  },
  op_watch: {
    title: "ウォッチャ（ファイル監視で発行）",
    opCard: true,
    fixed: { op: "watch" },
    note: "パスを監視し、変化が止まって静穏時間が経ったらリクエスト矢印の先のフィルタを実行します（ポーリング方式）。作成直後は一時停止の状態です — 配線が終わったらカードの「▶ 稼働開始」で動かします（再開時は今の状態を基準に監視します）。",
    fields: [
      {
        key: "path",
        label: "監視パス",
        type: "text",
        placeholder: "監視するファイルまたはフォルダのフルパス",
        hint: "フォルダを指定すると直下のファイルの作成・更新を監視します。",
      },
      {
        key: "events",
        label: "反応するイベント",
        type: "select",
        asList: true,
        default: "create,modify",
        options: [
          { value: "create,modify", label: "作成・更新の両方" },
          { value: "create", label: "作成のみ（新しいファイルが現れたら）" },
          { value: "modify", label: "更新のみ（中身が変わったら）" },
        ],
      },
      {
        key: "debounce",
        label: "静穏時間（秒）",
        type: "number",
        default: 2,
        min: 0,
        hint: "変化が止まってからこの秒数待って発火します（書き込み途中のファイルを掴まない）。",
      },
      {
        key: "min_interval",
        label: "発火間隔の下限（秒）",
        type: "number",
        default: 5,
        min: 0,
        hint: "ヒューズ: これより短い間隔では発火しません（自己発火ループ対策）。",
      },
      {
        key: "max_fires",
        label: "累計発火数の上限",
        type: "number",
        default: 100,
        min: 0,
        hint: "ヒューズ: 累計がこの回数に達したら止まります（0=無制限。無制限は明示設定のみ）。",
      },
    ],
  },
};

/** この builtin がダイアログ対応か（未対応なら呼び出し側で従来の JSON 編集へフォールバック）。 */
export function hasBuiltinSchema(builtin: string): boolean {
  return builtin in BUILTIN_SCHEMAS;
}

interface Props {
  builtin: string;
  /** 編集時はカードの現在の表示名、作成時は既定名。 */
  initialLabel?: string;
  /** 編集時は filter_ref の JSON、作成時は undefined。 */
  initialSpec?: string;
  onSave: (label: string, specJson: string) => void;
  onCancel: () => void;
}

/** スキーマ＋spec から各フィールドの初期文字列値を作る（JSON⇄フォーム往復でも共用）。 */
function initVals(schema: BuiltinSchema | undefined, base: Record<string, unknown>): Record<string, string> {
  const v: Record<string, string> = {};
  for (const f of schema?.fields ?? []) {
    const cur = base[f.key];
    if (typeof cur === "string" || typeof cur === "number") v[f.key] = String(cur);
    // v1.5.4 WS-P2: 配列値（events 等）はカンマ区切りへ（asList の往復）。
    else if (Array.isArray(cur)) v[f.key] = cur.join(",");
    else if (f.default !== undefined) v[f.key] = String(f.default);
    else v[f.key] = "";
  }
  return v;
}

export function BuiltinFilterDialog({ builtin, initialLabel, initialSpec, onSave, onCancel }: Props) {
  const schema = BUILTIN_SCHEMAS[builtin];

  const parseSpec = (s?: string): Record<string, unknown> => {
    try {
      return s ? (JSON.parse(s) as Record<string, unknown>) : {};
    } catch {
      return {};
    }
  };

  const [title, setTitle] = useState(initialLabel ?? "");
  // 元 spec を土台に保持（未知キーの後方互換）。JSON編集で書き換わり得るので state。
  const [base, setBase] = useState<Record<string, unknown>>(() => parseSpec(initialSpec));
  const [vals, setVals] = useState<Record<string, string>>(() => initVals(schema, parseSpec(initialSpec)));
  // v1.4.29 ⑤+: 「JSONで編集」モード。フォームでは届かない機微な設定を生JSONで直接編集できる。
  const [jsonMode, setJsonMode] = useState(false);
  const [jsonText, setJsonText] = useState("");
  const [jsonErr, setJsonErr] = useState<string | null>(null);

  if (!schema) return null;

  const setVal = (k: string, val: string) => setVals((prev) => ({ ...prev, [k]: val }));

  // 元 spec を土台に、builtin・固定キー・既知フィールドを上書き（未知キーは温存）。
  // v1.5.4 WS-P2: op カードは builtin キーを持たない（op キーが種別・fixed が付与する）。
  const buildSpec = (): Record<string, unknown> => {
    const spec: Record<string, unknown> = schema.opCard
      ? { ...base, ...schema.fixed }
      : { ...base, builtin, ...schema.fixed };
    for (const f of schema.fields) {
      const raw = (vals[f.key] ?? "").trim();
      if (f.asList) {
        // カンマ区切り → 文字列配列（events 等）。空欄はキーごと省く。
        const list = raw.split(",").map((s) => s.trim()).filter(Boolean);
        if (list.length) spec[f.key] = list;
        else delete spec[f.key];
      } else if (f.type === "number") {
        const n = parseInt(raw, 10);
        if (Number.isFinite(n)) spec[f.key] = Math.max(f.min ?? 0, n);
        else if (f.default !== undefined) spec[f.key] = f.default;
        else delete spec[f.key];
      } else if (f.optional && raw === "") {
        delete spec[f.key]; // 空欄は「未指定」＝キーごと省く
      } else {
        spec[f.key] = raw;
      }
    }
    return spec;
  };

  // フォーム → JSON。現在のフォーム値を整形JSONにして編集モードへ。
  const enterJson = () => {
    setJsonText(JSON.stringify(buildSpec(), null, 2));
    setJsonErr(null);
    setJsonMode(true);
  };

  // JSON → フォーム。パースできたら base とフォーム値へ取り込む（未知キーも base に残る）。
  const exitJson = () => {
    let parsed: Record<string, unknown>;
    try {
      parsed = JSON.parse(jsonText) as Record<string, unknown>;
    } catch (e) {
      setJsonErr(jsonMessage(e));
      return;
    }
    setBase(parsed);
    setVals(initVals(schema, parsed));
    setJsonErr(null);
    setJsonMode(false);
  };

  const onJsonChange = (t: string) => {
    setJsonText(t);
    try {
      JSON.parse(t);
      setJsonErr(null);
    } catch (e) {
      setJsonErr(jsonMessage(e));
    }
  };

  const save = () => {
    const label = title.trim() || initialLabel || schema.title;
    if (jsonMode) {
      let parsed: Record<string, unknown>;
      try {
        parsed = JSON.parse(jsonText) as Record<string, unknown>;
      } catch (e) {
        setJsonErr(jsonMessage(e));
        return;
      }
      onSave(label, JSON.stringify(parsed));
      return;
    }
    onSave(label, JSON.stringify(buildSpec()));
  };

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
          width: 480,
          maxHeight: "85vh",
          overflowY: "auto",
          boxShadow: "0 8px 32px #000000aa",
          fontFamily: "sans-serif",
          color: "var(--panel-text, #d6dded)",
          fontSize: 13,
        }}
      >
        <div style={{ display: "flex", alignItems: "baseline", justifyContent: "space-between", marginBottom: 4 }}>
          <div style={{ fontSize: 15 }}>{schema.opCard ? "オペレーションカードの設定" : "ビルトインフィルタの設定"}</div>
          {/* v1.4.29 ⑤+: フォーム⇄JSON の切替。JSON は機微な設定（フォーム外のキー）用。 */}
          <button
            style={{ ...smallBtn(), fontSize: 11 }}
            title={jsonMode ? "編集内容をフォームに取り込みます（不正な JSON のときは戻れません）" : "現在の設定を生 JSON で直接編集します（フォームにない項目も設定できます）"}
            onClick={jsonMode ? exitJson : enterJson}
          >
            {jsonMode ? "フォームに戻す ⤴" : "JSONで編集 ⤵"}
          </button>
        </div>
        <div style={{ fontSize: 12, marginBottom: 16, color: "var(--panel-muted, #8a93a8)" }}>
          {schema.title}
          {schema.note && (
            <div style={{ marginTop: 6, lineHeight: 1.6 }}>{schema.note}</div>
          )}
        </div>

        {row(
          "カード名",
          <input
            style={inputStyle()}
            value={title}
            onChange={(e) => setTitle(e.target.value)}
            placeholder={schema.title}
          />
        )}

        {jsonMode ? (
          <div>
            <textarea
              style={{
                ...inputStyle(),
                width: "100%",
                minHeight: 160,
                resize: "vertical",
                fontFamily: "Consolas, monospace",
                lineHeight: 1.5,
                whiteSpace: "pre",
                overflowWrap: "normal",
              }}
              spellCheck={false}
              value={jsonText}
              onChange={(e) => onJsonChange(e.target.value)}
            />
            <div style={{ fontSize: 11, margin: "4px 2px 0", color: jsonErr ? "#d96a6a" : "var(--panel-muted, #8a93a8)" }}>
              {jsonErr ? `✗ JSON エラー: ${jsonErr}` : "フォーム外のキーも設定できます（builtin キーは変更しないでください）。"}
            </div>
          </div>
        ) : (
          schema.fields.map((f) => (
            <div key={f.key}>
              {row(
                f.label,
                f.type === "select" ? (
                  <select style={inputStyle()} value={vals[f.key] ?? ""} onChange={(e) => setVal(f.key, e.target.value)}>
                    {f.options?.map((o) => (
                      <option key={o.value} value={o.value}>
                        {o.label}
                      </option>
                    ))}
                  </select>
                ) : (
                  <input
                    style={f.type === "number" ? { ...inputStyle(), width: 120 } : inputStyle()}
                    type={f.type === "number" ? "number" : "text"}
                    min={f.min}
                    value={vals[f.key] ?? ""}
                    onChange={(e) => setVal(f.key, e.target.value)}
                    placeholder={f.placeholder}
                  />
                )
              )}
              {f.hint && (
                <div style={{ fontSize: 11, color: "var(--panel-muted, #8a93a8)", margin: "-4px 0 10px 92px" }}>
                  {f.hint}
                </div>
              )}
            </div>
          ))
        )}

        <div style={{ display: "flex", justifyContent: "flex-end", gap: 8, marginTop: 16 }}>
          <button onClick={onCancel} style={btnStyle("#2a3346", "#aeb8cc")}>
            キャンセル
          </button>
          <button
            onClick={save}
            disabled={jsonMode && !!jsonErr}
            style={btnStyle(jsonMode && jsonErr ? "#2a3346" : "#3a6fb0", "#ffffff")}
          >
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
      <div style={{ width: 82, flex: "none", textAlign: "right", color: "var(--panel-muted, #aeb8cc)", fontSize: 12 }}>
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
    background: "var(--panel-input-bg, #0e121c)",
    border: "1px solid var(--panel-border, #38445e)",
    borderRadius: 6,
    color: "var(--panel-text, #eef2f9)",
    fontSize: 13,
    outline: "none",
    boxSizing: "border-box",
  };
}

function smallBtn(): React.CSSProperties {
  return {
    padding: "5px 10px",
    background: "var(--panel-btn-bg, #2a3346)",
    color: "var(--panel-text, #aeb8cc)",
    border: "1px solid var(--panel-border, #38445e)",
    borderRadius: 6,
    cursor: "pointer",
    fontSize: 12,
    fontFamily: "sans-serif",
    flex: "none",
  };
}

/** JSON.parse の例外を短い日本語向けメッセージに（SyntaxError の message をそのまま出す）。 */
function jsonMessage(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
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
