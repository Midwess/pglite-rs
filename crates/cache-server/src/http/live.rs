use actix_web::{web, HttpResponse};
use serde::Deserialize;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_stream::StreamExt;

use crate::api::AppState;
use crate::classify::Plan;

#[derive(Deserialize)]
pub struct LiveRequest {
    sql: String,
}

pub async fn live(state: web::Data<AppState>, query: web::Query<LiveRequest>) -> HttpResponse {
    if state.replica.is_halted() {
        return HttpResponse::ServiceUnavailable()
            .content_type("application/json")
            .body("{\"name\":\"HaltedError\",\"message\":\"replica halted\"}");
    }

    let (tables, sql) = match state.classifier.classify(&query.sql) {
        Ok(Plan::Cacheable { tables, sql, .. }) => (tables, sql),
        Ok(_) => {
            return HttpResponse::BadRequest().content_type("application/json").body(
                "{\"name\":\"RejectedError\",\"message\":\"live supports cacheable read queries only\"}",
            )
        }
        Err(error) => return super::query::error_response(error),
    };

    let receiver = state.live.subscribe(sql, tables).await;
    let stream = UnboundedReceiverStream::new(receiver)
        .map(|event| Ok::<_, actix_web::Error>(web::Bytes::from(event)));

    HttpResponse::Ok()
        .insert_header(("Cache-Control", "no-store"))
        .content_type("text/event-stream")
        .streaming(stream)
}
