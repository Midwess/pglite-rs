use actix_web::{web, HttpResponse};
use serde::Deserialize;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_stream::StreamExt;

use crate::di::Di;

#[derive(Deserialize)]
pub struct LiveRequest {
    sql: String,
}

pub async fn live(query: web::Query<LiveRequest>) -> HttpResponse {
    let di = Di::instance();
    if di.replica().is_halted() {
        return HttpResponse::ServiceUnavailable()
            .content_type("application/json")
            .body("{\"name\":\"HaltedError\",\"message\":\"replica halted\"}");
    }

    let cacheable = match di.classifier().classify(&query.sql) {
        Ok(query) => query,
        Err(error) => return super::query::error_response(error),
    };

    let receiver = di.live().subscribe(cacheable.sql, cacheable.tables).await;
    let stream = UnboundedReceiverStream::new(receiver)
        .map(|event| Ok::<_, actix_web::Error>(web::Bytes::from(event)));

    HttpResponse::Ok()
        .insert_header(("Cache-Control", "no-store"))
        .content_type("text/event-stream")
        .streaming(stream)
}
