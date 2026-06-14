use actix_web::http::StatusCode;
use actix_web::{web, HttpResponse};

use crate::api::AppState;
use crate::classify::Plan;
use crate::error::CacheError;
use crate::rows;

pub async fn query(state: web::Data<AppState>, body: String) -> HttpResponse {
    match run_query(state.get_ref(), &body).await {
        Ok((etag, json)) => {
            let mut builder = HttpResponse::Ok();
            builder.content_type("application/json");
            if etag.is_empty() {
                builder.insert_header(("Cache-Control", "no-store"));
            } else {
                builder.insert_header(("ETag", etag));
                builder.insert_header(("Cache-Control", "public, max-age=0, must-revalidate"));
            }
            builder.body(json)
        }
        Err(error) => error_response(error),
    }
}

pub async fn cursor(state: web::Data<AppState>, path: web::Path<(String, String)>) -> HttpResponse {
    let (hash, version) = path.into_inner();
    let key = format!("{hash}:{version}");
    match state.cache.get(&key).await {
        Some(result) => HttpResponse::Ok()
            .insert_header(("ETag", result.etag.clone()))
            .insert_header(("Cache-Control", "public, max-age=31536000, immutable"))
            .content_type("application/json")
            .body(result.body.clone()),
        None => HttpResponse::NotFound()
            .content_type("application/json")
            .body("{\"name\":\"NotFound\",\"message\":\"unknown cursor\"}"),
    }
}

async fn run_query(state: &AppState, sql: &str) -> Result<(String, String), CacheError> {
    if state.replica.is_halted() {
        return Err(CacheError::Halted(
            state
                .replica
                .halt_reason()
                .unwrap_or_else(|| "unknown".to_string()),
        ));
    }

    match state.classifier.classify(sql)? {
        Plan::Forward { sql } => {
            let json = state.upstream.forward(&sql).await?;
            Ok((String::new(), json))
        }
        Plan::PassThrough { sql } => {
            let json = rows::query_json(&state.db, &sql).await?;
            Ok((String::new(), json))
        }
        Plan::Cacheable {
            fingerprint,
            tables,
            eq_filters,
            sql,
        } => {
            let version = state.versions.version_of(&tables, &eq_filters);
            let key = format!("{fingerprint:x}:{}", version.0);
            let db = state.db.clone();
            let result = state
                .cache
                .get_or_compute(key, async move { rows::query_json(&db, &sql).await })
                .await?;
            Ok((result.etag.clone(), result.body.clone()))
        }
    }
}

fn error_response(error: CacheError) -> HttpResponse {
    let code = match &error {
        CacheError::Rejected(_) | CacheError::Parse(_) => StatusCode::BAD_REQUEST,
        CacheError::Halted(_) => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    HttpResponse::build(code)
        .content_type("application/json")
        .body(error.envelope())
}
