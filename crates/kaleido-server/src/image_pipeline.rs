//! U10: 图像管线消费模块（bookcover / illustration / lore-image / image-presets）。
//!
//! 在直出端点 `POST /api/v1/kaleido-tools/image`（uniapi cogview-4 / cf-manager flux /
//! grok2api grok-imagine）之上封装"消费"级端点：
//! - `POST /api/v1/kaleido-tools/bookcover`    书封面（BuildPrompt 模板：无文字/无印花/竖版 3:4）
//! - `POST /api/v1/kaleido-tools/illustration` 章节插图（校验章节存在 -> 生成 -> 回写章节点 imagePath）
//! - `POST /api/v1/kaleido-tools/lore-image`   资料(lore)项配图（链接 lore item -> 回写 entry.imagePath）
//! - `GET /POST /api/v1/kaleido-tools/presets` 图像方案预设库（prompt 注入，可选风格/景别；
//!    落盘 `$KALEIDO_DATA/image_presets.json`，原子写仿 st_compass.rs write_atomic）
//!
//! 落盘约定（U10）：所有产物写入 `$KALEIDO_DATA/works/{workspace_id}/images/{kind}/...`
//! （data/ 目录内、同工作区 jail），DB/JSON 只保存工作区相对路径
//! （如 `images/illustrations/{packId}/ch01-abc12345.png`）；前端经既有
//! `GET /api/v1/works/image-data-url?path=` 读取展示，无需新增读端点。
//!
//! 生图失败（上游 502 / b64 解码失败 / URL 下载失败）返回可读 JSON 错误，不阻塞正文生成。

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{fs, io::Write, path::Path};
use uuid::Uuid;

use crate::{map_core_err, session_from, AppState};
use crate::error_codes::*;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/kaleido-tools/bookcover", post(bookcover))
        .route("/api/v1/kaleido-tools/illustration", post(illustration))
        .route("/api/v1/kaleido-tools/lore-image", post(lore_image))
        .route(
            "/api/v1/kaleido-tools/presets",
            get(list_presets).post(upsert_preset),
        )
}

// ---------------------------------------------------------------------------
// 请求体（新增字段全部 serde default，兼容旧调用方）
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BookcoverBody {
    title: String,
    #[serde(default)]
    subtitle: Option<String>,
    /// 风格/方案：命中 image-presets 名称则注入其 prompt，否则按自由文本风格处理。
    #[serde(default)]
    style: Option<String>,
    #[serde(default)]
    channel: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IllustrationBody {
    /// Pack id（故事馆/档案馆的 workId）
    work_id: String,
    /// 章节 id（ch01）或章节号（1）
    chapter_id: String,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    style: Option<String>,
    /// 景别：特写/近景/中景/远景
    #[serde(default)]
    shot: Option<String>,
    #[serde(default)]
    channel: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoreImageBody {
    #[serde(default)]
    work_id: Option<String>,
    item_id: String,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    style: Option<String>,
    #[serde(default)]
    channel: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PresetBody {
    #[serde(default)]
    id: Option<String>,
    name: String,
    kind: String,
    prompt: String,
    #[serde(default)]
    style: Option<String>,
    #[serde(default)]
    shot: Option<String>,
}

// ---------------------------------------------------------------------------
// Prompt 构建（BuildPrompt 模板）
// ---------------------------------------------------------------------------

/// U10: 书封面 BuildPrompt——竖版 3:4，无文字、无印花、无标题排版。
fn bookcover_prompt(title: &str, subtitle: Option<&str>, style: Option<&str>) -> String {
    let subject = match subtitle {
        Some(s) if !s.trim().is_empty() => format!("《{title}》——{s}"),
        _ => format!("《{title}》"),
    };
    let style = style
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("史诗奇幻氛围，电影级光影");
    format!(
        "书籍封面插画，竖版 3:4 构图，高品质数字绘画。\n主题：{subject}。\n风格：{style}。\n\
         画面中严禁出现任何文字、字母、数字、符号、标题、标语、作者名、出版社标志或水印印章；\
         不要有任何文字排版、边框文字或印花装饰；画面干净，聚焦主题意境。"
    )
}

/// U10: 章节插图 BuildPrompt——横版叙事画面，注入章节标题/情节片段/风格/景别。
fn illustration_prompt(
    chapter_title: &str,
    body_snippet: &str,
    user_prompt: Option<&str>,
    style: Option<&str>,
    shot: Option<&str>,
) -> String {
    let mut p = format!(
        "小说章节插图，电影感叙事画面，高品质插画。\n章节：{}。\n情节片段：{}",
        chapter_title,
        truncate(body_snippet, 220)
    );
    if let Some(s) = style.map(str::trim).filter(|s| !s.is_empty()) {
        p.push_str(&format!("\n风格：{s}。"));
    }
    if let Some(s) = shot.map(str::trim).filter(|s| !s.is_empty()) {
        p.push_str(&format!("\n景别：{s}。"));
    }
    if let Some(u) = user_prompt.map(str::trim).filter(|s| !s.is_empty()) {
        p.push_str(&format!("\n补充要求：{u}。"));
    }
    p.push_str("\n画面中不要出现文字、水印或字幕；主体清晰，构图有张力。");
    p
}

/// U10: 资料(lore)项配图 BuildPrompt——设定概念图。
fn lore_prompt(item_title: &str, item_text: &str, user_prompt: Option<&str>, style: Option<&str>) -> String {
    let mut p = format!(
        "小说设定资料配图，概念艺术风格。\n条目：{}。\n描述：{}",
        item_title,
        truncate(item_text, 220)
    );
    if let Some(s) = style.map(str::trim).filter(|s| !s.is_empty()) {
        p.push_str(&format!("\n风格：{s}。"));
    }
    if let Some(u) = user_prompt.map(str::trim).filter(|s| !s.is_empty()) {
        p.push_str(&format!("\n补充要求：{u}。"));
    }
    p.push_str("\n画面中不要出现文字、水印或字幕；构图完整，细节考究。");
    p
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "…"
    }
}

/// U10: 文件名安全化（保留字母数字与连字符，中英文均可，最长 40）。
fn safe_stem(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_alphanumeric() || c == '-' || c == '_' {
            out.push(c);
        } else if c.is_whitespace() {
            out.push('-');
        }
        if out.chars().count() >= 40 {
            break;
        }
    }
    if out.is_empty() {
        out = "img".into();
    }
    out
}

// ---------------------------------------------------------------------------
// 生成 + 落盘
// ---------------------------------------------------------------------------

/// 生成并落盘一张图到工作区 jail：`images/{kind}/{sub}/{stem}-{uuid8}.{ext}`。
/// 返回 `(工作区相对路径, mime, 字节数)`；失败返回可读 Response（不阻塞正文生成）。
async fn generate_and_save_image(
    state: &AppState,
    workspace_id: &str,
    channel: &str,
    prompt: &str,
    size: Option<&str>,
    aspect_ratio: Option<&str>,
    kind: &str,
    sub: &str,
    stem: &str,
) -> Result<(String, String, usize), Response> {
    let fetch = crate::kaleido_tools::fetch_image(state, channel, prompt, size, aspect_ratio)
        .await
        .map_err(|e| {
            return err_with_code(
            StatusCode::BAD_GATEWAY,
            "IMG_ERROR", e,
            serde_json::json!({"channel": channel}),
            );
        })?;
    let bytes = match (fetch.b64, fetch.url) {
        (Some(b64), _) => B64.decode(b64.as_bytes()).map_err(|e| {
            return bad_gateway("IMG_ERROR", format!("上游 b64 解码失败：{e}"));
        })?,
        (None, Some(url)) => download_bytes(&url).await.map_err(|e| {
            return bad_gateway("IMG_ERROR", e);
        })?,
        (None, None) => {
            return Err(bad_gateway("IMG_ERROR", "上游未返回图片数据"));
        }
    };
    if bytes.is_empty() {
        return Err(bad_gateway("IMG_EMPTY", "上游返回空图片"));
    }
    let mime = sniff_mime(&bytes);
    let ext = match mime {
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        _ => "png",
    };
    let id = Uuid::new_v4().simple().to_string();
    let rel_dir = format!("images/{kind}/{}", safe_stem(sub));
    let rel = format!("{rel_dir}/{}-{}.{}", safe_stem(stem), &id[..8], ext);
    let abs = state
        .works
        .resolve(workspace_id, &rel, true)
        .map_err(map_core_err)?;
    write_atomic_bytes(&abs, &bytes)?;
    let size = bytes.len();
    Ok((rel, mime.to_string(), size))
}

/// U10: 原子写文件（仿 st_compass.rs write_atomic：tmp + rename + sync）。
fn write_atomic_bytes(path: &Path, bytes: &[u8]) -> Result<(), Response> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| map_core_err(kaleido_core::CoreError::Io(e)))?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp).map_err(|e| map_core_err(kaleido_core::CoreError::Io(e)))?;
        f.write_all(bytes).map_err(|e| map_core_err(kaleido_core::CoreError::Io(e)))?;
        f.sync_all().map_err(|e| map_core_err(kaleido_core::CoreError::Io(e)))?;
    }
    fs::rename(&tmp, path).map_err(|e| map_core_err(kaleido_core::CoreError::Io(e)))?;
    Ok(())
}

/// U10: 下载上游 URL 图片字节（no_proxy；grok2api 先试 :8020 再退回 :8000）。
async fn download_bytes(url: &str) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::builder().no_proxy().build().unwrap_or_default();
    let mut candidates = vec![url.to_string()];
    let alt = url.replace(":8020", ":8000");
    if alt != url {
        candidates.push(alt);
    }
    let mut last_err = String::from("下载失败");
    for u in candidates {
        match client.get(&u).send().await {
            Ok(r) if r.status().is_success() => {
                let bytes = r.bytes().await.map_err(|e| format!("读取响应失败：{e}"))?;
                if bytes.is_empty() {
                    last_err = format!("{u} 返回空内容");
                    continue;
                }
                return Ok(bytes.to_vec());
            }
            Ok(r) => last_err = format!("{u} HTTP {}", r.status()),
            Err(e) => last_err = format!("{u} 请求失败：{e}"),
        }
    }
    Err(last_err)
}

/// U10: magic byte 嗅探 mime（png/jpeg/webp，其余按 png 处理）。
fn sniff_mime(bytes: &[u8]) -> &'static str {
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else {
        "image/png"
    }
}

/// U10: 章节查找——先按 id，再按章节号（order）。
fn find_chapter<'a>(
    pack: &'a kaleido_core::StoryPack,
    chapter_ref: &str,
) -> Option<&'a kaleido_core::StoryChapter> {
    if let Ok(n) = chapter_ref.parse::<u32>() {
        if let Some(c) = pack.chapters.iter().find(|c| c.order == n) {
            return Some(c);
        }
    }
    pack.chapters.iter().find(|c| c.id == chapter_ref)
}

fn find_chapter_mut<'a>(
    pack: &'a mut kaleido_core::StoryPack,
    chapter_ref: &str,
) -> Option<&'a mut kaleido_core::StoryChapter> {
    // 单次遍历：优先按 order 匹配，否则按 id 匹配（避免两次 iter_mut 借用冲突）
    if let Ok(n) = chapter_ref.parse::<u32>() {
        if let Some(idx) = pack.chapters.iter().position(|c| c.order == n) {
            return pack.chapters.get_mut(idx);
        }
    }
    if let Some(idx) = pack.chapters.iter().position(|c| c.id == chapter_ref) {
        return pack.chapters.get_mut(idx);
    }
    None
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// U10: 书封面——POST /api/v1/kaleido-tools/bookcover {title, subtitle?, style?, channel?}
async fn bookcover(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<BookcoverBody>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let title = body.title.trim().to_string();
    if title.is_empty() {
        return bad_request("IMG_BAD_REQUEST", "title 必填");
    }
    if title.chars().count() > 60 {
        return bad_request("IMG_BAD_REQUEST", "title 过长（≤60 字符）");
    }
    let channel = body.channel.as_deref().unwrap_or("uniapi");
    let style = resolve_style(&state, body.style.as_deref());
    let prompt = bookcover_prompt(
        &title,
        body.subtitle.as_deref(),
        if style.is_empty() { None } else { Some(&style) },
    );
    // 竖版 3:4：uniapi 用 cogview 支持的 864x1152；grok2api 用 aspect_ratio=3:4；cf-manager 保持默认。
    let size = if channel == "uniapi" { Some("864x1152") } else { None };
    let aspect = if channel == "grok2api" { Some("3:4") } else { None };
    let (rel, mime, bytes) = match generate_and_save_image(
        &state,
        &session.workspace_id,
        channel,
        &prompt,
        size,
        aspect,
        "bookcover",
        &session.workspace_id,
        &title,
    )
    .await
    {
        Ok(x) => x,
        Err(r) => return r,
    };
    Json(json!({
        "ok": true,
        "kind": "bookcover",
        "imagePath": rel,
        "path": rel,
        "channel": channel,
        "mime": mime,
        "size": bytes,
        "title": title,
    }))
    .into_response()
}

/// U10: 章节插图——POST /api/v1/kaleido-tools/illustration
/// {workId, chapterId, prompt?, style?, shot?, channel?}
async fn illustration(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<IllustrationBody>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let work_id = body.work_id.trim().to_string();
    if work_id.is_empty() {
        return bad_request("IMG_BAD_REQUEST", "workId 必填（Pack id）");
    }
    let chapter_ref = body.chapter_id.trim().to_string();
    if chapter_ref.is_empty() {
        return bad_request("IMG_BAD_REQUEST", "chapterId 必填（章节 id 或章节号，如 ch01 / 1）");
    }
    // 校验 Pack 与章节存在
    let pack = match state.packs.get(&work_id) {
        Ok(p) => p,
        Err(e) => return map_core_err(e),
    };
    let chapter = match find_chapter(&pack, &chapter_ref) {
        Some(c) => c.clone(),
        None => {
            return not_found("IMG_NOT_FOUND", format!("章节 '{chapter_ref}' 不存在于 Pack {work_id}"));
        }
    };
    let body_snippet = if chapter.body_path.trim().is_empty() {
        String::new()
    } else {
        state
            .packs
            .read_chapter_body(&work_id, &chapter.body_path)
            .unwrap_or_default()
    };
    let channel = body.channel.as_deref().unwrap_or("uniapi");
    let style = resolve_style(&state, body.style.as_deref());
    let prompt = illustration_prompt(
        &chapter.title,
        &body_snippet,
        body.prompt.as_deref(),
        if style.is_empty() { None } else { Some(&style) },
        body.shot.as_deref(),
    );
    // 横版 4:3：uniapi 用 1152x864；grok2api 用 aspect_ratio=4:3。
    let size = if channel == "uniapi" { Some("1152x864") } else { None };
    let aspect = if channel == "grok2api" { Some("4:3") } else { None };
    let (rel, mime, bytes) = match generate_and_save_image(
        &state,
        &session.workspace_id,
        channel,
        &prompt,
        size,
        aspect,
        "illustrations",
        &work_id,
        &chapter.id,
    )
    .await
    {
        Ok(x) => x,
        Err(r) => return r,
    };
    // 回写章节点 imagePath（重新加载 pack，避免基于过期副本覆盖）
    let mut pack = match state.packs.get(&work_id) {
        Ok(p) => p,
        Err(e) => return map_core_err(e),
    };
    let Some(ch) = find_chapter_mut(&mut pack, &chapter_ref) else {
        return not_found("IMG_NOT_FOUND", format!("章节 '{chapter_ref}' 不存在于 Pack {work_id}"));
    };
    ch.image_path = rel.clone();
    if let Err(e) = state.packs.save(pack) {
        return map_core_err(e);
    }
    Json(json!({
        "ok": true,
        "kind": "illustration",
        "workId": work_id,
        "chapterId": chapter.id,
        "chapterOrder": chapter.order,
        "imagePath": rel,
        "path": rel,
        "channel": channel,
        "mime": mime,
        "size": bytes,
    }))
    .into_response()
}

/// U10: 资料(lore)项配图——POST /api/v1/kaleido-tools/lore-image
/// {workId, itemId, prompt?, style?, channel?}
async fn lore_image(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<LoreImageBody>,
) -> Response {
    let session = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let item_id = body.item_id.trim().to_string();
    if item_id.is_empty() {
        return bad_request("IMG_BAD_REQUEST", "itemId 必填");
    }
    let work_id = body
        .work_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    let work_id = match work_id {
        Some(w) => w,
        None => {
            return bad_request("IMG_BAD_REQUEST", "workId 必填（lore item 所属 Pack id）");
        }
    };
    let pack = match state.packs.get(&work_id) {
        Ok(p) => p,
        Err(e) => return map_core_err(e),
    };
    let entry = pack
        .lore_entries
        .iter()
        .find(|e| e.get("id").and_then(|v| v.as_str()) == Some(item_id.as_str()))
        .cloned();
    let entry = match entry {
        Some(e) => e,
        None => {
            return not_found("IMG_NOT_FOUND", format!("lore item '{item_id}' 不存在于 Pack {work_id}"));
        }
    };
    let entry_title = entry
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("资料条目")
        .to_string();
    let entry_text = entry
        .get("text")
        .or_else(|| entry.get("content"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let channel = body.channel.as_deref().unwrap_or("uniapi");
    let style = resolve_style(&state, body.style.as_deref());
    let prompt = lore_prompt(
        &entry_title,
        &entry_text,
        body.prompt.as_deref(),
        if style.is_empty() { None } else { Some(&style) },
    );
    let size = if channel == "uniapi" { Some("1152x864") } else { None };
    let aspect = if channel == "grok2api" { Some("4:3") } else { None };
    let (rel, mime, bytes) = match generate_and_save_image(
        &state,
        &session.workspace_id,
        channel,
        &prompt,
        size,
        aspect,
        "lore",
        &work_id,
        &item_id,
    )
    .await
    {
        Ok(x) => x,
        Err(r) => return r,
    };
    // 回写 lore entry.imagePath（重新加载 pack 再定位）
    let mut pack = match state.packs.get(&work_id) {
        Ok(p) => p,
        Err(e) => return map_core_err(e),
    };
    let mut found = false;
    for e in pack.lore_entries.iter_mut() {
        if e.get("id").and_then(|v| v.as_str()) == Some(item_id.as_str()) {
            if let Some(obj) = e.as_object_mut() {
                obj.insert("imagePath".into(), json!(rel));
                found = true;
            }
            break;
        }
    }
    if !found {
        return not_found("IMG_NOT_FOUND", format!("lore item '{item_id}' 不存在于 Pack {work_id}"));
    }
    if let Err(e) = state.packs.save(pack) {
        return map_core_err(e);
    }
    Json(json!({
        "ok": true,
        "kind": "loreimage",
        "workId": work_id,
        "itemId": item_id,
        "imagePath": rel,
        "path": rel,
        "channel": channel,
        "mime": mime,
        "size": bytes,
    }))
    .into_response()
}

// ---------------------------------------------------------------------------
// 图像方案预设库（data/image_presets.json，仿 CompassStore 落盘模式）
// ---------------------------------------------------------------------------

const PRESETS_FILE: &str = "image_presets.json";

/// U10: 内置图像方案（首启种子；与用户自定义预设并存，均以 name 命中注入）。
fn builtin_presets() -> Value {
    json!([
        { "id": "bp-cover-epic",   "name": "史诗奇幻", "kind": "bookcover",     "prompt": "史诗奇幻书封，宏大场景纵深，电影级光影，神秘氛围", "style": "史诗奇幻", "builtin": true },
        { "id": "bp-cover-ink",    "name": "国风水墨", "kind": "bookcover",     "prompt": "国风水墨画风，留白构图，宣纸质感，意境悠远", "style": "国风水墨", "builtin": true },
        { "id": "bp-cover-wuxia",  "name": "武侠水墨", "kind": "bookcover",     "prompt": "武侠题材，水墨泼墨，劲风衣袂，山巅剑客剪影，苍劲有力", "style": "武侠水墨", "builtin": true },
        { "id": "bp-cover-neon",   "name": "赛博霓虹", "kind": "bookcover",     "prompt": "赛博朋克霓虹都市，青品红对比光，雨夜街景，未来感", "style": "赛博霓虹", "builtin": true },
        { "id": "bp-cover-fantasy","name": "唯美幻想", "kind": "bookcover",     "prompt": "唯美幻想风格，柔光，梦幻色彩，细腻笔触，治愈氛围", "style": "唯美幻想", "builtin": true },
        { "id": "bp-cover-bw",     "name": "黑白素描", "kind": "bookcover",     "prompt": "黑白铅笔素描质感，高对比，细腻排线，艺术感", "style": "黑白素描", "builtin": true },
        { "id": "bp-cover-scifi",  "name": "硬核科幻", "kind": "bookcover",     "prompt": "硬核科幻，巨型星舰与空间站，冷色高光，精密机械质感", "style": "硬核科幻", "builtin": true },
        { "id": "bp-illu-close",   "name": "特写",     "kind": "illustration",  "prompt": "特写镜头，细节丰富，浅景深，情绪饱满", "style": "写实", "shot": "特写", "builtin": true },
        { "id": "bp-illu-mid",     "name": "中景",     "kind": "illustration",  "prompt": "中景镜头，人物与环境平衡，叙事清晰", "style": "写实", "shot": "中景", "builtin": true },
        { "id": "bp-illu-wide",    "name": "远景",     "kind": "illustration",  "prompt": "远景大场景，广阔空间感，环境叙事，气势恢宏", "style": "写实", "shot": "远景", "builtin": true },
        { "id": "bp-illu-water",   "name": "水彩",     "kind": "illustration",  "prompt": "水彩插画，通透晕染，柔和自然，文艺质感", "style": "水彩", "builtin": true },
        { "id": "bp-lore-west",    "name": "西幻设定", "kind": "loreimage",     "prompt": "西方奇幻设定插图，史诗氛围，细节考究，概念艺术质感", "style": "西幻设定", "builtin": true },
        { "id": "bp-lore-east",    "name": "东方仙侠", "kind": "loreimage",     "prompt": "东方仙侠设定插图，云雾山峦，灵气流转，古风意境", "style": "东方仙侠", "builtin": true },
    ])
}

fn presets_path(state: &AppState) -> std::path::PathBuf {
    state.auth.data_root().root().join(PRESETS_FILE)
}

/// 读预设库；文件不存在返回内置种子（不落盘，POST 时才写）。
fn load_presets(state: &AppState) -> Result<Value, Response> {
    let path = presets_path(state);
    if !path.exists() {
        return Ok(builtin_presets());
    }
    let raw = fs::read_to_string(&path).map_err(|e| map_core_err(kaleido_core::CoreError::Io(e)))?;
    match serde_json::from_str::<Value>(&raw) {
        Ok(v) if v.is_array() => Ok(v),
        Ok(_) => Ok(json!([])),
        Err(e) => Err(internal("IMG_INTERNAL", format!("presets 解析失败：{e}"))),
    }
}

/// U10: 风格/方案解析——命中预设（name 或 id 不区分大小写）则注入其 prompt；
/// 否则按自由文本风格处理（返回原样）。
fn resolve_style(state: &AppState, style: Option<&str>) -> String {
    let Some(style) = style else { return String::new() };
    let s = style.trim();
    if s.is_empty() {
        return String::new();
    }
    let presets = match load_presets(state) {
        Ok(v) => v,
        Err(_) => return s.to_string(),
    };
    for p in presets.as_array().into_iter().flatten() {
        let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let id = p.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if name.eq_ignore_ascii_case(s) || id.eq_ignore_ascii_case(s) {
            if let Some(prompt) = p.get("prompt").and_then(|v| v.as_str()) {
                if !prompt.trim().is_empty() {
                    return prompt.trim().to_string();
                }
            }
        }
    }
    s.to_string()
}

/// U10: GET /api/v1/kaleido-tools/presets —— 返回全部预设。
async fn list_presets(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    match load_presets(&state) {
        Ok(v) => Json(v).into_response(),
        Err(r) => r,
    }
}

/// U10: POST /api/v1/kaleido-tools/presets —— 按 id 或同名 upsert，原子落盘。
async fn upsert_preset(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PresetBody>,
) -> Response {
    if let Err(r) = session_from(&state, &headers) {
        return r;
    }
    let name = body.name.trim().to_string();
    let kind = body.kind.trim().to_string();
    let prompt = body.prompt.trim().to_string();
    if name.is_empty() || kind.is_empty() || prompt.is_empty() {
        return bad_request("IMG_BAD_REQUEST", "name / kind / prompt 均必填");
    }
    if !matches!(kind.as_str(), "bookcover" | "illustration" | "loreimage" | "general") {
        return bad_request("IMG_BAD_REQUEST", "kind 须为 bookcover|illustration|loreimage|general");
    }
    let mut presets = match load_presets(&state) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let arr = match presets.as_array_mut() {
        Some(a) => a,
        None => {
            return internal("IMG_INTERNAL", "presets 存储损坏");
        }
    };
    let target_id = body
        .id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    let idx = arr.iter().position(|p| {
        let pid = p.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let pname = p.get("name").and_then(|v| v.as_str()).unwrap_or("");
        (target_id.as_deref().map(|t| t == pid).unwrap_or(false)) || pname == name
    });
    let new_id = target_id.unwrap_or_else(|| {
        format!("ip-{}", &Uuid::new_v4().simple().to_string()[..8])
    });
    let mut entry = json!({
        "id": new_id,
        "name": name,
        "kind": kind,
        "prompt": prompt,
        "builtin": false,
    });
    if let Some(o) = entry.as_object_mut() {
        if let Some(st) = body.style.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            o.insert("style".into(), json!(st));
        }
        if let Some(sh) = body.shot.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            o.insert("shot".into(), json!(sh));
        }
    }
    match idx {
        Some(i) => arr[i] = entry,
        None => arr.push(entry),
    }
    // 原子落盘（仿 st_compass.rs write_atomic）——本函数返回 Response，用显式 match 替代 `?`
    let path = presets_path(&state);
    if let Some(parent) = path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            return map_core_err(kaleido_core::CoreError::Io(e));
        }
    }
    let pretty = match serde_json::to_string_pretty(&presets) {
        Ok(p) => p,
        Err(e) => {
            return internal("IMG_INTERNAL", e.to_string())
        }
    };
    let tmp = path.with_extension("tmp");
    {
        let mut f = match fs::File::create(&tmp) {
            Ok(f) => f,
            Err(e) => return map_core_err(kaleido_core::CoreError::Io(e)),
        };
        if let Err(e) = f.write_all(pretty.as_bytes()) {
            return map_core_err(kaleido_core::CoreError::Io(e));
        }
        if let Err(e) = f.sync_all() {
            return map_core_err(kaleido_core::CoreError::Io(e));
        }
    }
    if let Err(e) = fs::rename(&tmp, &path) {
        return map_core_err(kaleido_core::CoreError::Io(e));
    }
    Json(json!({ "ok": true, "presets": presets })).into_response()
}
