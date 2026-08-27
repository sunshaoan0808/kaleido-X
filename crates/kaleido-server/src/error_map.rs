//! CoreError -> HTTP mapping (P0-1)
use axum::{http::StatusCode, response::{IntoResponse, Response}, Json};
use kaleido_core::CoreError;
use serde_json::json;

pub(crate) fn map_core_err(e: CoreError) -> Response {
    match e {
        CoreError::Auth(msg) => (StatusCode::UNAUTHORIZED, Json(json!({"error": msg}))).into_response(),
        CoreError::RateLimited(msg) => {
            (StatusCode::TOO_MANY_REQUESTS, Json(json!({"error": msg}))).into_response()
        }
        CoreError::SessionCap {
            message,
            active,
            cap,
            policy,
        } => (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({
                "error": message,
                "code": "SESSION_CAP",
                "active": active,
                "cap": cap,
                "policy": policy,
                "hint": "GET /api/v1/sessions/stats; POST /api/v1/sessions/prune; PATCH /api/v1/settings sessionMax/sessionCapPolicy",
                "actions": [
                    {"method": "GET", "path": "/api/v1/sessions/stats", "desc": "inspect active/cap/policy"},
                    {"method": "POST", "path": "/api/v1/sessions/prune", "body": {"mode": "oldest", "count": 5}, "desc": "evict oldest auth sessions"},
                    {"method": "POST", "path": "/api/v1/sessions/prune", "body": {"mode": "expired"}, "desc": "drop expired only"},
                    {"method": "PATCH", "path": "/api/v1/settings", "body": {"sessionMax": 80, "sessionCapPolicy": "auto_evict"}, "desc": "raise cap / enable auto_evict"},
                    {"method": "POST", "path": "/api/v1/auth/logout", "desc": "free current token if held"}
                ]
            })),
        )
            .into_response(),
        CoreError::NotFound(msg) => {
            crate::error_codes::not_found("NOT_FOUND", msg)
        }
        CoreError::Forbidden(msg) => {
            crate::error_codes::forbidden("FORBIDDEN", msg)
        }
        CoreError::BadRequest(msg) => {
            crate::error_codes::bad_request("BAD_REQUEST", msg)
        }
        // Revision CAS conflict: concurrent write lost (PackStore / TavernSessionStore).
        CoreError::Conflict(msg) => {
            crate::error_codes::conflict("CONFLICT", msg)
        }
        // W11+: stable machine-readable codes (Works + future domains)
        CoreError::Coded {
            code,
            message,
            details,
        } => {
            let status = match code.as_str() {
                "WORKS_NOT_FOUND" => StatusCode::NOT_FOUND,
                "WORKS_PATH_ESCAPE" | "WORKS_PATH_TRAVERSAL" => StatusCode::FORBIDDEN,
                "WORKS_FILE_TOO_LARGE"
                | "WORKS_CONTENT_TOO_LARGE"
                | "WORKS_APPEND_TOO_LARGE"
                | "WORKS_PARENT_MISSING"
                | "WORKS_BINARY_REJECTED"
                | "WORKS_NOT_UTF8"
                | "WORKS_NOT_FILE"
                | "WORKS_IS_DIR"
                | "WORKS_ABSOLUTE_PATH"
                | "WORKS_DIR_NOT_EMPTY"
                | "WORKS_ROOT_FORBIDDEN"
                | "WORKS_LIST_NOT_DIR"
                | "WORKS_BINARY_CONTENT"
                | "WORKS_INVALID_PATH"
                | "WORKS_INVALID_WORKSPACE" => StatusCode::BAD_REQUEST,
                _ if code.starts_with("WORKS_") => StatusCode::BAD_REQUEST,
                _ => StatusCode::BAD_REQUEST,
            };
            let mut body = json!({
                "error": message,
                "code": code,
            });
            if let Some(obj) = body.as_object_mut() {
                if let Some(d) = details.as_object() {
                    for (k, v) in d {
                        obj.insert(k.clone(), v.clone());
                    }
                } else if !details.is_null() {
                    obj.insert("details".into(), details);
                }
            }
            (status, Json(body)).into_response()
        }
        other => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": other.to_string()})),
        )
            .into_response(),
    }
}
