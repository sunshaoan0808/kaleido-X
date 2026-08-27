//! SillyTavern export (S5-W2 T8).
//! POST /api/v1/partner/st-export — character card / world book → ST JSON (implemented).
//! L-4: PNG tEXt binary-card packaging is NOT implemented — only the ST JSON payload is
//! produced. Clients receive a JSON card; no .png wrapper is emitted. This is documented
//! as a future enhancement, not a missing success path.

use axum::{
    extract::State,
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{session_from, AppState};
use crate::error_codes::*;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StExportBody {
    #[serde(default)]
    pub character_card_id: Option<String>,
    #[serde(default)]
    pub world_book_id: Option<String>,
    /// world_book | character_card | auto
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub fields: Option<Value>,
}

pub fn router() -> Router<AppState> {
    Router::new().route("/api/v1/partner/st-export", post(st_export))
}

async fn st_export(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<StExportBody>,
) -> Response {
    let sess = match session_from(&state, &headers) {
        Ok(s) => s,
        Err(r) => return r,
    };
    // C2 审计修复：per-user 隔离。
    let partner = match state.partner.clone().scoped(&sess.user_id).load() {
        Ok(p) => p,
        Err(e) => {
            return internal("STX_INTERNAL", e.to_string());
        }
    };

    let kind = body
        .kind
        .as_deref()
        .unwrap_or("auto")
        .to_ascii_lowercase();

    // Prefer explicit id
    if let Some(id) = body.character_card_id.as_ref() {
        if let Some(cc) = partner.character_cards.iter().find(|c| &c.id == id) {
            return Json(export_character(cc)).into_response();
        }
        return not_found("STX_NOT_FOUND", format!("character card not found: {id}"));
    }
    if let Some(id) = body.world_book_id.as_ref() {
        if let Some(wb) = partner.world_books.iter().find(|w| &w.id == id) {
            return Json(export_world_book(wb)).into_response();
        }
        return not_found("STX_NOT_FOUND", format!("world book not found: {id}"));
    }

    // Inline payload
    if let Some(name) = body.name.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let fields = body.fields.clone().unwrap_or(json!({}));
        let content = body.content.clone().unwrap_or_default();
        if kind == "world_book" || kind == "worldbook" {
            let item = kaleido_core::PartnerItem {
                id: "inline".into(),
                name: name.to_string(),
                item_type: "world_book".into(),
                content,
                fields: Some(fields),
                world_book_id: None,
            };
            return Json(export_world_book(&item)).into_response();
        }
        let item = kaleido_core::PartnerItem {
            id: "inline".into(),
            name: name.to_string(),
            item_type: "character_card".into(),
            content,
            fields: Some(fields),
            world_book_id: None,
        };
        return Json(export_character(&item)).into_response();
    }

    // Selected partner item
    if let Some(id) = partner.selected_character_card_id.as_ref() {
        if let Some(cc) = partner.character_cards.iter().find(|c| &c.id == id) {
            return Json(export_character(cc)).into_response();
        }
    }
    if let Some(id) = partner.selected_world_book_id.as_ref() {
        if let Some(wb) = partner.world_books.iter().find(|w| &w.id == id) {
            return Json(export_world_book(wb)).into_response();
        }
    }
    if let Some(cc) = partner.character_cards.first() {
        return Json(export_character(cc)).into_response();
    }
    if let Some(wb) = partner.world_books.first() {
        return Json(export_world_book(wb)).into_response();
    }

    return bad_request("STX_BAD_REQUEST", "no partner item to export; provide characterCardId or create one");
}

fn field_str(fields: &Option<Value>, key: &str) -> String {
    fields
        .as_ref()
        .and_then(|f| f.get(key))
        .and_then(|v| {
            if let Some(s) = v.as_str() {
                Some(s.to_string())
            } else if let Some(a) = v.as_array() {
                Some(
                    a.iter()
                        .filter_map(|x| x.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                )
            } else {
                v.as_i64().map(|n| n.to_string())
            }
        })
        .unwrap_or_default()
}

fn export_character(cc: &kaleido_core::PartnerItem) -> Value {
    let description = if !cc.content.trim().is_empty() {
        cc.content.clone()
    } else {
        field_str(&cc.fields, "description")
    };
    let personality = field_str(&cc.fields, "personality");
    let scenario = field_str(&cc.fields, "scenario");
    let first_mes = field_str(&cc.fields, "firstMes");
    let first_mes = if first_mes.is_empty() {
        field_str(&cc.fields, "first_mes")
    } else {
        first_mes
    };
    let mes_example = field_str(&cc.fields, "mesExample");
    let system_prompt = field_str(&cc.fields, "systemPrompt");
    let creator_notes = field_str(&cc.fields, "creatorNotes");
    let tags = cc
        .fields
        .as_ref()
        .and_then(|f| f.get("tags"))
        .cloned()
        .unwrap_or(json!([]));

    let data = json!({
        "name": cc.name,
        "description": description,
        "personality": personality,
        "scenario": scenario,
        "first_mes": first_mes,
        "mes_example": mes_example,
        "creator_notes": creator_notes,
        "system_prompt": system_prompt,
        "post_history_instructions": field_str(&cc.fields, "postHistoryInstructions"),
        "tags": tags,
        "creator": "kaleido-server",
        "character_version": "1.0",
        "alternate_greetings": [],
        "extensions": {
            "kaleido": {
                "id": cc.id,
                "worldBookId": cc.world_book_id,
                "sourceFields": cc.fields,
            }
        }
    });

    // Full card envelope (what ST reads from PNG tEXt `chara`)
    let card = json!({
        "spec": "chara_card_v2",
        "spec_version": "2.0",
        "data": data,
    });

    let mut resp = json!({
        "ok": true,
        "format": "sillytavern_character_card_v2",
        "pngTextTodo": false,
        "spec": "chara_card_v2",
        "spec_version": "2.0",
        "data": data,
        // convenience full card
        "card": card.clone(),
        "source": {
            "id": cc.id,
            "type": "character_card",
            "name": cc.name,
        }
    });

    // W2: embed card into PNG tEXt `chara` so clients can download a .png card
    match kaleido_core::embed_st_card_in_png(&card, None) {
        Ok(png) => {
            resp["pngBase64"] = json!(kaleido_core::png_to_base64(&png));
        }
        Err(e) => {
            resp["pngError"] = json!(format!("{e}"));
        }
    }
    resp
}

fn export_world_book(wb: &kaleido_core::PartnerItem) -> Value {
    let content = if !wb.content.trim().is_empty() {
        wb.content.clone()
    } else {
        field_str(&wb.fields, "content")
    };
    let entries = json!([{
        "uid": 0,
        "key": [wb.name.clone()],
        "keysecondary": [],
        "comment": wb.name,
        "content": content,
        "constant": true,
        "selective": false,
        "order": 100,
        "position": 0,
        "disable": false,
        "probability": 100,
    }]);
    json!({
        "ok": true,
        "format": "sillytavern_world_info",
        "pngTextTodo": true,
        "name": wb.name,
        "entries": entries,
        "source": {
            "id": wb.id,
            "type": "world_book",
            "name": wb.name,
            "fields": wb.fields,
        }
    })
}

