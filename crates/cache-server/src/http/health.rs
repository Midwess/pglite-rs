use actix_web::{web, HttpResponse};
use serde_json::json;

use crate::api::AppState;

pub async fn healthz(state: web::Data<AppState>) -> HttpResponse {
    if state.replica.is_halted() {
        HttpResponse::ServiceUnavailable().json(json!({
            "status": "halted",
            "reason": state.replica.halt_reason(),
        }))
    } else {
        HttpResponse::Ok().json(json!({
            "status": "ok",
            "watermark": state.replica.watermark().0,
        }))
    }
}
