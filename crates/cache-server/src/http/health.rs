use actix_web::HttpResponse;
use serde_json::json;

use crate::di::Di;

pub async fn healthz() -> HttpResponse {
    let replica = Di::instance().replica();
    if replica.is_halted() {
        HttpResponse::ServiceUnavailable().json(json!({
            "status": "halted",
            "reason": replica.halt_reason(),
        }))
    } else {
        HttpResponse::Ok().json(json!({
            "status": "ok",
            "watermark": replica.watermark().0,
        }))
    }
}
