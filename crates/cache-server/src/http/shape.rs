use std::time::Duration;

use actix_web::{web, HttpResponse};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::di::Di;
use crate::rows;

#[derive(Deserialize)]
pub struct ShapeQuery {
    offset: Option<i64>,
    live: Option<bool>,
}

pub async fn shape(path: web::Path<String>, query: web::Query<ShapeQuery>) -> HttpResponse {
    let table = path.into_inner();
    let di = Di::instance();
    if !di.tables().contains(&table) {
        return HttpResponse::NotFound()
            .content_type("application/json")
            .body("{\"name\":\"NotFound\",\"message\":\"unknown shape\"}");
    }
    if di.replica().is_halted() {
        return HttpResponse::ServiceUnavailable().json(json!({
            "status": "halted",
            "reason": di.replica().halt_reason(),
        }));
    }

    match query.offset {
        None | Some(..=-1) => snapshot(&table).await,
        Some(offset) => tail(&table, offset as u64, query.live.unwrap_or(false)).await,
    }
}

async fn snapshot(table: &str) -> HttpResponse {
    let di = Di::instance();
    let offset = di.replica().watermark().0;
    match rows::query_json(di.db(), &format!("select * from \"{table}\"")).await {
        Ok(data) => {
            let snapshot: Value = serde_json::from_str(&data).unwrap_or_else(|_| json!([]));
            HttpResponse::Ok()
                .insert_header(("Cache-Control", "no-store"))
                .content_type("application/json")
                .body(
                    json!({
                        "offset": offset,
                        "snapshot": snapshot,
                        "up_to_date": true,
                    })
                    .to_string(),
                )
        }
        Err(error) => super::query::error_response(error),
    }
}

async fn tail(table: &str, after: u64, live: bool) -> HttpResponse {
    let shapes = Di::instance().shapes();
    let mut range = shapes.range(table, after);
    if live && range.changes.is_empty() && !range.must_refetch {
        let _ = tokio::time::timeout(Duration::from_secs(20), shapes.wait_for_change()).await;
        range = shapes.range(table, after);
    }

    if range.must_refetch {
        return HttpResponse::Conflict()
            .content_type("application/json")
            .body(
            "{\"name\":\"MustRefetch\",\"message\":\"offset evicted; request a fresh snapshot\"}",
        );
    }

    let changes: Vec<Value> = range
        .changes
        .iter()
        .map(|entry| {
            json!({
                "offset": entry.offset,
                "op": entry.op,
                "schema": entry.schema,
                "table": entry.table,
                "row": entry.row,
            })
        })
        .collect();

    HttpResponse::Ok()
        .insert_header(("Cache-Control", "no-store"))
        .content_type("application/json")
        .body(
            json!({
                "offset": range.latest_offset,
                "up_to_date": changes.is_empty(),
                "changes": changes,
            })
            .to_string(),
        )
}
