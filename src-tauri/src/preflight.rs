use crate::*;
use rusqlite::OptionalExtension;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use tauri::State;

// ---------------------------------------------------------------------------
// 運用前評価コマンド (v1.5.5 WS-P2 B) — 正本 = オペレーション層設計.md §6.2
//
// 終端カード右クリック「運用前評価」。検査対象＝そこから上流へ pull で到達する部分グラフ
// （＝実行と同じ到達集合のドライラン）。実行はしない静的トラバーサル。結果は固定名 MD
// レポートカード（「<カード名>の評価.md」上書き・upsert_chat_log_card の流儀）。
// キャンバス全体の一括評価は作らない（明示指定の原則）。
// ---------------------------------------------------------------------------

/// 検査の重大度（§6.2 の「級」）。
#[derive(Clone, Copy, PartialEq)]
enum Level {
    Error,
    Warning,
    Info,
}

struct Finding {
    level: Level,
    category: &'static str,
    message: String,
}

/// 到達集合の1ノード（Project 借用を持たない所有データ。ロック外で検査するため）。
struct Node {
    id: String,
    label: String,
    kind: String,
    real_id: String,
    filter_ref: Option<String>,
    mime: String,
}

impl Node {
    fn is_filter(&self) -> bool {
        self.kind == "filter" || self.filter_ref.is_some()
    }
    fn spec(&self) -> Option<flow::FilterSpec> {
        self.filter_ref
            .as_deref()
            .and_then(|r| flow::parse_spec(r).ok())
    }
}

/// 到達集合の1エッジ（flow のみ・attrs 付き）。
struct Edge {
    from: String,
    to: String,
    attrs: flow::EdgeAttrs,
}

/// 到達集合へ req で発行する op カード（トリガ/タイマ/ウォッチャ）1件。
struct OpCard {
    label: String,
    spec: ops::OpSpec,
}

/// 収集した部分グラフ（ロック内で作り、以後はロックなしで検査する）。
struct Graph {
    nodes: HashMap<String, Node>,
    edges: Vec<Edge>,
    /// 到達集合に req で発行しようとしている op カード（自己発火・ヒューズ検査用）。
    ops_into: Vec<OpCard>,
    /// プロジェクト内で req ターゲットを持たない op カード（未接続の情報項目用・ラベル）。
    dangling_ops: Vec<String>,
    /// v1.5.6 WS-M M1: フロー矢印が1本も繋がれていない IN/OUT バーチャルカード（ラベル）。
    dangling_ios: Vec<String>,
    /// v1.5.5 検証修正#5: 仮身でないもの（付箋など）からのフロー矢印が入っているカード id。
    /// キャンバスでは赤矢印警告になるが collect の JOIN では落ちるため、別枠で拾う。
    bad_inputs: Vec<String>,
}

#[derive(Serialize)]
pub(crate) struct EvalSummary {
    errors: usize,
    warnings: usize,
    infos: usize,
    /// 生成/更新したレポートカードの id（フロントは refresh して緑シグナルを付ける）。
    report_id: String,
}

/// 1ノード分を DB から読む（到達集合へ格納する所有データ化）。
fn load_node(p: &Project, id: &str) -> Option<Node> {
    p.conn
        .query_row(
            "SELECT v.id, v.label, v.kind, v.real_id,
                    COALESCE(v.mime_type, r.mime_type, ''), v.filter_ref
             FROM virtual_bodies v
             JOIN real_bodies r ON r.id = v.real_id
             WHERE v.id = ?1 AND v.deleted_at IS NULL",
            rusqlite::params![id],
            |row| {
                Ok(Node {
                    id: row.get(0)?,
                    label: row.get(1)?,
                    kind: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    real_id: row.get(3)?,
                    mime: row.get(4)?,
                    filter_ref: row.get(5)?,
                })
            },
        )
        .optional()
        .ok()
        .flatten()
}

/// 終端カードから上流へ pull 到達（eval_filter の入力収集クエリと同じ kind='flow'）。
/// フィルタカードだけを辿り（データカードは葉）、ノードとエッジを所有データで集める。
fn collect(p: &Project, terminal: &str) -> Result<Graph, String> {
    let mut nodes: HashMap<String, Node> = HashMap::new();
    let mut edges: Vec<Edge> = Vec::new();
    let mut bad_inputs: Vec<String> = Vec::new();
    let root = load_node(p, terminal).ok_or("終端カードが見つかりません")?;
    let mut stack = vec![root.id.clone()];
    nodes.insert(root.id.clone(), root);
    let mut expanded: HashSet<String> = HashSet::new();

    while let Some(cur) = stack.pop() {
        if !expanded.insert(cur.clone()) {
            continue;
        }
        // 現ノードがフィルタでなければ葉（データカード）＝入力を辿らない。
        let is_filter = nodes.get(&cur).map(|n| n.is_filter()).unwrap_or(false);
        if !is_filter {
            continue;
        }
        // 入力（flow 矢印・生きているカード）: eval_filter と同じクエリ。実行はしない。
        let mut stmt = p
            .conn
            .prepare(
                "SELECT rel.from_id, rel.attrs
                 FROM relations rel
                 JOIN virtual_bodies v ON v.id = rel.from_id AND v.deleted_at IS NULL
                 WHERE rel.to_id = ?1 AND rel.kind = 'flow' AND rel.deleted_at IS NULL
                 ORDER BY rel.created_at",
            )
            .map_err(|e| e.to_string())?;
        let ins: Vec<(String, Option<String>)> = stmt
            .query_map(rusqlite::params![cur], |r| {
                Ok((r.get(0)?, r.get::<_, Option<String>>(1)?))
            })
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect();
        for (from, attrs) in ins {
            edges.push(Edge {
                from: from.clone(),
                to: cur.clone(),
                attrs: flow::parse_edge_attrs(attrs.as_deref()),
            });
            if !nodes.contains_key(&from) {
                if let Some(n) = load_node(p, &from) {
                    nodes.insert(from.clone(), n);
                }
            }
            if !expanded.contains(&from) {
                stack.push(from);
            }
        }
        // v1.5.5 検証修正#5: 仮身に存在しない入力元（付箋など）からの flow 矢印。
        // 上の JOIN では行ごと落ちる＝キャンバスの赤矢印（invalidFlowIds）と検出範囲を揃える。
        // ソフト削除されたカードからの矢印は UI でも非表示なので対象外（NOT EXISTS は
        // deleted 込みの全行と照合）。
        let bad: i64 = p
            .conn
            .query_row(
                "SELECT COUNT(*) FROM relations rel
                 WHERE rel.to_id = ?1 AND rel.kind = 'flow' AND rel.deleted_at IS NULL
                   AND NOT EXISTS (SELECT 1 FROM virtual_bodies vb WHERE vb.id = rel.from_id)",
                rusqlite::params![cur],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if bad > 0 {
            bad_inputs.push(cur.clone());
        }
    }

    // op カード（トリガ/タイマ/ウォッチャ）と req ターゲットを読む。
    let (ops_into, dangling_ops, dangling_ios) = collect_ops(p, &nodes);
    Ok(Graph {
        nodes,
        edges,
        ops_into,
        dangling_ops,
        dangling_ios,
        bad_inputs,
    })
}

/// op カードを読み、①到達集合へ発行する op（自己発火・ヒューズ検査用）②ターゲット無しの
/// dangling op ③フロー未接続の IN/OUT（どちらも未接続の情報項目用）に分ける。
fn collect_ops(
    p: &Project,
    reachable: &HashMap<String, Node>,
) -> (Vec<OpCard>, Vec<String>, Vec<String>) {
    let mut ops_into = Vec::new();
    let mut dangling = Vec::new();
    let mut dangling_ios = Vec::new();
    let mut stmt = match p.conn.prepare(
        "SELECT id, label, filter_ref FROM virtual_bodies
         WHERE kind = 'op' AND deleted_at IS NULL AND filter_ref IS NOT NULL",
    ) {
        Ok(s) => s,
        Err(_) => return (ops_into, dangling, dangling_ios),
    };
    let rows: Vec<(String, String, String)> = match stmt.query_map([], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    }) {
        Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
        Err(_) => return (ops_into, dangling, dangling_ios),
    };
    for (id, label, spec_json) in rows {
        let Ok(spec) = ops::parse_op_spec(&spec_json) else {
            continue;
        };
        // v1.5.6 WS-M M1: IN/OUT はデータレーンの住人＝req ターゲットの概念が無い。
        // フロー矢印が1本も無いものを「未接続」情報として拾う（§6.2 の表の後半）。
        if spec.op == "in" || spec.op == "out" {
            let flows: i64 = p
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM relations rel
                     WHERE (rel.from_id = ?1 OR rel.to_id = ?1)
                       AND rel.kind = 'flow' AND rel.deleted_at IS NULL",
                    rusqlite::params![id],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            if flows == 0 {
                dangling_ios.push(label);
            }
            continue;
        }
        // req ターゲット（生きているフィルタカード）を集める。
        let targets: Vec<String> = p
            .conn
            .prepare(
                "SELECT v.id FROM relations rel
                 JOIN virtual_bodies v ON v.id = rel.to_id AND v.deleted_at IS NULL
                 WHERE rel.from_id = ?1 AND rel.kind = 'req' AND rel.deleted_at IS NULL",
            )
            .and_then(|mut s| {
                s.query_map(rusqlite::params![id], |r| r.get::<_, String>(0))
                    .map(|rows| rows.filter_map(|r| r.ok()).collect())
            })
            .unwrap_or_default();
        if targets.is_empty() {
            dangling.push(label);
        } else if targets.iter().any(|t| reachable.contains_key(t)) {
            ops_into.push(OpCard { label, spec });
        }
    }
    (ops_into, dangling, dangling_ios)
}

/// filterOutputMime（front model.ts）の Rust 版。外部コマンドは出力 MIME 不明で None。
fn filter_output_mime(spec: &flow::FilterSpec) -> Option<String> {
    match spec.builtin.as_deref() {
        // v1.5.12 P1: Unix標準テキストフィルタ群（head/tail/sort/count/cut/convert）も text/plain。
        // v1.5.17 P2: 集約系（concat/diff/paste）も text/plain（diff の実体拡張子は .diff）。
        Some("text_grep") | Some("text_replace") | Some("md_codeblock") | Some("text_head")
        | Some("text_tail") | Some("text_sort") | Some("text_count") | Some("text_cut")
        | Some("text_convert") | Some("text_concat") | Some("text_diff") | Some("text_paste") => {
            Some("text/plain".into())
        }
        Some("ai") => Some("text/markdown".into()),
        Some("t2i") | Some("img_resize") => Some("image/png".into()),
        // v2.0 t23d は常に glb を吐く
        Some("t23d") => Some("model/gltf-binary".into()),
        // v1.7 P1: web_fetch は通常 text/markdown だが、text/html 以外の直リンクは
        // raw のまま実身化する（画像/csv/json…）ため静的には不明＝チェックしない。
        Some("web_fetch") => None,
        // v1.7 P3: web_feed は常に記事カード（Markdown）を吐く。
        Some("web_feed") => Some("text/markdown".into()),
        // v1.7 P4: web_capture は常にスクリーンショット（PNG）を吐く。
        Some("web_capture") => Some("image/png".into()),
        // v1.7 P5: to_url のペイロードは URL 文字列（text/plain）— 下流に繋げば
        // web_fetch 等の入力になり、終端なら URL カードとして着地する。
        Some("to_url") => Some("text/plain".into()),
        Some("img_convert") => {
            let f = spec.format.as_deref().unwrap_or("png").to_lowercase();
            Some(if f == "jpg" || f == "jpeg" {
                "image/jpeg".into()
            } else {
                format!("image/{f}")
            })
        }
        _ => None,
    }
}

/// パス正規化（比較用・末尾区切りとスラッシュ向きを揃え小文字化）。canonicalize は
/// 実在しないパスで失敗するので使わない（ドライラン＝出力先はまだ無いこともある）。
fn norm_path(s: &str) -> String {
    s.trim()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_lowercase()
}

/// a が b を包含する（a==b もしくは b が a の配下）か。
fn path_covers(a: &str, b: &str) -> bool {
    let (a, b) = (norm_path(a), norm_path(b));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    b == a || b.starts_with(&format!("{a}/"))
}

/// 全検査を回して findings を集める（DB 非依存＝ロック外で実行可）。
/// `project_root` は自己発火検査（out_dir 未指定＝プロジェクト直下へ materialize）に、
/// `app_data`・`filters_dir` は依存未解決の検査（外部プログラム・AI・t2i）に使う。
fn inspect(g: &Graph, project_root: &Path, app_data: &Path, filters_dir: &Path) -> Vec<Finding> {
    let mut f = Vec::new();
    let label = |id: &str| -> String {
        g.nodes
            .get(id)
            .map(|n| n.label.clone())
            .unwrap_or_else(|| id.to_string())
    };
    // 入力本数（to ごと）。
    let mut in_count: HashMap<&str, usize> = HashMap::new();
    for e in &g.edges {
        *in_count.entry(e.to.as_str()).or_insert(0) += 1;
    }

    // 1) データ循環（エラー）: flow 矢印の閉路。3色 DFS。
    if let Some(cycle) = find_cycle(g) {
        let names: Vec<String> = cycle.iter().map(|id| label(id)).collect();
        f.push(Finding {
            level: Level::Error,
            category: "データ循環",
            message: format!("フロー矢印が循環しています: {}", names.join(" → ")),
        });
    }

    // 2) {inN} 超過・入力0（エラー）
    for n in g.nodes.values() {
        let Some(spec) = n.spec() else { continue };
        let count = *in_count.get(n.id.as_str()).unwrap_or(&0);
        let kind = spec.node_kind();
        // {inN} 超過（外部コマンドの位置指定が入力本数を超える）
        if kind == flow::NodeKind::External {
            if let Some(max_n) = max_in_placeholder(spec.cmd.as_deref().unwrap_or("")) {
                if max_n > count {
                    f.push(Finding {
                        level: Level::Error,
                        category: "入力不足",
                        message: format!(
                            "「{}」は {{in{}}} を参照しますが、入力は {} 本しかありません",
                            n.label, max_n, count
                        ),
                    });
                }
            }
        }
        // 入力0（非ジェネレータで入力なし＝処理する材料が無い）
        if count == 0 {
            let needs_input = match kind {
                flow::NodeKind::Generator | flow::NodeKind::Invalid => false,
                flow::NodeKind::T2i => spec.own_prompt().is_none(),
                // v1.7 P1/P3/P4: web_* はパラメータ欄の URL 直書きで単独実行できる。
                flow::NodeKind::Builtin
                    if matches!(
                        spec.builtin.as_deref(),
                        Some("web_fetch") | Some("web_feed") | Some("web_capture")
                    ) =>
                {
                    crate::web::spec_url(&spec).is_none()
                }
                flow::NodeKind::External => {
                    !spec.is_shell() && spec.cmd.as_deref().map(|c| c.contains("{in")).unwrap_or(false)
                }
                _ => !spec.is_shell(),
            };
            if needs_input {
                f.push(Finding {
                    level: Level::Error,
                    category: "入力なし",
                    message: format!("「{}」に入力の矢印がありません（処理する材料がありません）", n.label),
                });
            }
        }
    }

    // 3) 自己発火リスク（警告）: ウォッチャの監視パスが到達下流の出力先(out_dir)と重なる。
    let out_dirs: Vec<(&str, String)> = g
        .nodes
        .values()
        .filter_map(|n| {
            n.spec()
                .and_then(|s| s.out_dir.clone())
                .filter(|d| !d.trim().is_empty())
                .map(|d| (n.label.as_str(), d))
        })
        .collect();
    let root_str = project_root.to_string_lossy();
    for op in &g.ops_into {
        if op.spec.op != "watch" {
            continue;
        }
        let Some(watch) = op.spec.path.as_deref() else { continue };
        for (card_label, dir) in &out_dirs {
            if path_covers(watch, dir) || path_covers(dir, watch) {
                f.push(Finding {
                    level: Level::Warning,
                    category: "自己発火リスク",
                    message: format!(
                        "ウォッチャ「{}」の監視パスが、下流「{}」の出力先と重なっています（発振の恐れ）",
                        op.label, card_label
                    ),
                });
            }
        }
        // v1.5.5 検証修正#3: out_dir 未指定の実行結果は**プロジェクト直下**に materialize
        // される。監視パスがプロジェクトフォルダを含むなら、結果カードの生成で再発火する
        // （§6.2「発振の主犯」の最頻ケース）。直下のみなので、サブフォルダ監視は対象外。
        if !root_str.is_empty() && path_covers(watch, &root_str) {
            f.push(Finding {
                level: Level::Warning,
                category: "自己発火リスク",
                message: format!(
                    "ウォッチャ「{}」の監視パスがプロジェクトフォルダを含んでいます（実行結果は既定でプロジェクト直下に生成されるため、再発火のループになります）",
                    op.label
                ),
            });
        }
    }

    // 4) ヒューズ未設定（警告）: タイマ/ウォッチャの発行上限が無制限に明示変更されている。
    for op in &g.ops_into {
        if (op.spec.op == "timer" || op.spec.op == "watch") && op.spec.max_fires == Some(0) {
            f.push(Finding {
                level: Level::Warning,
                category: "ヒューズ未設定",
                message: format!(
                    "「{}」の発火上限が無制限（max_fires=0）です。暴走時に止まりません",
                    op.label
                ),
            });
        }
    }

    // 5) 型不一致（警告）: accept_mime に合わない入力（接続時赤矢印の一括版）。
    for e in &g.edges {
        let (Some(to), Some(from)) = (g.nodes.get(&e.to), g.nodes.get(&e.from)) else {
            continue;
        };
        if !to.is_filter() {
            continue;
        }
        let accept = to
            .spec()
            .and_then(|s| s.accept_mime)
            .unwrap_or_else(|| "*/*".into());
        let out_mime = if from.is_filter() {
            match from.spec().as_ref().and_then(filter_output_mime) {
                Some(m) => m,
                None => continue, // 外部コマンド等・出力 MIME 不明はチェックしない
            }
        } else {
            if from.kind == "drop" && from.real_id.starts_with("drop:") {
                continue; // 空のドロップポイント（中身が入るまで保留）
            }
            // v1.7 P2: URLカードは url_ref 文字列を text/plain で供給する（実行契約）。
            if from.mime == crate::project::URL_MIME {
                "text/plain".into()
            } else {
                from.mime.clone()
            }
        };
        if !flow::mime_matches(&accept, &out_mime) {
            f.push(Finding {
                level: Level::Warning,
                category: "型不一致",
                message: format!(
                    "「{}」({}) → 「{}」(受入 {}) は型が合いません",
                    from.label, out_mime, to.label, accept
                ),
            });
        }
    }

    // 6) セレクタ全滅可能（警告）: 全 IN に条件が付き always 枝が無い。
    for n in g.nodes.values() {
        let Some(spec) = n.spec() else { continue };
        if spec.node_kind() != flow::NodeKind::Select {
            continue;
        }
        let ins: Vec<&Edge> = g.edges.iter().filter(|e| e.to == n.id).collect();
        if ins.is_empty() {
            continue; // 入力0 は別項目（2）で扱う
        }
        let has_always = ins.iter().any(|e| e.attrs.when.is_none());
        if !has_always {
            f.push(Finding {
                level: Level::Warning,
                category: "セレクタ全滅可能",
                message: format!(
                    "「{}」は全ての入力に条件が付いています（全枝が不成立だと何も採用されません）",
                    n.label
                ),
            });
        }
    }

    // 7) 出力衝突（警告）: 同一固定名（out_dir＋prefix）へ複数フローが書く。
    let mut fixed: HashMap<(String, String), Vec<String>> = HashMap::new();
    for n in g.nodes.values() {
        let Some(spec) = n.spec() else { continue };
        let dir = spec.out_dir.clone().unwrap_or_default();
        if let Some(prefix) = spec.prefix.clone().filter(|s| !s.trim().is_empty()) {
            fixed
                .entry((norm_path(&dir), prefix))
                .or_default()
                .push(n.label.clone());
        }
    }
    for ((_, prefix), labels) in &fixed {
        if labels.len() >= 2 {
            f.push(Finding {
                level: Level::Warning,
                category: "出力衝突",
                message: format!(
                    "接頭辞「{}」で複数のカードが同じ場所へ書き込みます: {}",
                    prefix,
                    labels.join("・")
                ),
            });
        }
    }

    // 8) 依存未解決（警告）: 外部プログラム不在・AI 不達・t2i 環境未構築。
    inspect_dependencies(g, app_data, filters_dir, &mut f);

    // 9) 排他資源の競合可能性（情報）: 同一録音デバイスを使うカード同士。
    let mut devices: HashMap<String, Vec<String>> = HashMap::new();
    for n in g.nodes.values() {
        let Some(spec) = n.spec() else { continue };
        if spec.record {
            if let Some(dev) = flow::dshow_audio_device(spec.cmd.as_deref().unwrap_or("")) {
                devices.entry(dev).or_default().push(n.label.clone());
            }
        }
    }
    for (dev, labels) in &devices {
        if labels.len() >= 2 {
            f.push(Finding {
                level: Level::Info,
                category: "資源競合",
                message: format!(
                    "録音デバイス「{}」を複数のカードが使います（同時実行は排他になります）: {}",
                    dev,
                    labels.join("・")
                ),
            });
        }
    }

    // v1.5.5 稼働スイッチ: 停止中の op（情報）—「なぜ発火しないのか」に評価が答える。
    for op in &g.ops_into {
        if op.spec.paused {
            f.push(Finding {
                level: Level::Info,
                category: "停止中",
                message: format!(
                    "「{}」は一時停止中です（カードの「▶ 稼働開始」を押すまで自動発火しません）",
                    op.label
                ),
            });
        }
    }

    // v1.5.5 検証修正#5: 仮身でないもの（付箋など）からの入力（警告）。
    // キャンバスの赤矢印（invalidFlowIds の「入力にできないノード」）の一括レポート版。
    for id in &g.bad_inputs {
        f.push(Finding {
            level: Level::Warning,
            category: "不正な入力元",
            message: format!(
                "「{}」に仮身でないもの（付箋など）からのフロー矢印が入っています（実行時は入力になりません）",
                label(id)
            ),
        });
    }

    // 10) 未接続（情報）: ターゲットの無い op カード（発行先が繋がれていない）。
    for label in &g.dangling_ops {
        f.push(Finding {
            level: Level::Info,
            category: "未接続",
            message: format!(
                "オペレーション「{}」はリクエスト矢印の先が繋がれていません（発行先がありません）",
                label
            ),
        });
    }
    // v1.5.6 WS-M M1（§6.2 の表の後半）: フロー矢印が無い IN/OUT バーチャルカード。
    for label in &g.dangling_ios {
        f.push(Finding {
            level: Level::Info,
            category: "未接続",
            message: format!(
                "IN/OUT カード「{}」にフロー矢印が繋がれていません（口として機能していません）",
                label
            ),
        });
    }

    f
}

/// 依存未解決の検査（ネットワーク接触あり＝ロック外で呼ぶこと）。
fn inspect_dependencies(g: &Graph, app_data: &Path, filters_dir: &Path, f: &mut Vec<Finding>) {
    let mut checked_endpoints: HashSet<String> = HashSet::new();
    let mut t2i_checked = false;
    let mut t23d_checked = false;
    for n in g.nodes.values() {
        let Some(spec) = n.spec() else { continue };
        match spec.node_kind() {
            flow::NodeKind::External | flow::NodeKind::Generator => {
                // name 指定（新形式）のプログラム所在。cmd のみ（旧形式）は解決対象外。
                if let Some(name) = spec.name.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                    if flow::resolve_program(name, spec.path.as_deref(), filters_dir).is_none() {
                        f.push(Finding {
                            level: Level::Warning,
                            category: "依存未解決",
                            message: format!("「{}」: プログラム「{}」が見つかりません", n.label, name),
                        });
                    }
                }
            }
            flow::NodeKind::Ai => {
                let provider = spec.provider.as_deref().unwrap_or("ollama");
                let endpoint = flow::effective_ai_endpoint(&spec);
                // ollama のみ到達確認（openai 準拠は health 規約が異なるので誤検知を避け保留）。
                if provider == "ollama" && checked_endpoints.insert(endpoint.clone()) {
                    if !ollama_running(&endpoint) {
                        f.push(Finding {
                            level: Level::Warning,
                            category: "依存未解決",
                            message: format!("AI エンドポイント {} に接続できません（Ollama 未起動）", endpoint),
                        });
                    }
                }
                // v1.5.5 検証修正#4: 検査していないものは「検査していない」と報告する
                // （無言スキップはユーザーが検査済みと誤解する）。
                if provider != "ollama" && checked_endpoints.insert(endpoint.clone()) {
                    f.push(Finding {
                        level: Level::Info,
                        category: "未検査",
                        message: format!(
                            "「{}」の {} 準拠エンドポイント（{}）は自動検査の対象外です（起動は手動で確認してください）",
                            n.label, provider, endpoint
                        ),
                    });
                }
            }
            flow::NodeKind::T2i => {
                if !t2i_checked {
                    t2i_checked = true;
                    if !detect_python(app_data).deps_ok {
                        f.push(Finding {
                            level: Level::Warning,
                            category: "依存未解決",
                            message: "画像生成(t2i)の Python 環境が未構築です（依存が揃っていません）".into(),
                        });
                    }
                }
            }
            flow::NodeKind::T23d => {
                if !t23d_checked {
                    t23d_checked = true;
                    if let Err(e) = crate::runtime::t23d::t23d_resolve_engine(app_data) {
                        f.push(Finding {
                            level: Level::Warning,
                            category: "依存未解決",
                            message: format!("3D生成(t23d): {e}"),
                        });
                    }
                }
            }
            _ => {}
        }
    }
}

/// 外部コマンド中の最大 `{inN}` 番号（`{in*}` は入力本数に依存しないので None）。
fn max_in_placeholder(cmd: &str) -> Option<usize> {
    let re = regex::Regex::new(r"\{in(\d+)\}").ok()?;
    re.captures_iter(cmd)
        .filter_map(|c| c[1].parse::<usize>().ok())
        .max()
}

/// 到達集合の flow エッジで有向閉路を探す（3色 DFS）。閉路上のカード列を返す。
fn find_cycle(g: &Graph) -> Option<Vec<String>> {
    // 隣接（from → to）。
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for e in &g.edges {
        adj.entry(e.from.as_str()).or_default().push(e.to.as_str());
    }
    #[derive(Clone, Copy, PartialEq)]
    enum C {
        White,
        Gray,
        Black,
    }
    let mut color: HashMap<&str, C> = g.nodes.keys().map(|k| (k.as_str(), C::White)).collect();
    // 反復 DFS（スタック上に訪問中パスを保持し、Gray への再訪＝閉路）。
    for start in g.nodes.keys() {
        if color.get(start.as_str()) != Some(&C::White) {
            continue;
        }
        let mut path: Vec<&str> = Vec::new();
        let mut stack: Vec<(&str, usize)> = vec![(start.as_str(), 0)];
        color.insert(start.as_str(), C::Gray);
        path.push(start.as_str());
        while let Some(&(node, idx)) = stack.last() {
            let next = adj.get(node).and_then(|v| v.get(idx)).copied();
            match next {
                Some(nb) => {
                    stack.last_mut().unwrap().1 += 1;
                    match color.get(nb).copied().unwrap_or(C::Black) {
                        C::Gray => {
                            // 閉路検出: path の nb 以降を切り出す。
                            if let Some(pos) = path.iter().position(|&x| x == nb) {
                                let mut cyc: Vec<String> = path[pos..].iter().map(|s| s.to_string()).collect();
                                cyc.push(nb.to_string());
                                return Some(cyc);
                            }
                        }
                        C::White => {
                            color.insert(nb, C::Gray);
                            path.push(nb);
                            stack.push((nb, 0));
                        }
                        C::Black => {}
                    }
                }
                None => {
                    color.insert(node, C::Black);
                    path.pop();
                    stack.pop();
                }
            }
        }
    }
    None
}

/// findings を MD レポートへ整形（級ごとに節・件数付き）。
fn render(terminal_label: &str, node_count: usize, filter_count: usize, findings: &[Finding]) -> Vec<u8> {
    let count = |lv: Level| findings.iter().filter(|f| f.level == lv).count();
    let (ne, nw, ni) = (count(Level::Error), count(Level::Warning), count(Level::Info));
    let mut md = format!("# 「{terminal_label}」の運用前評価\n\n");
    md.push_str(&format!(
        "到達カード数: {node_count}（うちフィルタ {filter_count}）／エラー {ne}・警告 {nw}・情報 {ni}\n\n",
    ));
    if findings.is_empty() {
        md.push_str("✓ 問題は見つかりませんでした。\n\n");
        md.push_str(SCOPE_NOTE);
        return md.into_bytes();
    }
    let section = |md: &mut String, title: &str, lv: Level| {
        let items: Vec<&Finding> = findings.iter().filter(|f| f.level == lv).collect();
        if items.is_empty() {
            return;
        }
        md.push_str(&format!("## {title}（{}）\n\n", items.len()));
        for it in items {
            md.push_str(&format!("- **{}**: {}\n", it.category, it.message));
        }
        md.push('\n');
    };
    section(&mut md, "エラー", Level::Error);
    section(&mut md, "警告", Level::Warning);
    section(&mut md, "情報", Level::Info);
    md.push_str(SCOPE_NOTE);
    md.into_bytes()
}

/// v1.5.5 検証修正#4: 検査範囲の明示（未検査項目を検査済みと誤解させない）。
const SCOPE_NOTE: &str = "---\n※ 依存検査の範囲: 外部プログラムの所在／Ollama への到達／t2i の Python 環境。\n\
モデルファイルの中身の整合・openai 準拠エンドポイントの到達は対象外です。\n";

/// 固定名レポートカードを生成/上書き（upsert_chat_log_card の流儀）。
/// 「<終端カード名>の評価.md」を照合→実身差し替え→終端カードから flow 矢印。
fn write_report(p: &Project, terminal_id: &str, terminal_label: &str, bytes: &[u8]) -> Result<String, String> {
    let sanitize = |s: &str| -> String {
        s.chars()
            .map(|c| match c {
                '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
                c if c.is_control() => '_',
                c => c,
            })
            .collect::<String>()
    };
    let fname = format!(
        "{}の評価.md",
        sanitize(terminal_label).chars().take(40).collect::<String>()
    );
    let dest = p.root.join(&fname);
    std::fs::write(&dest, bytes).map_err(|e| format!("レポート書き出し失敗: {e}"))?;
    let dest_str = dest.to_string_lossy().into_owned();

    let hash = cas::store_bytes(&p.cas_root, bytes).map_err(|e| e.to_string())?;
    db::insert_real_body(&p.conn, &hash, "text/markdown", bytes.len() as u64, Some("md"))
        .map_err(|e| e.to_string())?;

    let existing: Option<String> = p
        .conn
        .query_row(
            "SELECT id FROM virtual_bodies
             WHERE deleted_at IS NULL AND (committed_path = ?1 OR origin_path = ?1)",
            rusqlite::params![dest_str],
            |r| r.get(0),
        )
        .optional()
        .unwrap_or(None);

    let report_id = if let Some(vid) = existing {
        p.conn
            .execute(
                "UPDATE virtual_bodies SET real_id = ?1, committed_mtime = ?2 WHERE id = ?3",
                rusqlite::params![hash, file_mtime_ms(&dest), vid],
            )
            .map_err(|e| e.to_string())?;
        vid
    } else {
        let (fx, fy): (f64, f64) = p
            .conn
            .query_row(
                "SELECT pos_x, pos_y FROM virtual_bodies WHERE id = ?1",
                rusqlite::params![terminal_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap_or((0.0, 0.0));
        let vid = uuid();
        p.conn
            .execute(
                "INSERT INTO virtual_bodies
                    (id, real_id, label, pos_x, pos_y, created_at,
                     committed_path, committed_mtime, kind)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'file')",
                rusqlite::params![
                    vid,
                    hash,
                    fname,
                    fx + 260.0,
                    fy + 120.0,
                    db::now_ms(),
                    dest_str,
                    file_mtime_ms(&dest),
                ],
            )
            .map_err(|e| e.to_string())?;
        vid
    };

    // 終端 → レポート の flow 矢印（無ければ張る＝別フローの入力に使える）。
    let has_rel: bool = p
        .conn
        .query_row(
            "SELECT 1 FROM relations
             WHERE from_id = ?1 AND to_id = ?2 AND kind = 'flow' AND deleted_at IS NULL",
            rusqlite::params![terminal_id, report_id],
            |_| Ok(true),
        )
        .optional()
        .unwrap_or(None)
        .unwrap_or(false);
    if !has_rel {
        let _ = p.conn.execute(
            "INSERT INTO relations (from_id, to_id, kind, created_at) VALUES (?1, ?2, 'flow', ?3)",
            rusqlite::params![terminal_id, report_id, db::now_ms()],
        );
    }
    Ok(report_id)
}

/// 運用前評価コマンド（終端カード右クリック「運用前評価」）。
/// 手順: ①ロック内で到達集合を所有データ化 → ②ロック外で検査（依存チェックは network）→
/// ③ロック内でレポートカードを固定名上書き。ネットワーク接触中はロックを持たない。
#[tauri::command(async)]
pub(crate) fn evaluate_flow(id: String, state: State<AppState>) -> Result<EvalSummary, String> {
    // ① 収集（短ロック）。root は③の書き込みガードと自己発火検査に使う。
    let (graph, terminal_label, app_data, filters_dir, root) = {
        let guard = state.current.lock().unwrap();
        let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
        let g = collect(p, &id)?;
        // v1.5.5 検証修正#2: 発行系の op カードは評価の起点にしない（req 先を辿らないため
        // 空レポートになる上、write_report が op カードに flow 矢印を張り不変条件を壊す）。
        // v1.5.6 WS-M M1（§6.2）: **OUT バーチャルカードは起点にできる**（終端カードまたは
        // OUT が正・データレーンの住人なので flow を上流に辿れる）。IN は口の向きが逆なので不可。
        let terminal_op = g.nodes.get(&id).filter(|n| n.kind == "op").and_then(|n| {
            n.filter_ref
                .as_deref()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                .and_then(|v| v.get("op").and_then(|o| o.as_str().map(String::from)))
        });
        if let Some(k) = terminal_op {
            if k != "out" {
                return Err(
                    "このカードは評価の起点にできません。フィルタカードまたは OUT カードで「運用前評価」を実行してください"
                        .into(),
                );
            }
        }
        let label = g
            .nodes
            .get(&id)
            .map(|n| n.label.clone())
            .unwrap_or_else(|| "フロー".into());
        (g, label, state.app_data.clone(), filters_dir(&p.root), p.root.clone())
    };

    // ② 検査（ロック外・依存チェックの network を含む）
    let node_count = graph.nodes.len();
    let filter_count = graph.nodes.values().filter(|n| n.is_filter()).count();
    let findings = inspect(&graph, &root, &app_data, &filters_dir);
    let (mut ne, mut nw, mut ni) = (0, 0, 0);
    for f in &findings {
        match f.level {
            Level::Error => ne += 1,
            Level::Warning => nw += 1,
            Level::Info => ni += 1,
        }
    }
    let bytes = render(&terminal_label, node_count, filter_count, &findings);

    // ③ レポートカード生成（短ロック）
    let report_id = {
        let guard = state.current.lock().unwrap();
        let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
        // v1.5.5 検証修正#1: ②のネットワーク接触中にプロジェクトが切り替わったら書き込まない
        // （WS-C 起動元束縛の原則・ops.rs スケジューラ tick と同じガード）。切替先のプロジェクト
        // にレポートファイルと宙ぶらりんの flow 矢印が入るのを防ぐ。
        if p.root != root {
            return Err(
                "評価中にプロジェクトが切り替わったため、レポートは書き込みませんでした。元のプロジェクトで再実行してください"
                    .into(),
            );
        }
        write_report(p, &id, &terminal_label, &bytes)?
    };

    Ok(EvalSummary {
        errors: ne,
        warnings: nw,
        infos: ni,
        report_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, label: &str, kind: &str, filter_ref: Option<&str>, mime: &str) -> Node {
        Node {
            id: id.into(),
            label: label.into(),
            kind: kind.into(),
            real_id: format!("{id}:real"),
            filter_ref: filter_ref.map(|s| s.into()),
            mime: mime.into(),
        }
    }

    fn graph_of(nodes: Vec<Node>, edges: Vec<(&str, &str)>) -> Graph {
        let mut map = HashMap::new();
        for n in nodes {
            map.insert(n.id.clone(), n);
        }
        Graph {
            nodes: map,
            edges: edges
                .into_iter()
                .map(|(fr, to)| Edge {
                    from: fr.into(),
                    to: to.into(),
                    attrs: flow::EdgeAttrs::default(),
                })
                .collect(),
            ops_into: vec![],
            dangling_ops: vec![],
            dangling_ios: vec![],
            bad_inputs: vec![],
        }
    }

    #[test]
    fn detects_cycle() {
        // a → b → c → a の閉路。
        let g = graph_of(
            vec![
                node("a", "A", "filter", Some(r#"{"builtin":"text_grep"}"#), ""),
                node("b", "B", "filter", Some(r#"{"builtin":"text_grep"}"#), ""),
                node("c", "C", "filter", Some(r#"{"builtin":"text_grep"}"#), ""),
            ],
            vec![("a", "b"), ("b", "c"), ("c", "a")],
        );
        assert!(find_cycle(&g).is_some());
    }

    #[test]
    fn no_cycle_on_dag() {
        let g = graph_of(
            vec![
                node("a", "A", "file", None, "text/plain"),
                node("b", "B", "filter", Some(r#"{"builtin":"text_grep"}"#), ""),
            ],
            vec![("a", "b")],
        );
        assert!(find_cycle(&g).is_none());
    }

    #[test]
    fn type_mismatch_flagged() {
        // 画像カード → text/* 受入の grep は不一致。
        let g = graph_of(
            vec![
                node("img", "写真", "file", None, "image/png"),
                node("grep", "grep", "filter", Some(r#"{"builtin":"text_grep","accept_mime":"text/*"}"#), ""),
            ],
            vec![("img", "grep")],
        );
        let f = inspect(&g, Path::new(""), Path::new("."), Path::new("."));
        assert!(f.iter().any(|x| x.category == "型不一致"));
    }

    #[test]
    fn type_match_ok() {
        let g = graph_of(
            vec![
                node("txt", "メモ", "file", None, "text/plain"),
                node("grep", "grep", "filter", Some(r#"{"builtin":"text_grep","accept_mime":"text/*"}"#), ""),
            ],
            vec![("txt", "grep")],
        );
        let f = inspect(&g, Path::new(""), Path::new("."), Path::new("."));
        assert!(!f.iter().any(|x| x.category == "型不一致"));
    }

    #[test]
    fn selector_all_conditional_flagged() {
        // 全 IN に when が付いたセレクタ = 全滅可能。
        let mut g = graph_of(
            vec![
                node("s", "セレクタ", "filter", Some(r#"{"builtin":"select"}"#), ""),
                node("x", "X", "file", None, "text/plain"),
                node("y", "Y", "file", None, "text/plain"),
            ],
            vec![("x", "s"), ("y", "s")],
        );
        for e in &mut g.edges {
            e.attrs.when = Some("success".into());
        }
        let f = inspect(&g, Path::new(""), Path::new("."), Path::new("."));
        assert!(f.iter().any(|x| x.category == "セレクタ全滅可能"));
    }

    #[test]
    fn selector_with_always_branch_ok() {
        let mut g = graph_of(
            vec![
                node("s", "セレクタ", "filter", Some(r#"{"builtin":"select"}"#), ""),
                node("x", "X", "file", None, "text/plain"),
                node("y", "Y", "file", None, "text/plain"),
            ],
            vec![("x", "s"), ("y", "s")],
        );
        g.edges[0].attrs.when = Some("success".into());
        // edges[1] は always（when=None）
        let f = inspect(&g, Path::new(""), Path::new("."), Path::new("."));
        assert!(!f.iter().any(|x| x.category == "セレクタ全滅可能"));
    }

    #[test]
    fn no_input_flagged() {
        let g = graph_of(
            vec![node("grep", "grep", "filter", Some(r#"{"builtin":"text_grep"}"#), "")],
            vec![],
        );
        let f = inspect(&g, Path::new(""), Path::new("."), Path::new("."));
        assert!(f.iter().any(|x| x.category == "入力なし"));
    }

    #[test]
    fn placeholder_overflow_flagged() {
        // {in2} を参照するが入力1本 → 超過。
        let g = graph_of(
            vec![
                node("a", "A", "file", None, "text/plain"),
                node("cc", "gcc", "filter", Some(r#"{"name":"gcc.exe","cmd":"{in1} {in2} -o {out}"}"#), ""),
            ],
            vec![("a", "cc")],
        );
        let f = inspect(&g, Path::new(""), Path::new("."), Path::new("."));
        assert!(f.iter().any(|x| x.category == "入力不足"));
    }

    #[test]
    fn max_in_placeholder_parse() {
        assert_eq!(max_in_placeholder("{in1} {in2} -o {out}"), Some(2));
        assert_eq!(max_in_placeholder("{in} {out}"), None);
        assert_eq!(max_in_placeholder("gcc -o out"), None);
    }

    #[test]
    fn path_covers_basic() {
        assert!(path_covers("C:/out", "C:/out/sub/x.png"));
        assert!(path_covers("C:\\out", "C:/out"));
        assert!(!path_covers("C:/out", "C:/other"));
    }

    #[test]
    fn watch_on_project_root_flagged() {
        // v1.5.5 検証修正#3: out_dir 未指定の結果はプロジェクト直下に生まれる。
        // プロジェクトフォルダ（またはその親）を監視するウォッチャは再発火ループ。
        let mut g = graph_of(
            vec![node("grep", "grep", "filter", Some(r#"{"builtin":"text_grep"}"#), "")],
            vec![],
        );
        g.ops_into.push(OpCard {
            label: "監視".into(),
            spec: ops::parse_op_spec(r#"{"op":"watch","path":"C:\\proj"}"#).unwrap(),
        });
        let f = inspect(&g, Path::new("C:\\proj"), Path::new("."), Path::new("."));
        assert!(f.iter().any(|x| x.category == "自己発火リスク"));
        // サブフォルダ監視（直下に結果は落ちない）は対象外。
        let mut g2 = graph_of(vec![], vec![]);
        g2.ops_into.push(OpCard {
            label: "監視".into(),
            spec: ops::parse_op_spec(r#"{"op":"watch","path":"C:\\proj\\incoming"}"#).unwrap(),
        });
        let f2 = inspect(&g2, Path::new("C:\\proj"), Path::new("."), Path::new("."));
        assert!(!f2.iter().any(|x| x.category == "自己発火リスク"));
    }

    #[test]
    fn paused_op_reported_as_info() {
        let mut g = graph_of(
            vec![node("grep", "grep", "filter", Some(r#"{"builtin":"text_grep"}"#), "")],
            vec![],
        );
        g.ops_into.push(OpCard {
            label: "タイマ".into(),
            spec: ops::parse_op_spec(r#"{"op":"timer","every":60,"paused":true}"#).unwrap(),
        });
        let f = inspect(&g, Path::new(""), Path::new("."), Path::new("."));
        assert!(f.iter().any(|x| x.category == "停止中"));
    }

    #[test]
    fn bad_input_source_flagged() {
        // v1.5.5 検証修正#5: 付箋など仮身でないものからの flow 矢印は警告。
        let mut g = graph_of(
            vec![node("grep", "grep", "filter", Some(r#"{"builtin":"text_grep","accept_mime":"text/*"}"#), "")],
            vec![],
        );
        g.bad_inputs.push("grep".into());
        let f = inspect(&g, Path::new(""), Path::new("."), Path::new("."));
        assert!(f.iter().any(|x| x.category == "不正な入力元"));
    }
}
