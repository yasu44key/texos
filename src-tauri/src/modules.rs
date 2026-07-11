#![allow(unused_imports)]
use crate::*;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::State;

// ---------------------------------------------------------------------------
// v1.5.8 WS-M M2: モジュールライブラリ（共有とモジュール設計.md §10）
//
// 1モジュール = 1フォルダ: <app_data>/modules/<name>/{module.json, blobs/}
//   - module.json = カード列・矢印列（attrs込み）・図形・相対座標・依存（requires）
//   - blobs/      = ファイル種カードの実身コピー（CAS 形式 <first2>/<rest>・§10.2 常に同梱）
// ライブラリは個人の道具箱（filter_library と同じ流儀・配布経路ではない・§10.1）。
// 配布はプロジェクト zip → 受け手が「ライブラリに登録」。
// ---------------------------------------------------------------------------

/// §10.2 巨大データ警告のしきい値（実装時決定の例=100MB。実測で調整）。
const MODULE_SIZE_WARN: u64 = 100 * 1024 * 1024;

pub(crate) fn modules_dir(app_data: &Path) -> PathBuf {
    app_data.join("modules")
}

/// フォルダ名にできない文字を潰す（preflight のレポート名と同じ流儀）。
fn sanitize_name(s: &str) -> String {
    let out: String = s
        .trim()
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    out.chars().take(60).collect()
}

/// モジュール内の1カード。id は元プロジェクトのもの（window_of・矢印の端点解決用）で、
/// 取り込み時に全再割当される（send_selection_to_project の id_map 方式）。
#[derive(Serialize, Deserialize)]
struct ModCard {
    id: String,
    label: String,
    /// 図形左上からの相対座標（§10.1）。
    x: f64,
    y: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    filter_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    url_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    folder_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    memo: Option<String>,
    #[serde(default)]
    z: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ext: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    window_of: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    style_ref: Option<String>,
    /// v1.5.9: プレビュー展開状態（チャット欄・テキスト/画像プレビュー）を保存・復元（要件3）。
    #[serde(default)]
    expanded: bool,
    /// 参照 real_body（センチネル "filter:sentinel" 等も含めて再現する）。
    real_id: String,
    real_mime: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    real_ext: Option<String>,
    /// ファイル種カードの実身コピー（blobs/ 内の SHA-256）。§10.2 常に同梱。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    blob: Option<String>,
    #[serde(default)]
    blob_size: u64,
}

/// モジュール内の矢印（両端がメンバーのものだけ・attrs 込み）。
#[derive(Serialize, Deserialize)]
struct ModEdge {
    from: String,
    to: String,
    kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    attrs: Option<String>,
}

/// 境界の図形（コンテナ）。位置は展開先で決まるためサイズ・見た目だけ持つ。
#[derive(Serialize, Deserialize)]
struct ModShape {
    kind: String,
    width: f64,
    height: f64,
    rotation: f64,
    fill: String,
    opacity: f64,
    label: String,
}

/// AI 依存（§10.3 spec から自動収集）。
#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct ModAiDep {
    provider: String,
    model: String,
    endpoint: String,
}

/// 依存リスト（手動スキーマ欄は設けない＝spec 走査のみ・§10.3）。
#[derive(Serialize, Deserialize, Default, Clone)]
pub(crate) struct ModRequires {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    programs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    ai: Vec<ModAiDep>,
    /// 外部環境を要する builtin（"t2i"・"whisper"）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    builtins: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct ModuleJson {
    name: String,
    created_at: i64,
    shape: ModShape,
    cards: Vec<ModCard>,
    relations: Vec<ModEdge>,
    #[serde(default)]
    requires: ModRequires,
    #[serde(default)]
    total_blob_bytes: u64,
}

/// ライブラリ一覧の1行（メニュー「ユーザーのモジュール」の素材）。
#[derive(Serialize)]
pub(crate) struct ModuleEntry {
    pub name: String,
    pub created_at: i64,
    pub cards: usize,
    pub total_blob_bytes: u64,
}

#[tauri::command]
pub(crate) fn list_module_library(state: State<AppState>) -> Result<Vec<ModuleEntry>, String> {
    let dir = modules_dir(&state.app_data);
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let mj = e.path().join("module.json");
            if let Ok(s) = std::fs::read_to_string(&mj) {
                if let Ok(m) = serde_json::from_str::<ModuleJson>(&s) {
                    out.push(ModuleEntry {
                        name: m.name,
                        created_at: m.created_at,
                        cards: m.cards.len(),
                        total_blob_bytes: m.total_blob_bytes,
                    });
                }
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// §10.3: メンバーの spec から依存を自動収集（preflight::inspect_dependencies と同じ走査）。
fn collect_requires(specs: &[String]) -> ModRequires {
    let mut req = ModRequires::default();
    let mut seen_prog = std::collections::HashSet::new();
    let mut seen_ai = std::collections::HashSet::new();
    let mut seen_builtin = std::collections::HashSet::new();
    for s in specs {
        let Ok(spec) = flow::parse_spec(s) else { continue };
        match spec.node_kind() {
            flow::NodeKind::External | flow::NodeKind::Generator => {
                if let Some(name) = spec.name.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                    if seen_prog.insert(name.to_string()) {
                        req.programs.push(name.to_string());
                    }
                }
            }
            flow::NodeKind::Ai => {
                let provider = spec.provider.clone().unwrap_or_else(|| "ollama".into());
                let model = spec.model.clone().unwrap_or_default();
                let endpoint = flow::effective_ai_endpoint(&spec);
                if seen_ai.insert(format!("{provider}\u{1}{model}\u{1}{endpoint}")) {
                    req.ai.push(ModAiDep { provider, model, endpoint });
                }
            }
            flow::NodeKind::T2i => {
                if seen_builtin.insert("t2i") {
                    req.builtins.push("t2i".into());
                }
            }
            flow::NodeKind::Builtin => {
                if spec.builtin.as_deref() == Some("whisper") && seen_builtin.insert("whisper") {
                    req.builtins.push("whisper".into());
                }
            }
            _ => {}
        }
    }
    req
}

/// 展開先での依存所在チェック（§10.3 初回アテンションの素材）。
/// ollama への到達はネットワーク接触（タイムアウト3秒）なので、呼び出し側は
/// プロジェクトロックの外で呼ぶこと（preflight と同じ注意）。
fn check_requires(req: &ModRequires, app_data: &Path, filters_dir: &Path) -> Vec<String> {
    let mut missing = Vec::new();
    for name in &req.programs {
        if flow::resolve_program(name, None, filters_dir).is_none() {
            missing.push(format!(
                "プログラム「{name}」が見つかりません（.texos\\filters への同梱か PATH を確認）"
            ));
        }
    }
    let mut checked = std::collections::HashSet::new();
    for dep in &req.ai {
        if dep.provider == "ollama" && checked.insert(dep.endpoint.clone()) {
            if !ollama_running(&dep.endpoint) {
                missing.push(format!(
                    "AI エンドポイント {} に接続できません（Ollama 未起動・モデル: {}）",
                    dep.endpoint, dep.model
                ));
            }
        }
    }
    for b in &req.builtins {
        match b.as_str() {
            "t2i" => {
                if !detect_python(app_data).deps_ok {
                    missing.push("画像生成(t2i)の Python 環境が未構築です（カードの「環境を構築する」から導入）".into());
                }
            }
            "whisper" => {
                if !whisper_cli_path(app_data).is_file() {
                    missing.push("Whisper（文字起こし）が未導入です（カードのセットアップから導入）".into());
                }
            }
            _ => {}
        }
    }
    missing
}

#[derive(Serialize)]
pub(crate) struct SaveModuleResult {
    /// false = 合計サイズがしきい値超過で未保存（allow_large=true で再実行）。
    pub saved: bool,
    pub total_bytes: u64,
    pub cards: usize,
}

/// 図形右クリック「ライブラリに登録」（§10.1）。範囲 = ロック済みメンバー
/// （§8.1 スナップショット・shapes.members）。実身は常にコピー同梱（§10.2）。
/// 同名は上書き（filter_library と同じ流儀）。
#[tauri::command]
pub(crate) fn save_module_library(
    name: String,
    shape_id: String,
    allow_large: bool,
    state: State<AppState>,
) -> Result<SaveModuleResult, String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("モジュール名を入力してください".into());
    }
    let folder = sanitize_name(&name);
    if folder.is_empty() {
        return Err("モジュール名に使える文字がありません".into());
    }
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;

    // 図形（境界）とロック済みメンバー（§8.1: ライブラリ化はロックを前提とするスナップショット）。
    let shape_row: Option<(String, f64, f64, f64, f64, f64, String, f64, String, i64, Option<String>)> = p
        .conn
        .query_row(
            "SELECT kind,pos_x,pos_y,width,height,rotation,fill,opacity,label,locked,members
             FROM shapes WHERE id = ?1 AND deleted_at IS NULL",
            rusqlite::params![shape_id],
            |r| {
                Ok((
                    r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?,
                    r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?,
                ))
            },
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let Some((s_kind, s_x, s_y, s_w, s_h, s_rot, s_fill, s_opacity, s_label, s_locked, s_members)) =
        shape_row
    else {
        return Err("図形が見つかりません".into());
    };
    if s_locked == 0 {
        return Err("先に図形右クリック「メンバーをロック」でメンバーを固定してください（ライブラリ化はロックのスナップショットを保存します）".into());
    }
    let members: Vec<String> = s_members
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    if members.is_empty() {
        return Err("ロックされたメンバーがいません（カードを乗せてからロックし直してください）".into());
    }

    // §9 の口（IN/OUT）を同伴: ロック後の「モジュール化」で生まれた IN/OUT カードは
    // members スナップショットに入っていないことがある（M1 は members を書き換えなかった）。
    // メンバーと flow で直結している IN/OUT は境界の口としてモジュールに含める
    //（1ホップで十分＝IN/OUT はメンバーに直結する。外側半分の矢印は両端条件で自然に落ちる）。
    let mut export_ids: Vec<String> = members.clone();
    {
        let seed: std::collections::HashSet<&String> = members.iter().collect();
        let mut stmt = p
            .conn
            .prepare(
                "SELECT rel.from_id, rel.to_id, vf.kind, vf.filter_ref, vt.kind, vt.filter_ref
                 FROM relations rel
                 JOIN virtual_bodies vf ON vf.id = rel.from_id AND vf.deleted_at IS NULL
                 JOIN virtual_bodies vt ON vt.id = rel.to_id AND vt.deleted_at IS NULL
                 WHERE rel.kind = 'flow' AND rel.deleted_at IS NULL",
            )
            .map_err(|e| e.to_string())?;
        #[allow(clippy::type_complexity)]
        let rows: Vec<(String, String, Option<String>, Option<String>, Option<String>, Option<String>)> =
            stmt.query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
            })
            .map_err(|e| e.to_string())?
            .flatten()
            .collect();
        for (f, t, fk, fs, tk, ts) in rows {
            let f_in = seed.contains(&f);
            let t_in = seed.contains(&t);
            if f_in == t_in {
                continue; // またいでいない（内部 or 完全外部）
            }
            let (other, kind, spec) = if f_in { (t, tk, ts) } else { (f, fk, fs) };
            if kind.as_deref() == Some("op")
                && spec.as_deref().map(ops::is_io_op_spec).unwrap_or(false)
                && !export_ids.contains(&other)
            {
                export_ids.push(other);
            }
        }
    }

    // メンバーカードの行を読む（send_selection_to_project の CardRow と同じ持ち物）。
    struct MemberRow {
        real_id: String,
        label: String,
        x: f64,
        y: f64,
        kind: Option<String>,
        filter_ref: Option<String>,
        url_ref: Option<String>,
        folder_ref: Option<String>,
        project_ref: Option<String>,
        memo: Option<String>,
        z: i64,
        ext: Option<String>,
        mime_type: Option<String>,
        window_of: Option<String>,
        note_text: Option<String>,
        style_ref: Option<String>,
        expanded: bool,
        real_mime: String,
        real_ext: Option<String>,
    }
    let mut cards: Vec<ModCard> = Vec::new();
    let mut blob_jobs: Vec<(usize, PathBuf)> = Vec::new(); // (cards index, live path)
    let mut total: u64 = 0;
    for mid in &export_ids {
        let row: Option<MemberRow> = p
            .conn
            .query_row(
                "SELECT v.real_id, v.label, v.pos_x, v.pos_y, v.kind, v.filter_ref,
                        v.url_ref, v.folder_ref, v.project_ref, v.memo, v.z_order,
                        v.ext, v.mime_type, v.window_of, v.note_text, v.style_ref,
                        v.expanded,
                        COALESCE(v.mime_type, r.mime_type), r.ext
                 FROM virtual_bodies v JOIN real_bodies r ON r.id = v.real_id
                 WHERE v.id = ?1 AND v.deleted_at IS NULL",
                rusqlite::params![mid],
                |r| {
                    Ok(MemberRow {
                        real_id: r.get(0)?,
                        label: r.get(1)?,
                        x: r.get(2)?,
                        y: r.get(3)?,
                        kind: r.get(4)?,
                        filter_ref: r.get(5)?,
                        url_ref: r.get(6)?,
                        folder_ref: r.get(7)?,
                        project_ref: r.get(8)?,
                        memo: r.get(9)?,
                        z: r.get(10)?,
                        ext: r.get(11)?,
                        mime_type: r.get(12)?,
                        window_of: r.get(13)?,
                        note_text: r.get(14)?,
                        style_ref: r.get(15)?,
                        expanded: r.get::<_, i64>(16)? != 0,
                        real_mime: r.get(17)?,
                        real_ext: r.get(18)?,
                    })
                },
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let Some(MemberRow {
            real_id, label, x, y, kind, filter_ref, url_ref, folder_ref, project_ref,
            memo, z, ext, mime_type, window_of, note_text, style_ref, expanded, real_mime, real_ext,
        }) = row
        else {
            continue; // 削除済みメンバーはスキップ（回収漏れの残骸）
        };
        // プロジェクト参照カードは持ち出さない（send_selection と同じ・環境依存が強すぎる）。
        if project_ref.is_some() {
            continue;
        }
        // タイマ/ウォッチャは停止状態で持ち出す（新環境の誤発火防止・v1.5.5 の流儀）。
        // fired_count もリセット（部品としてまっさらに）。IN/OUT/トリガは対象外。
        let filter_ref = filter_ref.map(|spec| {
            if kind.as_deref() == Some("op") {
                if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&spec) {
                    let op = v.get("op").and_then(|o| o.as_str()).unwrap_or("");
                    if matches!(op, "timer" | "watch") {
                        if let Some(o) = v.as_object_mut() {
                            o.insert("paused".into(), serde_json::Value::Bool(true));
                            o.remove("fired_count");
                        }
                        return v.to_string();
                    }
                }
            }
            spec
        });
        // 付箋の file_ref は元プロジェクトのコミット先を指すので外す（send_selection と同じ）。
        let style_ref = if kind.as_deref() == Some("note") {
            style_ref.as_deref().and_then(|s| {
                serde_json::from_str::<serde_json::Value>(s).ok().map(|mut v| {
                    if let Some(o) = v.as_object_mut() {
                        o.remove("file_ref");
                    }
                    v.to_string()
                })
            })
        } else {
            style_ref
        };
        let idx = cards.len();
        // ファイル種カードは実身コピーを blobs へ同梱（§10.2 常にコピー）。
        let is_file = kind.as_deref().map(|k| k == "file").unwrap_or(true)
            && url_ref.is_none()
            && folder_ref.is_none();
        if is_file {
            let live = resolve_view_path(p, mid, &real_id);
            if live.is_file() {
                total += std::fs::metadata(&live).map(|m| m.len()).unwrap_or(0);
                blob_jobs.push((idx, live));
            }
        }
        cards.push(ModCard {
            id: mid.clone(),
            label,
            x: x - s_x,
            y: y - s_y,
            kind,
            filter_ref,
            url_ref,
            folder_ref,
            memo,
            z,
            ext,
            mime_type,
            window_of,
            note_text,
            style_ref,
            expanded,
            real_id,
            real_mime,
            real_ext,
            blob: None,
            blob_size: 0,
        });
    }
    if cards.is_empty() {
        return Err("持ち出せるメンバーカードがありません".into());
    }

    // §10.2 巨大データ警告: しきい値超過は一旦返す（フロントが確認 → allow_large で再実行）。
    if total > MODULE_SIZE_WARN && !allow_large {
        return Ok(SaveModuleResult {
            saved: false,
            total_bytes: total,
            cards: cards.len(),
        });
    }

    // メンバー内で閉じた矢印だけ運ぶ（attrs 込み・send_selection と同じ選別）。
    let member_set: std::collections::HashSet<&String> = export_ids.iter().collect();
    let relations: Vec<ModEdge> = {
        let mut stmt = p
            .conn
            .prepare("SELECT from_id,to_id,kind,attrs FROM relations WHERE deleted_at IS NULL")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        rows.flatten()
            .filter(|(f, t, _, _)| member_set.contains(f) && member_set.contains(t))
            .map(|(from, to, kind, attrs)| ModEdge { from, to, kind, attrs })
            .collect()
    };

    // 依存の自動収集（§10.3・フィルタカードの spec 走査）。
    let specs: Vec<String> = cards
        .iter()
        .filter(|c| c.kind.as_deref() == Some("filter"))
        .filter_map(|c| c.filter_ref.clone())
        .collect();
    let requires = collect_requires(&specs);

    // 書き出し（同名は上書き＝フォルダごと作り直し）。
    let dir = modules_dir(&state.app_data).join(&folder);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| format!("既存モジュールの削除に失敗: {e}"))?;
    }
    let blobs = dir.join("blobs");
    std::fs::create_dir_all(&blobs).map_err(|e| e.to_string())?;
    for (idx, live) in blob_jobs {
        let hash = cas::store_file(&blobs, &live).map_err(|e| format!("実身コピーに失敗: {e}"))?;
        cards[idx].blob_size = std::fs::metadata(&live).map(|m| m.len()).unwrap_or(0);
        cards[idx].blob = Some(hash);
    }
    let n_cards = cards.len();
    let m = ModuleJson {
        name: name.clone(),
        created_at: db::now_ms(),
        shape: ModShape {
            kind: s_kind,
            width: s_w,
            height: s_h,
            rotation: s_rot,
            fill: s_fill,
            opacity: s_opacity,
            label: if s_label.is_empty() { name } else { s_label },
        },
        cards,
        relations,
        requires,
        total_blob_bytes: total,
    };
    let json = serde_json::to_string_pretty(&m).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("module.json"), json).map_err(|e| e.to_string())?;
    Ok(SaveModuleResult {
        saved: true,
        total_bytes: total,
        cards: n_cards,
    })
}

#[tauri::command]
pub(crate) fn delete_module_library(name: String, state: State<AppState>) -> Result<(), String> {
    let dir = modules_dir(&state.app_data).join(sanitize_name(&name));
    if !dir.join("module.json").is_file() {
        return Err(format!("「{name}」はライブラリにありません"));
    }
    std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())
}

/// v1.5.12 メニュー再編: モジュールの登録名を変更（「カテゴリ/名前」への移動もこれ）。
/// module.json の name を書き換え、フォルダ名（sanitize_name）も追従して改名する。
/// 一覧（list_module_library）は module.json の name を読むので、フォルダ改名に
/// 失敗しても名前の変更自体は成立している（次回の改名で追従できる）。
#[tauri::command]
pub(crate) fn rename_module_library(
    old: String,
    new: String,
    state: State<AppState>,
) -> Result<(), String> {
    let new = new.trim().to_string();
    if new.is_empty() {
        return Err("新しい名前を入力してください".into());
    }
    let dir = modules_dir(&state.app_data);
    let src = dir.join(sanitize_name(&old));
    let mj = src.join("module.json");
    if !mj.is_file() {
        return Err(format!("「{old}」はライブラリにありません"));
    }
    if new == old {
        return Ok(());
    }
    let dst = dir.join(sanitize_name(&new));
    if dst != src && dst.join("module.json").is_file() {
        return Err(format!("「{new}」は既にあります"));
    }
    let mut m: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&mj).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    m["name"] = serde_json::Value::String(new.clone());
    let json = serde_json::to_string_pretty(&m).map_err(|e| e.to_string())?;
    std::fs::write(&mj, json).map_err(|e| e.to_string())?;
    if dst != src {
        std::fs::rename(&src, &dst).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[derive(Serialize)]
pub(crate) struct PlaceModuleResult {
    pub new_card_ids: Vec<String>,
    pub shape_id: String,
    /// 依存アテンションの付箋（未導入の依存があるときだけ・§10.3）。
    pub note_id: Option<String>,
    pub missing: Vec<String>,
}

/// キャンバスメニュー「ユーザーのモジュール」→ 展開（§8.5 透過＝インライン展開）。
/// ID は全再割当（send_selection_to_project の id_map 方式）・blobs はプロジェクト CAS へ
/// materialize（§10.2）・図形はロック済み（members=新ID）で置く。
#[tauri::command]
pub(crate) fn place_module_from_library(
    name: String,
    x: f64,
    y: f64,
    state: State<AppState>,
) -> Result<PlaceModuleResult, String> {
    let dir = modules_dir(&state.app_data).join(sanitize_name(&name));
    let json = std::fs::read_to_string(dir.join("module.json"))
        .map_err(|_| format!("モジュール「{name}」が見つかりません"))?;
    let m: ModuleJson =
        serde_json::from_str(&json).map_err(|e| format!("module.json が壊れています: {e}"))?;

    // 依存チェックのうちネットワーク接触（ollama 到達）はプロジェクトロックの外で。
    // filters_dir はプロジェクト依存なのでロック取得後に programs を見る（下）。
    let app_data = state.app_data.clone();
    let ai_missing: Vec<String> = {
        let ai_only = ModRequires {
            programs: vec![],
            ai: m.requires.ai.clone(),
            builtins: m.requires.builtins.clone(),
        };
        check_requires(&ai_only, &app_data, Path::new(""))
    };

    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let mut missing = ai_missing;
    {
        let prog_only = ModRequires {
            programs: m.requires.programs.clone(),
            ai: vec![],
            builtins: vec![],
        };
        let mut prog_missing = check_requires(&prog_only, &app_data, &filters_dir(&p.root));
        missing.append(&mut prog_missing);
    }

    // ID 全再割当（旧→新 UUID）。
    let mut id_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for c in &m.cards {
        id_map.entry(c.id.clone()).or_insert_with(uuid);
    }

    let blobs_root = dir.join("blobs");
    // 同梱データの事前検証: DB へ挿入し始めてから欠損に気づくと中途半端な展開が残るため、
    // 先に blobs の実在を確かめる（モジュールフォルダの手動破損への防御）。
    for c in &m.cards {
        if let Some(h) = &c.blob {
            if !cas::object_path(&blobs_root, h).is_file() {
                return Err(format!(
                    "モジュールの同梱データが見つかりません（{}）。ライブラリのフォルダが壊れている可能性があります",
                    c.label
                ));
            }
        }
    }
    let mut new_card_ids: Vec<String> = Vec::new();
    for c in &m.cards {
        let new_id = id_map.get(&c.id).unwrap().clone();
        // real_body: 実身コピー持ちは blob ハッシュ、それ以外（センチネル等）は元の行を再現。
        let real_id = if let Some(h) = &c.blob {
            // blobs → プロジェクト CAS へ materialize（内容アドレスなのでコピーだけ）。
            let src = cas::object_path(&blobs_root, h);
            let dst = cas::object_path(&p.cas_root, h);
            if !dst.exists() {
                if let Some(parent) = dst.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                std::fs::copy(&src, &dst)
                    .map_err(|e| format!("実身の取り込みに失敗（{}）: {e}", c.label))?;
            }
            p.conn
                .execute(
                    "INSERT OR IGNORE INTO real_bodies (id,mime_type,size,created_at,ext)
                     VALUES (?1,?2,?3,?4,?5)",
                    rusqlite::params![h, c.real_mime, c.blob_size as i64, db::now_ms(), c.real_ext],
                )
                .map_err(|e| e.to_string())?;
            h.clone()
        } else {
            p.conn
                .execute(
                    "INSERT OR IGNORE INTO real_bodies (id,mime_type,size,created_at,ext)
                     VALUES (?1,?2,0,?3,?4)",
                    rusqlite::params![c.real_id, c.real_mime, db::now_ms(), c.real_ext],
                )
                .map_err(|e| e.to_string())?;
            c.real_id.clone()
        };
        // window_of はモジュール内なら新IDへ貼替、外は持ち出し時点で無効なので NULL。
        let new_window = c.window_of.as_ref().and_then(|w| id_map.get(w).cloned());
        p.conn
            .execute(
                "INSERT INTO virtual_bodies
                    (id,real_id,label,pos_x,pos_y,created_at,kind,filter_ref,url_ref,
                     folder_ref,memo,z_order,ext,mime_type,window_of,note_text,style_ref,expanded)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
                rusqlite::params![
                    new_id,
                    real_id,
                    c.label,
                    x + c.x,
                    y + c.y,
                    db::now_ms(),
                    c.kind,
                    c.filter_ref,
                    c.url_ref,
                    c.folder_ref,
                    c.memo,
                    c.z,
                    c.ext,
                    c.mime_type,
                    new_window,
                    c.note_text,
                    c.style_ref,
                    c.expanded as i64,
                ],
            )
            .map_err(|e| e.to_string())?;
        new_card_ids.push(new_id);
    }

    // 矢印（attrs 込み・端点は新ID）。
    for e in &m.relations {
        if let (Some(nf), Some(nt)) = (id_map.get(&e.from), id_map.get(&e.to)) {
            p.conn
                .execute(
                    "INSERT INTO relations (from_id,to_id,kind,attrs,created_at)
                     VALUES (?1,?2,?3,?4,?5)",
                    rusqlite::params![nf, nt, e.kind, e.attrs, db::now_ms()],
                )
                .map_err(|e| e.to_string())?;
        }
    }

    // 境界の図形（ロック済み・members=新ID＝取り込み直後からモジュールとして保護）。
    let shape_id = uuid();
    let members_json = serde_json::to_string(&new_card_ids).map_err(|e| e.to_string())?;
    p.conn
        .execute(
            "INSERT INTO shapes
                (id,kind,pos_x,pos_y,width,height,rotation,fill,opacity,z_order,label,locked,members,created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,0,?10,1,?11,?12)",
            rusqlite::params![
                shape_id,
                m.shape.kind,
                x,
                y,
                m.shape.width,
                m.shape.height,
                m.shape.rotation,
                m.shape.fill,
                m.shape.opacity,
                m.shape.label,
                members_json,
                db::now_ms()
            ],
        )
        .map_err(|e| e.to_string())?;

    // §10.3 初回アテンション: 未導入の依存があれば注意カード（付箋）を図形の右に添える。
    // 案内コメントに矢印は付けない（付けるとプロンプト定数＝入力になってしまう・§10.4）。
    let note_id = if missing.is_empty() {
        None
    } else {
        let text = format!(
            "⚠ モジュール「{}」の依存が未導入です\n\n{}\n\nこのモジュールは外部プログラム・AI環境を使用します。初回実行時にセットアップを求められることがあります。",
            m.name,
            missing.iter().map(|s| format!("・{s}")).collect::<Vec<_>>().join("\n")
        );
        let nid = uuid();
        p.conn
            .execute(
                "INSERT OR IGNORE INTO real_bodies (id, mime_type, size, created_at)
                 VALUES ('note:sentinel', 'text/vnd.texos.note', 0, ?1)",
                rusqlite::params![db::now_ms()],
            )
            .map_err(|e| e.to_string())?;
        p.conn
            .execute(
                "INSERT INTO virtual_bodies (id, real_id, label, pos_x, pos_y, created_at, kind, note_text)
                 VALUES (?1, 'note:sentinel', ?2, ?3, ?4, ?5, 'note', ?6)",
                rusqlite::params![
                    nid,
                    "依存の注意",
                    x + m.shape.width + 24.0,
                    y,
                    db::now_ms(),
                    text
                ],
            )
            .map_err(|e| e.to_string())?;
        Some(nid)
    };

    Ok(PlaceModuleResult {
        new_card_ids,
        shape_id,
        note_id,
        missing,
    })
}

#[derive(Serialize)]
pub(crate) struct ExportLibrariesResult {
    pub path: String,
    pub filters: usize,
    pub launchers: usize,
    pub modules: usize,
}

/// 再帰コピー（modules/ のフォルダ構造をそのまま運ぶ）。
fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let to = dst.join(e.file_name());
        if e.path().is_dir() {
            copy_dir_all(&e.path(), &to)?;
        } else {
            std::fs::copy(e.path(), &to)?;
        }
    }
    Ok(())
}

/// T3 決着（§10.1）: 「プロジェクトに、すべての個人フィルタとモジュールを書き出し」
/// ＝バックアップの代用。ライブラリはプロジェクトに付いていかない（§10.1）ため、
/// zip で持ち出したいときはこのコマンドで明示的に写しを置く。
#[tauri::command]
pub(crate) fn export_libraries_to_project(state: State<AppState>) -> Result<ExportLibrariesResult, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let dest = p.root.join("ライブラリバックアップ");
    if dest.exists() {
        std::fs::remove_dir_all(&dest).map_err(|e| format!("古いバックアップの削除に失敗: {e}"))?;
    }
    std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;

    let filters = read_library(&state.app_data).len();
    let launchers = read_launcher_library(&state.app_data).len();
    for f in ["filter_library.json", "launcher_library.json"] {
        let src = state.app_data.join(f);
        if src.is_file() {
            std::fs::copy(&src, dest.join(f)).map_err(|e| e.to_string())?;
        }
    }
    let mdir = modules_dir(&state.app_data);
    let mut modules = 0usize;
    if mdir.is_dir() {
        if let Ok(rd) = std::fs::read_dir(&mdir) {
            modules = rd.flatten().filter(|e| e.path().join("module.json").is_file()).count();
        }
        copy_dir_all(&mdir, &dest.join("modules")).map_err(|e| e.to_string())?;
    }
    Ok(ExportLibrariesResult {
        path: dest.to_string_lossy().into_owned(),
        filters,
        launchers,
        modules,
    })
}

// ---------------------------------------------------------------------------
// v1.5.9: ライブラリバックアップからの取り込み（読み戻し・T3「バックアップ代用」の補完）
//
// 運用（2026-07-09 ユーザー確認）:
//   - 他人に渡す = バックアップ無しのプロジェクトのキャンバスに必要なモジュールを展開して渡す
//     （受け手は図形右クリック「ライブラリに登録」・§10.1 の正規ルート）
//   - 自分の復元 = 「ライブラリバックアップ」フォルダ入りプロジェクトから**選んで戻す**（本機能）
// ---------------------------------------------------------------------------

fn backup_dir(root: &Path) -> PathBuf {
    root.join("ライブラリバックアップ")
}

fn read_backup_lib(path: &Path) -> Vec<LibFilter> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

#[derive(Serialize)]
pub(crate) struct BackupEntry {
    pub name: String,
    /// 現在の個人ライブラリに同名があるか（取り込むと上書きになる）。
    pub exists: bool,
}

#[derive(Serialize)]
pub(crate) struct BackupList {
    pub filters: Vec<BackupEntry>,
    pub launchers: Vec<BackupEntry>,
    pub modules: Vec<BackupEntry>,
}

/// 開いているプロジェクトの「ライブラリバックアップ」フォルダの中身を一覧する
/// （メニュー「ライブラリバックアップから取り込み」の素材）。
#[tauri::command]
pub(crate) fn list_library_backup(state: State<AppState>) -> Result<BackupList, String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let dir = backup_dir(&p.root);
    if !dir.is_dir() {
        return Err(
            "このプロジェクトに「ライブラリバックアップ」フォルダがありません。\n書き出し元の環境で「個人ライブラリをプロジェクトへ書き出し」を実行したプロジェクトを開いてください".into(),
        );
    }
    let cur_filters: std::collections::HashSet<String> = read_library(&state.app_data)
        .into_iter()
        .map(|e| e.name)
        .collect();
    let cur_launchers: std::collections::HashSet<String> = read_launcher_library(&state.app_data)
        .into_iter()
        .map(|e| e.name)
        .collect();
    let filters = read_backup_lib(&dir.join("filter_library.json"))
        .into_iter()
        .map(|e| BackupEntry {
            exists: cur_filters.contains(&e.name),
            name: e.name,
        })
        .collect();
    let launchers = read_backup_lib(&dir.join("launcher_library.json"))
        .into_iter()
        .map(|e| BackupEntry {
            exists: cur_launchers.contains(&e.name),
            name: e.name,
        })
        .collect();
    let mut modules: Vec<BackupEntry> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir.join("modules")) {
        for e in rd.flatten() {
            if let Ok(s) = std::fs::read_to_string(e.path().join("module.json")) {
                if let Ok(m) = serde_json::from_str::<ModuleJson>(&s) {
                    let exists = modules_dir(&state.app_data)
                        .join(e.file_name())
                        .join("module.json")
                        .is_file();
                    modules.push(BackupEntry { name: m.name, exists });
                }
            }
        }
    }
    modules.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(BackupList {
        filters,
        launchers,
        modules,
    })
}

/// バックアップから1件を個人ライブラリへ取り込む（同名は上書き＝復元の意味論）。
/// kind = "filter" | "launcher" | "module"。
#[tauri::command]
pub(crate) fn import_library_backup_entry(
    kind: String,
    name: String,
    state: State<AppState>,
) -> Result<(), String> {
    let guard = state.current.lock().unwrap();
    let p = guard.as_ref().ok_or("プロジェクトが開かれていません")?;
    let dir = backup_dir(&p.root);
    match kind.as_str() {
        "filter" => {
            let entry = read_backup_lib(&dir.join("filter_library.json"))
                .into_iter()
                .find(|e| e.name == name)
                .ok_or_else(|| format!("バックアップにフィルタ「{name}」がありません"))?;
            flow::parse_spec(&entry.spec)?; // 壊れた定義を戻さない（save_filter_library と同じ）
            let mut list = read_library(&state.app_data);
            list.retain(|e| e.name != name);
            list.push(entry);
            list.sort_by(|a, b| a.name.cmp(&b.name));
            write_library(&state.app_data, &list)
        }
        "launcher" => {
            let entry = read_backup_lib(&dir.join("launcher_library.json"))
                .into_iter()
                .find(|e| e.name == name)
                .ok_or_else(|| format!("バックアップにランチャー「{name}」がありません"))?;
            flow::parse_spec(&entry.spec)?;
            let mut list = read_launcher_library(&state.app_data);
            list.retain(|e| e.name != name);
            list.push(entry);
            list.sort_by(|a, b| a.name.cmp(&b.name));
            let json = serde_json::to_string_pretty(&list).map_err(|e| e.to_string())?;
            std::fs::write(launcher_library_path(&state.app_data), json).map_err(|e| e.to_string())
        }
        "module" => {
            // フォルダ名でなく module.json の name で照合（sanitize 前の名前が正）。
            let src_root = dir.join("modules");
            let mut found: Option<(PathBuf, std::ffi::OsString)> = None;
            if let Ok(rd) = std::fs::read_dir(&src_root) {
                for e in rd.flatten() {
                    if let Ok(s) = std::fs::read_to_string(e.path().join("module.json")) {
                        if let Ok(m) = serde_json::from_str::<ModuleJson>(&s) {
                            if m.name == name {
                                found = Some((e.path(), e.file_name()));
                                break;
                            }
                        }
                    }
                }
            }
            let (src, folder) =
                found.ok_or_else(|| format!("バックアップにモジュール「{name}」がありません"))?;
            let dst = modules_dir(&state.app_data).join(&folder);
            if dst.exists() {
                std::fs::remove_dir_all(&dst)
                    .map_err(|e| format!("既存モジュールの削除に失敗: {e}"))?;
            }
            copy_dir_all(&src, &dst).map_err(|e| format!("コピーに失敗: {e}"))
        }
        other => Err(format!("不明な種別です: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// §10.3: spec 走査による依存の自動収集（外部プログラム・AI・t2i/whisper）と重複排除。
    #[test]
    fn requires_are_collected_and_deduped() {
        let specs: Vec<String> = [
            r#"{"name":"sp2tab.exe","cmd":"\"{in}\" > \"{out}\""}"#,
            r#"{"name":"sp2tab.exe","cmd":"-v \"{in}\""}"#, // 同名プログラムは1回
            r#"{"builtin":"ai","model":"gemma3:4b"}"#,
            r#"{"builtin":"ai","model":"gemma3:4b"}"#, // 同一 AI 依存も1回
            r#"{"builtin":"t2i","model":"sd15"}"#,
            r#"{"builtin":"whisper"}"#,
            r#"{"builtin":"text_grep","pattern":"x"}"#, // 内蔵のみ＝依存なし
            r#"{"op":"in"}"#,                            // op spec は parse_spec が拒否＝スキップ
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let req = collect_requires(&specs);
        assert_eq!(req.programs, vec!["sp2tab.exe"]);
        assert_eq!(req.ai.len(), 1);
        assert_eq!(req.ai[0].provider, "ollama");
        assert_eq!(req.ai[0].model, "gemma3:4b");
        assert_eq!(req.ai[0].endpoint, "http://localhost:11434");
        assert_eq!(req.builtins, vec!["t2i", "whisper"]);
    }

    /// モジュールのフォルダ名: パスに使えない文字を潰し、長さを抑える。
    #[test]
    fn sanitize_name_strips_path_chars() {
        assert_eq!(sanitize_name(r#" a/b\c:d*e?f"g<h>i|j "#), "a_b_c_d_e_f_g_h_i_j");
        assert_eq!(sanitize_name("日本語モジュール"), "日本語モジュール");
        assert_eq!(sanitize_name("").len(), 0);
    }
}
