// --- ComfyUI バックエンド（t2i カードの backend:"comfyui"）--------------------------------
// docs/ComfyUIバックエンド設計.md の Phase 1。カードは ComfyUI の **API 形式ワークフロー JSON** と
// 「差し込み口（バインディング）」を持ち、TexoS は値を差し込んで外部の ComfyUI に投げる:
//   /upload/image（画像入力）→ POST /prompt → GET /history ポーリング → GET /view（PNG）
// ComfyUI は外のサーバ（TexoS は起動・停止しない）。Ollama / LM Studio と同じ扱い。
//
// 純関数（検出・差し込み・検証）と HTTP を分けてある＝前者は ComfyUI 無しで単体テストできる。

use serde::Serialize;
use serde_json::{json, Map, Value};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::flow::FilterSpec;

/// 既定の接続先（ComfyUI の既定ポート）。カードの `endpoint` が空ならこれ。
pub const COMFY_DEFAULT_URL: &str = "http://127.0.0.1:8188";

/// 差し込み口 1 件。`slot` は FilterSpec の名前に揃える
/// （prompt / negative / seed / steps / guidance / width / height / strength / image）。
#[derive(serde::Deserialize, Serialize, Clone, Debug, PartialEq)]
pub struct ComfyBinding {
    pub slot: String,
    /// ノード ID（API 形式のキー。サブグラフ内は "459:452" のような複合 ID）。
    pub node: String,
    /// そのノードの入力名。
    pub input: String,
    /// slot=image のときの番号（1 始まり＝画像入力の何枚目か）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<u32>,
}

/// カードがこのバックエンドを使うか。
pub fn is_comfy(spec: &FilterSpec) -> bool {
    spec.backend.as_deref().map(str::trim) == Some("comfyui")
}

/// 接続先 URL（明示 endpoint 優先・空は既定）。末尾の `/` は落とす。
pub fn effective_endpoint(spec: &FilterSpec) -> String {
    normalize_base(spec.endpoint.as_deref().unwrap_or(""))
}

fn normalize_base(s: &str) -> String {
    let s = s.trim().trim_end_matches('/');
    if s.is_empty() {
        COMFY_DEFAULT_URL.to_string()
    } else {
        s.to_string()
    }
}

/// この PC 上の ComfyUI か（GPU を TexoS の他の生成と取り合うか）。
pub fn is_local(base: &str) -> bool {
    let rest = base.split("://").nth(1).unwrap_or(base);
    let host = rest.split('/').next().unwrap_or("");
    let host = if host.starts_with('[') {
        host.split(']').next().unwrap_or("").trim_start_matches('[')
    } else {
        host.split(':').next().unwrap_or("")
    };
    matches!(host.to_ascii_lowercase().as_str(), "127.0.0.1" | "localhost" | "::1")
}

// ============================================================================================
// 純関数: 形式判定・差し込み口の自動検出・差し込み・検証
// ============================================================================================

/// 文字列をワークフロー（API 形式）として読む。UI 形式は案内付きで拒否する。
pub fn parse_workflow(text: &str) -> Result<Value, String> {
    let v: Value = serde_json::from_str(text.trim()).map_err(|e| format!("JSON として読めません: {e}"))?;
    // /prompt の本文（{"prompt": {...}, "client_id": ...}）を丸ごと貼られた場合は中身を採る。
    let v = match v.get("prompt") {
        Some(p) if p.is_object() && v.get("class_type").is_none() => p.clone(),
        _ => v,
    };
    check_workflow(&v)?;
    Ok(v)
}

/// API 形式か（全エントリが class_type と inputs を持つオブジェクト）。
pub fn check_workflow(v: &Value) -> Result<(), String> {
    let obj = v.as_object().ok_or("ワークフローが JSON オブジェクトではありません")?;
    if obj.contains_key("nodes") && obj.contains_key("links") {
        return Err(
            "これは ComfyUI の UI 形式（保存ファイル・テンプレート）です。ComfyUI のメニュー「ワークフロー → エクスポート（API）」で書き出した JSON を貼ってください"
                .into(),
        );
    }
    if obj.is_empty() {
        return Err("ワークフローが空です".into());
    }
    for (id, n) in obj {
        if n.get("class_type").and_then(Value::as_str).is_none() || !n.get("inputs").is_some_and(Value::is_object) {
            return Err(format!(
                "ノード {id} に class_type / inputs がありません（API 形式で書き出した JSON を貼ってください）"
            ));
        }
    }
    Ok(())
}

fn class_of<'a>(wf: &'a Value, id: &str) -> &'a str {
    wf.get(id).and_then(|n| n.get("class_type")).and_then(Value::as_str).unwrap_or("")
}

fn inputs_of<'a>(wf: &'a Value, id: &str) -> Option<&'a Map<String, Value>> {
    wf.get(id).and_then(|n| n.get("inputs")).and_then(Value::as_object)
}

/// 入力値が他ノードへのリンク `["<id>", n]` ならその (id, n)。
fn as_link(v: &Value) -> Option<(String, u64)> {
    let a = v.as_array()?;
    if a.len() != 2 {
        return None;
    }
    let id = match &a[0] {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    Some((id, a[1].as_u64()?))
}

/// 分岐ノード（If/Else Switch 等＝`switch` が真偽値リテラルで on_true / on_false を持つ）なら、
/// 実際に使われる側の入力名。
fn switch_branch(inputs: &Map<String, Value>) -> Option<&'static str> {
    let sw = inputs.get("switch")?.as_bool()?;
    if inputs.contains_key("on_true") && inputs.contains_key("on_false") {
        Some(if sw { "on_true" } else { "on_false" })
    } else {
        None
    }
}

/// 画像を出す出力ノードの候補（Save* / Preview Image）。比較・テキスト表示ノードは除く。
fn is_image_output(class: &str) -> bool {
    class.starts_with("SaveImage") || class == "PreviewImage"
}

/// 出力ノードの既定（Save* を優先・同類は ID 順）。
pub fn detect_output(wf: &Value) -> Option<String> {
    let obj = wf.as_object()?;
    let mut ids: Vec<&String> = obj.keys().filter(|id| is_image_output(class_of(wf, id))).collect();
    ids.sort_by_key(|id| (!class_of(wf, id).starts_with("SaveImage"), id_order_key(id)));
    ids.first().map(|s| s.to_string())
}

/// ID の並べ順（"459:452" も数値として比べる）。
fn id_order_key(id: &str) -> Vec<u64> {
    id.split(':').map(|p| p.parse().unwrap_or(u64::MAX)).collect()
}

/// 出力ノードから遡って「実際に計算に使われる」ノード集合（分岐は選ばれた側だけ辿る）。
fn active_nodes(wf: &Value, output: &str) -> std::collections::HashSet<String> {
    let mut seen = std::collections::HashSet::new();
    let mut stack = vec![output.to_string()];
    while let Some(id) = stack.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        let Some(ins) = inputs_of(wf, &id) else { continue };
        let branch = switch_branch(ins);
        for (k, v) in ins {
            if let Some(b) = branch {
                if (k == "on_true" || k == "on_false") && k != b {
                    continue;
                }
            }
            if let Some((src, _)) = as_link(v) {
                stack.push(src);
            }
        }
    }
    seen
}

/// テキストの差し込み先を辿る: 文字列リテラルに行き着いた (node, input) を返す。
/// 経由するノード（テキスト表示・分岐・文字列プリミティブ等）はリンクを追うだけ。
fn trace_text(wf: &Value, id: &str, keys: &[&str], depth: u32) -> Option<(String, String)> {
    if depth > 8 {
        return None;
    }
    let ins = inputs_of(wf, id)?;
    let mut order: Vec<&str> = Vec::new();
    if let Some(b) = switch_branch(ins) {
        order.push(b);
    }
    order.extend_from_slice(keys);
    order.extend_from_slice(&["text", "prompt", "value", "string", "source"]);
    for k in order {
        let Some(v) = ins.get(k) else { continue };
        if v.is_string() {
            return Some((id.to_string(), k.to_string()));
        }
        if let Some((src, _)) = as_link(v) {
            if let Some(hit) = trace_text(wf, &src, &[], depth + 1) {
                return Some(hit);
            }
        }
    }
    None
}

/// 差し込み口の自動検出（初期値。ユーザーが確認・修正する）。返り値は (口, 気づき)。
pub fn detect_bindings(wf: &Value, output: &str) -> (Vec<ComfyBinding>, Vec<String>) {
    let mut out: Vec<ComfyBinding> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    let active = active_nodes(wf, output);
    let mut ids: Vec<&String> = active.iter().collect();
    ids.sort_by_key(|id| id_order_key(id));
    let push = |out: &mut Vec<ComfyBinding>, slot: &str, node: &str, input: &str, index: Option<u32>| {
        if !out.iter().any(|b| b.node == node && b.input == input) {
            out.push(ComfyBinding { slot: slot.into(), node: node.into(), input: input.into(), index });
        }
    };

    // 1) 正・負プロンプト: positive / negative を持つノード（KSampler・CFGGuider 等）から辿る。
    for id in &ids {
        let Some(ins) = inputs_of(wf, id) else { continue };
        let (Some(pos), neg) = (ins.get("positive").and_then(as_link), ins.get("negative").and_then(as_link)) else {
            continue;
        };
        if let Some((n, k)) = trace_text(wf, &pos.0, &["prompt", "text"], 0) {
            push(&mut out, "prompt", &n, &k, None);
        }
        if let Some((nid, _)) = neg {
            // 1 つのエンコーダが正負を両方出す型（Qwen-Image 2.1 の TextEncode）は negative_prompt。
            let same = nid == pos.0;
            let neg_lit = inputs_of(wf, &nid)
                .and_then(|i| i.get("negative_prompt"))
                .is_some_and(Value::is_string);
            if neg_lit {
                push(&mut out, "negative", &nid, "negative_prompt", None);
            } else if !same {
                if let Some((n, k)) = trace_text(wf, &nid, &["text"], 0) {
                    push(&mut out, "negative", &n, &k, None);
                }
            }
        }
        break; // 最初のサンプラーだけ（多段サンプラーは手で足す）
    }
    if !out.iter().any(|b| b.slot == "prompt") {
        notes.push("正プロンプトの差し込み先が見つかりませんでした。手で追加してください".into());
    }

    // 2) 数値: シード・ステップ・CFG・サイズ（リテラルのものだけ）。
    let lit_num = |ins: &Map<String, Value>, k: &str| ins.get(k).is_some_and(Value::is_number);
    for id in &ids {
        let Some(ins) = inputs_of(wf, id) else { continue };
        let class = class_of(wf, id);
        for k in ["seed", "noise_seed"] {
            if lit_num(ins, k) && !out.iter().any(|b| b.slot == "seed") {
                push(&mut out, "seed", id, k, None);
            }
        }
        if lit_num(ins, "steps") && !out.iter().any(|b| b.slot == "steps") {
            push(&mut out, "steps", id, "steps", None);
        }
        if lit_num(ins, "cfg") && !out.iter().any(|b| b.slot == "guidance") {
            push(&mut out, "guidance", id, "cfg", None);
        }
        if class.contains("Latent") && lit_num(ins, "width") && lit_num(ins, "height") && !out.iter().any(|b| b.slot == "width") {
            push(&mut out, "width", id, "width", None);
            push(&mut out, "height", id, "height", None);
        }
    }
    if !out.iter().any(|b| b.slot == "width") {
        notes.push("画像サイズはワークフロー側で決まります（リテラルの width/height が見つかりません）".into());
    }

    // 3) 画像入力: 使われている LoadImage。番号は受け側の入力名（images.image_1 等）から採る。
    let mut loads: Vec<(u32, String)> = Vec::new();
    for id in &ids {
        if class_of(wf, id) != "LoadImage" {
            continue;
        }
        let mut num = u32::MAX;
        for cid in &ids {
            let Some(ins) = inputs_of(wf, cid) else { continue };
            for (k, v) in ins {
                if as_link(v).is_some_and(|(s, _)| &s == *id) {
                    if let Some(n) = trailing_number(k) {
                        num = num.min(n);
                    }
                }
            }
        }
        loads.push((num, id.to_string()));
    }
    loads.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| id_order_key(&a.1).cmp(&id_order_key(&b.1))));
    for (i, (_, id)) in loads.iter().enumerate() {
        push(&mut out, "image", id, "image", Some(i as u32 + 1));
    }
    (out, notes)
}

/// "images.image_3" → 3 / "image2" → 2。
fn trailing_number(k: &str) -> Option<u32> {
    let digits: String = k.chars().rev().take_while(char::is_ascii_digit).collect::<Vec<_>>().into_iter().rev().collect();
    if digits.is_empty() || !k.to_ascii_lowercase().contains("image") {
        return None;
    }
    digits.parse().ok()
}

/// 1 回の実行で差し込む値。
#[derive(Debug, Default, Clone)]
pub struct SlotValues {
    pub prompt: String,
    pub negative: Option<String>,
    pub seed: i64,
    pub steps: Option<u32>,
    pub guidance: Option<f64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub strength: Option<f64>,
    /// アップロード済みの画像名（LoadImage.image に入れる値。矢印順）。
    pub images: Vec<String>,
}

/// ワークフローのコピーに値を差し込む。返り値は (投げる JSON, 気づき)。
/// 画像口より画像が少ないとき、余った LoadImage はノードごと外し、それを参照する入力キーも消す
/// （未接続の画像口は optional 扱い＝実機確認済み。設計メモ §1-4）。
pub fn build_prompt(wf: &Value, bindings: &[ComfyBinding], v: &SlotValues) -> Result<(Value, Vec<String>), String> {
    let mut wf = wf.clone();
    let mut notes = Vec::new();
    let image_slots: Vec<&ComfyBinding> = bindings.iter().filter(|b| b.slot == "image").collect();
    if !image_slots.is_empty() && v.images.is_empty() {
        return Err(format!(
            "このワークフローは画像入力を使います（{} 枚分）。画像カードを接続してください",
            image_slots.len()
        ));
    }
    if v.images.len() > image_slots.len() {
        notes.push(format!(
            "画像が {} 枚来ましたが、差し込み口は {} 個です（余りは使いません）",
            v.images.len(),
            image_slots.len()
        ));
    }
    let mut removed: Vec<String> = Vec::new();
    for b in bindings {
        let val: Option<Value> = match b.slot.as_str() {
            "prompt" => Some(json!(v.prompt)),
            "negative" => v.negative.as_ref().map(|s| json!(s)),
            "seed" => Some(json!(v.seed)),
            "steps" => v.steps.map(|x| json!(x)),
            "guidance" => v.guidance.map(|x| json!(x)),
            "width" => v.width.map(|x| json!(x)),
            "height" => v.height.map(|x| json!(x)),
            "strength" => v.strength.map(|x| json!(x)),
            "image" => {
                let i = b.index.unwrap_or(1).max(1) as usize;
                match v.images.get(i - 1) {
                    Some(name) => Some(json!(name)),
                    None => {
                        removed.push(b.node.clone());
                        None
                    }
                }
            }
            other => return Err(format!("未知の差し込み口: {other}")),
        };
        let Some(val) = val else { continue };
        let node = wf
            .get_mut(&b.node)
            .ok_or_else(|| format!("差し込み先のノード {} がワークフローにありません", b.node))?;
        let ins = node
            .get_mut("inputs")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| format!("ノード {} に inputs がありません", b.node))?;
        ins.insert(b.input.clone(), val);
    }
    if !removed.is_empty() {
        let obj = wf.as_object_mut().unwrap();
        for id in &removed {
            obj.remove(id);
        }
        for n in obj.values_mut() {
            if let Some(ins) = n.get_mut("inputs").and_then(Value::as_object_mut) {
                ins.retain(|_, val| !as_link(val).is_some_and(|(s, _)| removed.contains(&s)));
            }
        }
    }
    Ok((wf, notes))
}

/// `/object_info` の 1 入力の定義から combo の選択肢を取り出す（旧形式 [[...]] と新形式 ["COMBO", {options}]）。
fn combo_options(def: &Value) -> Option<Vec<String>> {
    let a = def.as_array()?;
    let opts = match a.first()? {
        Value::Array(list) => list.clone(),
        Value::String(s) if s == "COMBO" => a.get(1)?.get("options")?.as_array()?.clone(),
        _ => return None,
    };
    Some(opts.iter().filter_map(|o| o.as_str().map(str::to_string)).collect())
}

/// `/object_info` と突き合わせた検証。返り値は (エラー, 注意)。
pub fn validate(wf: &Value, bindings: &[ComfyBinding], output: Option<&str>, info: &Value) -> (Vec<String>, Vec<String>) {
    let mut errs = Vec::new();
    let mut warns = Vec::new();
    let Some(obj) = wf.as_object() else {
        return (vec!["ワークフローが JSON オブジェクトではありません".into()], warns);
    };
    let input_def = |class: &str, input: &str| -> Option<Value> {
        let i = info.get(class)?.get("input")?;
        i.get("required").and_then(|r| r.get(input)).or_else(|| i.get("optional").and_then(|r| r.get(input))).cloned()
    };
    for (id, n) in obj {
        let class = n.get("class_type").and_then(Value::as_str).unwrap_or("");
        if info.get(class).is_none() {
            errs.push(format!("ノード {id}: 「{class}」が ComfyUI にありません（カスタムノード未導入か、名前が変わった可能性）"));
            continue;
        }
        // モデル系 combo（*_name）の現在値が選択肢にあるか。
        if let Some(ins) = n.get("inputs").and_then(Value::as_object) {
            for (k, val) in ins {
                let Some(s) = val.as_str() else { continue };
                if !k.ends_with("_name") || k == "sampler_name" {
                    continue;
                }
                if let Some(opts) = input_def(class, k).as_ref().and_then(combo_options) {
                    if !opts.iter().any(|o| o == s) {
                        errs.push(format!("ノード {id}（{class}）: モデル「{s}」が ComfyUI にありません（{k}）"));
                    }
                }
            }
        }
    }
    for b in bindings {
        let Some(n) = obj.get(&b.node) else {
            errs.push(format!("差し込み口 {}: ノード {} がワークフローにありません", b.slot, b.node));
            continue;
        };
        let class = n.get("class_type").and_then(Value::as_str).unwrap_or("");
        // 動的入力（"images.image_1" のような . 付き）は object_info に載らないので名前検査をしない。
        if info.get(class).is_some() && !b.input.contains('.') && input_def(class, &b.input).is_none() {
            errs.push(format!("差し込み口 {}: 「{class}」に入力「{}」がありません（名前が変わった可能性）", b.slot, b.input));
        }
    }
    match output {
        Some(o) if !obj.contains_key(o) => errs.push(format!("出力ノード {o} がワークフローにありません")),
        None => warns.push("画像の出力ノード（Save Image / Preview Image）が見つかりません".into()),
        _ => {}
    }
    (errs, warns)
}

// ============================================================================================
// HTTP（reqwest::blocking — 呼び出し側が tokio 文脈外のスレッドで回す）
// ============================================================================================

fn client(timeout_secs: u64) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build()
        .map_err(|e| e.to_string())
}

fn conn_err(base: &str, e: reqwest::Error) -> String {
    if e.is_connect() || e.is_timeout() {
        format!("ComfyUI（{base}）に接続できません。ComfyUI を起動してください")
    } else {
        format!("ComfyUI への要求に失敗: {e}")
    }
}

fn get_json(base: &str, path: &str, timeout: u64) -> Result<Value, String> {
    let r = client(timeout)?.get(format!("{base}{path}")).send().map_err(|e| conn_err(base, e))?;
    if !r.status().is_success() {
        return Err(format!("ComfyUI {path}: HTTP {}", r.status()));
    }
    r.json().map_err(|e| format!("ComfyUI {path} の応答を読めません: {e}"))
}

fn post_json(base: &str, path: &str, body: &Value) -> Result<reqwest::blocking::Response, String> {
    client(30)?.post(format!("{base}{path}")).json(body).send().map_err(|e| conn_err(base, e))
}

/// 接続テストの結果（設定ダイアログ表示用）。
#[derive(Serialize)]
pub struct ComfyCheck {
    pub url: String,
    pub version: String,
    /// (名前, VRAM 合計 MB, VRAM 空き MB)
    pub devices: Vec<(String, u64, u64)>,
}

pub fn check(base: &str) -> Result<ComfyCheck, String> {
    let v = get_json(base, "/system_stats", 5)?;
    let version = v["system"]["comfyui_version"].as_str().unwrap_or("?").to_string();
    let devices = v["devices"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|d| {
                    (
                        d["name"].as_str().unwrap_or("").to_string(),
                        d["vram_total"].as_u64().unwrap_or(0) / 1_048_576,
                        d["vram_free"].as_u64().unwrap_or(0) / 1_048_576,
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(ComfyCheck { url: base.to_string(), version, devices })
}

/// `/object_info`（数 MB）を接続先＋版ごとにキャッシュして返す。
fn object_info(base: &str) -> Result<std::sync::Arc<Value>, String> {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<String, (String, Arc<Value>)>>> = OnceLock::new();
    let version = check(base)?.version;
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some((ver, v)) = cache.lock().unwrap().get(base) {
        if *ver == version {
            return Ok(v.clone());
        }
    }
    let v = Arc::new(get_json(base, "/object_info", 60)?);
    cache.lock().unwrap().insert(base.to_string(), (version, v.clone()));
    Ok(v)
}

/// GPU を空けさせる（ローカルの ComfyUI が載せているモデルを全部下ろす）。best-effort・短時間。
/// 自前 t2i / t23d デーモンで生成する直前に呼ぶ（VRAM 排他・設計メモ §4-2）。
pub fn free_local() {
    let _ = std::thread::scope(|s| {
        s.spawn(|| {
            if let Ok(c) = reqwest::blocking::Client::builder()
                .connect_timeout(std::time::Duration::from_millis(500))
                .timeout(std::time::Duration::from_secs(3))
                .build()
            {
                let _ = c
                    .post(format!("{COMFY_DEFAULT_URL}/free"))
                    .json(&json!({"unload_models": true, "free_memory": true}))
                    .send();
            }
        })
        .join()
    });
}

/// 画像の拡張子（先頭バイトで判定・既定 png）。
fn image_ext(b: &[u8]) -> (&'static str, &'static str) {
    if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        ("jpg", "image/jpeg")
    } else if b.len() > 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        ("webp", "image/webp")
    } else {
        ("png", "image/png")
    }
}

/// 画像を ComfyUI の input/texos/ に上げる。名前は CAS ハッシュ＝同じ画像は上書きで溜まらない。
fn upload_image(base: &str, bytes: &[u8]) -> Result<String, String> {
    let (ext, mime) = image_ext(bytes);
    let name = format!("{}.{ext}", &crate::cas::hash_bytes(bytes)[..32]);
    let part = reqwest::blocking::multipart::Part::bytes(bytes.to_vec())
        .file_name(name.clone())
        .mime_str(mime)
        .map_err(|e| e.to_string())?;
    let form = reqwest::blocking::multipart::Form::new()
        .part("image", part)
        .text("type", "input")
        .text("subfolder", "texos")
        .text("overwrite", "true");
    let r = client(120)?
        .post(format!("{base}/upload/image"))
        .multipart(form)
        .send()
        .map_err(|e| conn_err(base, e))?;
    if !r.status().is_success() {
        return Err(format!("画像のアップロードに失敗（HTTP {}）", r.status()));
    }
    let v: Value = r.json().map_err(|e| format!("アップロード応答を読めません: {e}"))?;
    let n = v["name"].as_str().unwrap_or(&name);
    let sub = v["subfolder"].as_str().unwrap_or("");
    Ok(if sub.is_empty() { n.to_string() } else { format!("{sub}/{n}") })
}

/// `/prompt` の 400（検証エラー）を読める文にする。
fn format_prompt_error(v: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(m) = v["error"]["message"].as_str() {
        let d = v["error"]["details"].as_str().unwrap_or("");
        parts.push(if d.is_empty() { m.to_string() } else { format!("{m}: {d}") });
    }
    if let Some(ne) = v["node_errors"].as_object() {
        for (id, e) in ne {
            let class = e["class_type"].as_str().unwrap_or("");
            for er in e["errors"].as_array().into_iter().flatten() {
                let m = er["message"].as_str().unwrap_or("");
                let d = er["details"].as_str().unwrap_or("");
                parts.push(format!("ノード {id}（{class}）: {m} {d}").trim().to_string());
            }
        }
    }
    if parts.is_empty() {
        v.to_string()
    } else {
        parts.join(" / ")
    }
}

/// 自分のジョブだけ止める（v0.41.0 は /interrupt に prompt_id を渡せる）＋キュー待ちなら取り下げ。
fn cancel_prompt(base: &str, prompt_id: &str) {
    let _ = post_json(base, "/interrupt", &json!({ "prompt_id": prompt_id }));
    let _ = post_json(base, "/queue", &json!({ "delete": [prompt_id] }));
}

/// 乱数シード（ComfyUI の seed は非負。JS で扱える 2^53 未満に収める）。
fn random_seed() -> i64 {
    let h = crate::cas::hash_bytes(format!("{:?}{}", std::time::SystemTime::now(), std::process::id()).as_bytes());
    (u64::from_str_radix(&h[..13], 16).unwrap_or(1) & ((1u64 << 50) - 1)) as i64
}

/// 実行結果（実行ログ用の付帯情報つき）。
pub struct ComfyRun {
    pub bytes: Vec<u8>,
    pub ext: String,
    pub mime: String,
    pub seed: i64,
    pub notes: Vec<String>,
}

/// builtin:"t2i" backend:"comfyui" の 1 回の生成。`cancel` は中断ボタン（ジョブの cancel フラグ）。
pub fn run(spec: &FilterSpec, prompt: &str, images: &[&[u8]], cancel: &AtomicBool) -> Result<ComfyRun, String> {
    std::thread::scope(|s| {
        s.spawn(|| run_blocking(spec, prompt, images, cancel))
            .join()
            .map_err(|_| "画像生成スレッドが異常終了しました".to_string())?
    })
}

fn run_blocking(spec: &FilterSpec, prompt: &str, images: &[&[u8]], cancel: &AtomicBool) -> Result<ComfyRun, String> {
    let base = effective_endpoint(spec);
    let wf = spec
        .comfy_workflow
        .as_ref()
        .ok_or("ComfyUI のワークフローが未設定です（カードの設定で API 形式の JSON を取り込んでください）")?;
    check_workflow(wf)?;
    let bindings = spec.comfy_bindings.clone().unwrap_or_default();
    let prompt = prompt.trim();
    if prompt.is_empty() && bindings.iter().any(|b| b.slot == "prompt") {
        return Err("入力テキスト（プロンプト）が空です".into());
    }

    let mut uploaded = Vec::new();
    for img in images {
        if cancel.load(Ordering::Relaxed) {
            return Err("中断されました".into());
        }
        uploaded.push(upload_image(&base, img)?);
    }
    let seed = spec.seed.unwrap_or_else(random_seed);
    let values = SlotValues {
        prompt: prompt.to_string(),
        negative: spec.negative.clone().filter(|s| !s.trim().is_empty()),
        seed,
        steps: spec.steps,
        guidance: spec.guidance,
        width: spec.width,
        height: spec.height,
        strength: spec.strength,
        images: uploaded,
    };
    let (body, notes) = build_prompt(wf, &bindings, &values)?;

    let client_id = "texos-comfy";
    let r = post_json(&base, "/prompt", &json!({ "prompt": body, "client_id": client_id }))?;
    let status = r.status();
    let v: Value = r.json().unwrap_or_else(|_| json!({}));
    if !status.is_success() {
        return Err(format!("ComfyUI がワークフローを受け付けません: {}", format_prompt_error(&v)));
    }
    let prompt_id = v["prompt_id"].as_str().ok_or("ComfyUI が prompt_id を返しませんでした")?.to_string();

    // 完了待ち（/history ポーリング）。制限時間は経過時間で判定（未指定/0＝無制限）。
    let started = std::time::Instant::now();
    let limit = spec.timeout_secs.filter(|&s| s > 0).map(std::time::Duration::from_secs);
    let entry = loop {
        if cancel.load(Ordering::Relaxed) {
            cancel_prompt(&base, &prompt_id);
            return Err("中断されました".into());
        }
        if limit.is_some_and(|l| started.elapsed() > l) {
            cancel_prompt(&base, &prompt_id);
            return Err("画像生成がタイムアウトしました（ComfyUI 側の生成は中断しました）。制限時間を延ばしてください".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(1000));
        let h = match get_json(&base, &format!("/history/{prompt_id}"), 10) {
            Ok(h) => h,
            // 一時的な不通は待ち続ける（制限時間・中断で抜けられる）。
            Err(_) => continue,
        };
        if let Some(e) = h.get(&prompt_id) {
            if e["status"]["completed"].as_bool() == Some(true) || e["status"]["status_str"].as_str() == Some("error") {
                break e.clone();
            }
        }
    };
    if entry["status"]["status_str"].as_str() == Some("error") {
        let msg = entry["status"]["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|m| m[0].as_str() == Some("execution_error"))
            .map(|m| {
                let d = &m[1];
                let ex = d["exception_message"].as_str().unwrap_or("").trim();
                let oom = ex.to_ascii_lowercase().contains("out of memory") || d["exception_type"].as_str().unwrap_or("").contains("OutOfMemory");
                let hint = if oom { "（GPU メモリ不足。TexoS の画像生成・3D 生成サーバや他のアプリを止めてから再実行してください）" } else { "" };
                format!("ノード {}（{}）: {ex}{hint}", d["node_id"].as_str().unwrap_or("?"), d["node_type"].as_str().unwrap_or(""))
            })
            .unwrap_or_else(|| "ComfyUI の実行に失敗しました".into());
        return Err(format!("ComfyUI 実行エラー: {msg}"));
    }

    // 画像を選ぶ: 指定の出力ノード → 無ければ outputs の最初の画像。
    let outputs = entry["outputs"].as_object().cloned().unwrap_or_default();
    let pick = |id: &str| outputs.get(id).and_then(|o| o["images"].as_array()).and_then(|a| a.first()).cloned();
    let img = spec
        .comfy_output
        .as_deref()
        .and_then(pick)
        .or_else(|| {
            let mut ids: Vec<&String> = outputs.keys().collect();
            ids.sort_by_key(|id| id_order_key(id));
            ids.into_iter().find_map(|id| pick(id))
        })
        .ok_or("ComfyUI の結果に画像がありません（前回と全く同じ入力だとキャッシュで出力が省かれることがあります。シードを空欄＝毎回にしてください）")?;
    let q = [
        ("filename", img["filename"].as_str().unwrap_or("")),
        ("subfolder", img["subfolder"].as_str().unwrap_or("")),
        ("type", img["type"].as_str().unwrap_or("output")),
    ];
    let r = client(60)?
        .get(format!("{base}/view"))
        .query(&q)
        .send()
        .map_err(|e| conn_err(&base, e))?;
    if !r.status().is_success() {
        return Err(format!("画像の取得に失敗（HTTP {}）", r.status()));
    }
    let bytes = r.bytes().map_err(|e| format!("画像の受信に失敗: {e}"))?.to_vec();
    if bytes.is_empty() {
        return Err("ComfyUI が空の画像を返しました".into());
    }
    let (ext, mime) = image_ext(&bytes);
    Ok(ComfyRun { bytes, ext: ext.into(), mime: mime.into(), seed, notes })
}

// ============================================================================================
// Tauri コマンド（設定ダイアログ用）
// ============================================================================================

#[tauri::command(async)]
pub(crate) fn comfy_check(endpoint: Option<String>) -> Result<ComfyCheck, String> {
    let base = normalize_base(endpoint.as_deref().unwrap_or(""));
    std::thread::scope(|s| s.spawn(|| check(&base)).join().map_err(|_| "異常終了".to_string())?)
}

#[derive(Serialize)]
pub struct ComfyAnalysis {
    /// 読み込んだワークフロー（/prompt 本文を貼られた場合は中身だけ）。
    pub workflow: Value,
    pub bindings: Vec<ComfyBinding>,
    pub output: Option<String>,
    /// ノード ID → (class_type, タイトル)。バインディング表の表示用。
    pub nodes: Vec<(String, String, String)>,
    pub errors: Vec<String>,
    pub notes: Vec<String>,
}

/// ワークフローの取り込み・解析。`bindings`/`output` を渡すと検出せずそれを検証する（編集時）。
#[tauri::command(async)]
pub(crate) fn comfy_analyze(
    endpoint: Option<String>,
    workflow: String,
    bindings: Option<Vec<ComfyBinding>>,
    output: Option<String>,
) -> Result<ComfyAnalysis, String> {
    let base = normalize_base(endpoint.as_deref().unwrap_or(""));
    let wf = parse_workflow(&workflow)?;
    let output = output.filter(|s| !s.trim().is_empty()).or_else(|| detect_output(&wf));
    let (bindings, mut notes) = match bindings {
        Some(b) => (b, Vec::new()),
        None => match output.as_deref() {
            Some(o) => detect_bindings(&wf, o),
            None => (Vec::new(), Vec::new()),
        },
    };
    let info = std::thread::scope(|s| s.spawn(|| object_info(&base)).join().map_err(|_| "異常終了".to_string()))?;
    let (errors, warns) = match info {
        Ok(info) => validate(&wf, &bindings, output.as_deref(), &info),
        Err(e) => (Vec::new(), vec![format!("ComfyUI に接続できないため、ノード・モデルの検証は省略しました（{e}）")]),
    };
    notes.extend(warns);
    let mut nodes: Vec<(String, String, String)> = wf
        .as_object()
        .unwrap()
        .iter()
        .map(|(id, n)| {
            (
                id.clone(),
                n["class_type"].as_str().unwrap_or("").to_string(),
                n["_meta"]["title"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    nodes.sort_by_key(|n| id_order_key(&n.0));
    Ok(ComfyAnalysis { workflow: wf, bindings, output, nodes, errors, notes })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Qwen-Image 2.1 t2i テンプレートを API 形式にしたもの（実機 /history から採取・長文は短縮）。
    fn qwen_t2i() -> Value {
        serde_json::from_str(r#"{
          "13": {"class_type": "ResolutionSelector", "inputs": {"aspect_ratio": "1:1 (Square)", "megapixels": 1.0, "multiple": 8}},
          "461": {"class_type": "SaveImageAdvanced", "inputs": {"filename_prefix": "Qwen_image_2.1", "format": "png", "images": ["459:457", 0]}},
          "459:451": {"class_type": "UNETLoader", "inputs": {"unet_name": "qwen_image_2.1_int8_convrot.safetensors", "weight_dtype": "default"}},
          "459:452": {"class_type": "TextEncodeQwenImage21", "inputs": {"prompt": ["459:472", 0], "negative_prompt": "", "resolution": 1024, "clip": ["459:453", 0]}},
          "459:453": {"class_type": "CLIPLoader", "inputs": {"clip_name": "qwen3vl_8b_int8_convrot.safetensors", "type": "qwen_image", "device": "default"}},
          "459:454": {"class_type": "VAELoader", "inputs": {"vae_name": "qwen_image_2.1_vae_bf16.safetensors"}},
          "459:456": {"class_type": "EmptyLatentImage", "inputs": {"width": ["13", 0], "height": ["13", 1], "batch_size": 1}},
          "459:457": {"class_type": "VAEDecode", "inputs": {"samples": ["459:458", 0], "vae": ["459:454", 0]}},
          "459:458": {"class_type": "KSampler", "inputs": {"seed": 447606998181262, "steps": 25, "cfg": 1.0, "sampler_name": "euler", "scheduler": "simple", "denoise": 1.0, "model": ["459:480", 0], "positive": ["459:452", 0], "negative": ["459:452", 1], "latent_image": ["459:456", 0]}},
          "459:471": {"class_type": "TextGenerate", "inputs": {"prompt": "enhancer input", "sampling_mode.seed": 1, "clip": ["459:473", 0], "system_prompt": ["459:475", 0]}},
          "459:472": {"class_type": "PreviewAny", "inputs": {"source": ["459:474", 0]}},
          "459:473": {"class_type": "CLIPLoader", "inputs": {"clip_name": "pe.safetensors", "type": "stable_diffusion", "device": "default"}},
          "459:474": {"class_type": "ComfySwitchNode", "inputs": {"switch": false, "on_false": "Greyscale fashion editorial portrait", "on_true": ["459:471", 0]}},
          "459:475": {"class_type": "PrimitiveStringMultiline", "inputs": {"value": "system prompt"}},
          "459:480": {"class_type": "QwenImage21Cache", "inputs": {"device": "auto", "dtype": "default", "model": ["459:451", 0]}}
        }"#).unwrap()
    }

    /// 画像編集テンプレート（image_1 だけ接続・実機で通った形）。
    fn qwen_edit() -> Value {
        serde_json::from_str(r#"{
          "461": {"class_type": "SaveImageAdvanced", "inputs": {"filename_prefix": "Qwen_image_2.1", "images": ["459:457", 0]}},
          "470": {"class_type": "LoadImage", "inputs": {"image": "gothic_cathedral_00003.png"}},
          "472": {"class_type": "ImageCompare", "inputs": {"image_a": ["470", 0], "image_b": ["461", 0]}},
          "459:456": {"class_type": "EmptyLatentImage", "inputs": {"width": 1024, "height": 1024, "batch_size": 1}},
          "459:457": {"class_type": "VAEDecode", "inputs": {"samples": ["459:458", 0], "vae": ["459:454", 0]}},
          "459:454": {"class_type": "VAELoader", "inputs": {"vae_name": "v.safetensors"}},
          "459:458": {"class_type": "KSampler", "inputs": {"seed": 9, "steps": 25, "cfg": 1.0, "positive": ["459:474", 0], "negative": ["459:474", 1], "latent_image": ["459:468", 0]}},
          "459:468": {"class_type": "ComfySwitchNode", "inputs": {"switch": false, "on_false": ["459:474", 2], "on_true": ["459:456", 0]}},
          "459:474": {"class_type": "TextEncodeQwenImage21", "inputs": {"prompt": ["459:480", 0], "negative_prompt": "", "resolution": 0, "images.image_1": ["470", 0], "vae": ["459:454", 0]}},
          "459:480": {"class_type": "PreviewAny", "inputs": {"source": ["459:484", 0]}},
          "459:484": {"class_type": "ComfySwitchNode", "inputs": {"switch": false, "on_false": "Turn the character into a figure", "on_true": ["459:500", 0]}},
          "459:485": {"class_type": "BatchImagesNode", "inputs": {"images.image0": ["470", 0]}},
          "459:500": {"class_type": "TextGenerate", "inputs": {"prompt": "x", "image": ["459:485", 0]}}
        }"#).unwrap()
    }

    fn find<'a>(b: &'a [ComfyBinding], slot: &str) -> Option<&'a ComfyBinding> {
        b.iter().find(|x| x.slot == slot)
    }

    #[test]
    fn rejects_ui_format_and_unwraps_prompt_body() {
        let e = parse_workflow(r#"{"nodes": [], "links": [], "version": 0.4}"#).unwrap_err();
        assert!(e.contains("エクスポート（API）"), "{e}");
        let v = parse_workflow(r#"{"prompt": {"1": {"class_type": "X", "inputs": {}}}, "client_id": "c"}"#).unwrap();
        assert!(v.get("1").is_some());
    }

    #[test]
    fn detects_qwen_t2i_bindings_through_switch_and_preview() {
        let wf = qwen_t2i();
        assert_eq!(detect_output(&wf).as_deref(), Some("461"));
        let (b, notes) = detect_bindings(&wf, "461");
        // 正プロンプトは PreviewAny → Switch(false) の on_false リテラルまで辿る。
        assert_eq!(find(&b, "prompt").map(|x| (x.node.as_str(), x.input.as_str())), Some(("459:474", "on_false")));
        assert_eq!(find(&b, "negative").map(|x| x.input.as_str()), Some("negative_prompt"));
        assert_eq!(find(&b, "seed").map(|x| x.node.as_str()), Some("459:458"));
        assert_eq!(find(&b, "steps").map(|x| x.input.as_str()), Some("steps"));
        assert_eq!(find(&b, "guidance").map(|x| x.input.as_str()), Some("cfg"));
        // サイズは ResolutionSelector からのリンク＝リテラルでないので口を作らない。
        assert!(find(&b, "width").is_none());
        assert!(notes.iter().any(|n| n.contains("サイズ")));
        // プロンプト強化（TextGenerate）は分岐の使われない側＝口にしない。
        assert!(!b.iter().any(|x| x.node == "459:471"));
        assert!(find(&b, "image").is_none());
    }

    #[test]
    fn detects_edit_image_slot_and_skips_inactive_latent() {
        let wf = qwen_edit();
        let (b, _) = detect_bindings(&wf, "461");
        let img = find(&b, "image").unwrap();
        assert_eq!((img.node.as_str(), img.index), ("470", Some(1)));
        assert_eq!(find(&b, "prompt").map(|x| x.node.as_str()), Some("459:484"));
        // EmptyLatentImage は Switch の使われない側＝サイズの口にしない。
        assert!(find(&b, "width").is_none());
    }

    #[test]
    fn build_prompt_inserts_values_and_drops_unused_images() {
        let wf = qwen_edit();
        let mut b = detect_bindings(&wf, "461").0;
        let v = SlotValues { prompt: "make it a figure".into(), seed: 42, images: vec!["texos/abc.png".into()], ..Default::default() };
        let (out, notes) = build_prompt(&wf, &b, &v).unwrap();
        assert_eq!(out["459:484"]["inputs"]["on_false"], "make it a figure");
        assert_eq!(out["459:458"]["inputs"]["seed"], 42);
        assert_eq!(out["470"]["inputs"]["image"], "texos/abc.png");
        assert!(notes.is_empty());
        // 未指定の数値は差し込まない＝ワークフローの値のまま。
        assert_eq!(out["459:458"]["inputs"]["steps"], 25);

        // 2 枚目の口を足して 1 枚だけ渡す → 2 枚目の LoadImage は外れ、参照キーも消える。
        let mut wf2 = wf.clone();
        wf2["471"] = json!({"class_type": "LoadImage", "inputs": {"image": "b.png"}});
        wf2["459:474"]["inputs"]["images.image_2"] = json!(["471", 0]);
        b.push(ComfyBinding { slot: "image".into(), node: "471".into(), input: "image".into(), index: Some(2) });
        let (out, _) = build_prompt(&wf2, &b, &v).unwrap();
        assert!(out.get("471").is_none());
        assert!(out["459:474"]["inputs"].get("images.image_2").is_none());
        assert!(out["459:474"]["inputs"].get("images.image_1").is_some());

        // 画像口があるのに画像ゼロはエラー。
        let none = SlotValues { prompt: "x".into(), ..Default::default() };
        assert!(build_prompt(&wf, &b, &none).unwrap_err().contains("画像カード"));
    }

    #[test]
    fn validate_reports_missing_nodes_inputs_and_models() {
        let wf = qwen_t2i();
        let (b, _) = detect_bindings(&wf, "461");
        let mut info = json!({});
        for (_, n) in wf.as_object().unwrap() {
            info[n["class_type"].as_str().unwrap()] = json!({"input": {"required": {}}});
        }
        info["KSampler"]["input"]["required"] = json!({"seed": ["INT", {}], "steps": ["INT", {}], "cfg": ["FLOAT", {}]});
        info["ComfySwitchNode"]["input"]["required"] = json!({"on_false": ["*", {}]});
        info["TextEncodeQwenImage21"]["input"]["required"] = json!({"negative_prompt": ["STRING", {}]});
        info["UNETLoader"]["input"]["required"] = json!({"unet_name": ["COMBO", {"options": ["other.safetensors"]}]});
        info.as_object_mut().unwrap().remove("QwenImage21Cache");
        let (errs, _) = validate(&wf, &b, Some("461"), &info);
        assert!(errs.iter().any(|e| e.contains("QwenImage21Cache")), "{errs:?}");
        assert!(errs.iter().any(|e| e.contains("qwen_image_2.1_int8_convrot")), "{errs:?}");
        assert_eq!(errs.len(), 2, "{errs:?}");
        // 入力名が変わった口は検出される。
        let bad = vec![ComfyBinding { slot: "steps".into(), node: "459:458".into(), input: "num_steps".into(), index: None }];
        let (errs, _) = validate(&wf, &bad, Some("461"), &info);
        assert!(errs.iter().any(|e| e.contains("num_steps")), "{errs:?}");
    }

    #[test]
    fn local_detection() {
        assert!(is_local("http://127.0.0.1:8188"));
        assert!(is_local("http://localhost:8188/"));
        assert!(is_local("http://[::1]:8188"));
        assert!(!is_local("http://192.168.1.5:8188"));
    }

    /// 実機（ComfyUI 起動中・Qwen-Image 2.1 導入済み）。`cargo test comfy_live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn comfy_live_qwen_t2i() {
        let wf = qwen_t2i_full();
        let (b, _) = detect_bindings(&wf, "461");
        let spec = crate::flow::parse_spec(&json!({
            "builtin": "t2i", "backend": "comfyui", "comfy_workflow": wf,
            "comfy_bindings": b, "comfy_output": "461", "steps": 8
        }).to_string()).unwrap();
        let cancel = AtomicBool::new(false);
        let r = run(&spec, "a small red teacup on a wooden table, studio photo", &[], &cancel).unwrap();
        assert!(r.bytes.starts_with(b"\x89PNG"));
        let out = std::env::temp_dir().join("texos_comfy_live.png");
        std::fs::write(&out, &r.bytes).unwrap();
        eprintln!("seed={} bytes={} -> {}", r.seed, r.bytes.len(), out.display());
    }

    /// 実機テスト用: 環境変数 TEXOS_COMFY_WF（API 形式 JSON のパス）があればそれ、無ければ短縮版。
    fn qwen_t2i_full() -> Value {
        match std::env::var("TEXOS_COMFY_WF") {
            Ok(p) => parse_workflow(&std::fs::read_to_string(p).unwrap()).unwrap(),
            Err(_) => qwen_t2i(),
        }
    }
}
