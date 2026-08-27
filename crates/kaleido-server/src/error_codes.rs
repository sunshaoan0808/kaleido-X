//! P1-4 S1: unified machine-readable error envelope helpers.
//!
//! Goal: every non-2xx JSON body carries `code` (stable, SCREAMING_SNAKE)
//! alongside the legacy `error` (human text). Existing `map_core_err` already
//! does this for CoreError paths (P0-1/W11); these helpers extend the same
//! contract to handlers that build raw `json!({"error": ...})` responses.
//!
//! Contract:
//!   { "error": "<human msg>", "code": "<STABLE_CODE>", ...details }
//!   - 400-family: caller-supplied domain code, else BAD_REQUEST
//!   - 404: NOT_FOUND unless a domain code given
//!   - 409: CONFLICT; 429: RATE_LIMITED; 5xx: INTERNAL with code preserved
//!     if supplied (e.g. EMBED_FAIL), else INTERNAL_ERROR

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};

/// Build an error response with mandatory `error` + `code` fields.
/// `details` may be Value::Null (omitted), an object (merged), or anything
/// else (nested under "details").
pub(crate) fn err_with_code(
    status: StatusCode,
    code: &str,
    msg: impl Into<String>,
    details: Value,
) -> Response {
    let mut body = json!({ "error": msg.into(), "code": code });
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

/// Convenience wrappers matching the common status families.
pub(crate) fn bad_request(code: &str, msg: impl Into<String>) -> Response {
    err_with_code(StatusCode::BAD_REQUEST, code, msg, Value::Null)
}
pub(crate) fn not_found(code: &str, msg: impl Into<String>) -> Response {
    err_with_code(StatusCode::NOT_FOUND, code, msg, Value::Null)
}
pub(crate) fn unauthorized(msg: impl Into<String>) -> Response {
    err_with_code(StatusCode::UNAUTHORIZED, "UNAUTHORIZED", msg, Value::Null)
}
pub(crate) fn forbidden(code: &str, msg: impl Into<String>) -> Response {
    err_with_code(StatusCode::FORBIDDEN, code, msg, Value::Null)
}
pub(crate) fn conflict(code: &str, msg: impl Into<String>) -> Response {
    err_with_code(StatusCode::CONFLICT, code, msg, Value::Null)
}
pub(crate) fn internal(code: &str, msg: impl Into<String>) -> Response {
    err_with_code(
        StatusCode::INTERNAL_SERVER_ERROR,
        code,
        msg,
        Value::Null,
    )
}
/// 503 Service Unavailable — external dependency down (e.g. embed engine).
pub(crate) fn service_unavailable(code: &str, msg: impl Into<String>) -> Response {
    err_with_code(
        StatusCode::SERVICE_UNAVAILABLE,
        code,
        msg,
        Value::Null,
    )
}

/// 429 统一构造（[P7] 生产路径暂未接线；tests 断言状态码映射，保留防漂移）。
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn too_many_requests(code: &str, msg: impl Into<String>) -> Response {
    err_with_code(
        StatusCode::TOO_MANY_REQUESTS,
        code,
        msg,
        Value::Null,
    )
}
/// 422 Unprocessable Entity — semantic validation failure (well-formed body).
pub(crate) fn unprocessable(code: &str, msg: impl Into<String>) -> Response {
    err_with_code(
        StatusCode::UNPROCESSABLE_ENTITY,
        code,
        msg,
        Value::Null,
    )
}
/// 502 Bad Gateway — upstream dependency returned garbage/failed.
pub(crate) fn bad_gateway(code: &str, msg: impl Into<String>) -> Response {
    err_with_code(StatusCode::BAD_GATEWAY, code, msg, Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn body_of(r: Response) -> Value {
        let bytes = axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn envelope_has_error_and_code() {
        let r = bad_request("AUTHOR_BAD_ID", "invalid project id");
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        let b = body_of(r).await;
        assert_eq!(b["code"], "AUTHOR_BAD_ID");
        assert_eq!(b["error"], "invalid project id");
    }

    #[tokio::test]
    async fn details_object_merges_and_scalars_nest() {
        let r = err_with_code(
            StatusCode::NOT_FOUND,
            "BG_NOT_FOUND",
            "job not found",
            json!({"jobId": "j1"}),
        );
        let b = body_of(r).await;
        assert_eq!(b["jobId"], "j1");

        let r = err_with_code(
            StatusCode::INTERNAL_SERVER_ERROR,
            "EMBED_FAIL",
            "boom",
            json!("scalar"),
        );
        let b = body_of(r).await;
        assert_eq!(b["details"], "scalar");
    }

    #[tokio::test]
    async fn status_families() {
        assert_eq!(unauthorized("nope").status(), StatusCode::UNAUTHORIZED);
        let b = body_of(unauthorized("nope")).await;
        assert_eq!(b["code"], "UNAUTHORIZED");
        assert_eq!(conflict("REV_CONFLICT", "x").status(), StatusCode::CONFLICT);
        assert_eq!(
            internal("SERIALIZE", "x").status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            service_unavailable("EMBED_UNAVAILABLE", "x").status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            too_many_requests("ST_SERVER_BUSY", "x").status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            unprocessable("ST_DIFF_PARSE", "x").status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(bad_gateway("UPSTREAM", "x").status(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn null_details_omitted() {
        let b = body_of(not_found("BT_NOT_FOUND", "job not found")).await;
        assert!(b.get("details").is_none());
        assert_eq!(b.as_object().unwrap().len(), 2); // error + code only
    }
}
