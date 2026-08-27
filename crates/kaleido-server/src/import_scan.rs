//! POST /api/v1/import/scan — import-safety preview endpoint (P7).
//! Accepts base64 payloads typed `text` | `image` | `docx` and reports
//! threats / structural errors without persisting anything.

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde::Deserialize;
use serde_json::json;

use crate::encoding_sniff::decode_text;
use crate::AppState;

#[derive(Deserialize)]
pub struct ScanRequest {
    #[serde(rename = "type")]
    kind: String,
    data: String, // base64
}

pub fn router() -> Router<AppState> {
    Router::new().route("/api/v1/import/scan", post(scan))
}

fn bad(msg: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"ok": false, "error": msg})),
    )
        .into_response()
}

async fn scan(State(_state): State<AppState>, Json(body): Json<ScanRequest>) -> Response {
    let raw = match B64.decode(&body.data) {
        Ok(b) => b,
        Err(_) => return bad("invalid base64"),
    };
    let mut threats: Vec<serde_json::Value> = Vec::new();
    let mut error: Option<String> = None;
    match body.kind.as_str() {
        "text" => {
            // 编码嗅探：集成 ReadAware decodeTextBook 逻辑（encoding_sniff::decode_text），
            // BOM → UTF-8/GB18030/Big5/Shift_JIS/EUC-KR 严格解码 + mojibake 检测，
            // 避免 GBK 源 txt 被 from_utf8_lossy 强解成 � 乱码（2026-08-06 智取美母案例）。
            let (text, _enc) = decode_text(&raw);
            for t in kaleido_core::inspect_imported_plain_text(&text) {
                threats.push(serde_json::to_value(t).unwrap_or_else(|_| json!(null)));
            }
        }
        "image" => {
            if let Err(e) = kaleido_core::read_raster_image_metadata(&raw) {
                error = Some(e.to_string());
            }
        }
        "docx" => {
            if let Err(e) = kaleido_core::validate_docx(&raw) {
                error = Some(e);
            }
        }
        _ => return bad("unknown type (expected text|image|docx)"),
    }
    let safe = threats.is_empty() && error.is_none();
    Json(json!({"ok": true, "safe": safe, "threats": threats, "error": error})).into_response()
}
