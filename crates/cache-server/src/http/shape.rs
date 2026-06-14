use std::time::Duration;

use actix_web::{web, HttpResponse};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::api::AppState;
use crate::rows;

#[derive(Deserialize)]
pub struct ShapeQuery {
    offset: Option<i64>,
    live: Option<bool>,
}

pub async fn shape(
    state: web::Data<AppState>,
    path: web::Path<String>,
    query: web::Query<ShapeQuery>,
) -> HttpResponse {
    let table = path.into_inner();
    if !state.tables.contains(&table) {
        return HttpResponse::NotFound()
            .content_type("application/json")
            .body("{\"name\":\"NotFound\",\"message\":\"unknown shape\"}");
    }
    if state.replica.is_halted() {
        return HttpResponse::ServiceUnavailable().json(json!({
            "status": "halted",
            "reason": state.replica.halt_reason(),
        }));
    }

    match query.offset {
        None | Some(..=-1) => snapshot(&state, &table).await,
        Some(offset) => tail(&state, &table, offset as u64, query.live.unwrap_or(false)).await,
    }
}

async fn snapshot(state: &AppState, table: &str) -> HttpResponse {
    let offset = state.replica.watermark().0;
    match rows::query_json(&state.db, &format!("select * from \"{table}\"")).await {
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

async fn tail(state: &AppState, table: &str, after: u64, live: bool) -> HttpResponse {
    let mut range = state.shapes.range(table, after);
    if live && range.changes.is_empty() && !range.must_refetch {
        let _ = tokio::time::timeout(Duration::from_secs(20), state.shapes.wait_for_change()).await;
        range = state.shapes.range(table, after);
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
