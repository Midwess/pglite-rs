use std::sync::Arc;

use actix_web::http::StatusCode;
use actix_web::{web, HttpResponse};
use serde::Deserialize;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_stream::StreamExt;

use crate::cache::CachedResult;
use crate::classify::CacheableQuery;
use crate::di::Di;
use crate::error::CacheError;
use crate::rows;

#[derive(Deserialize)]
pub struct QueryParams {
    live: Option<bool>,
}

#[derive(Deserialize)]
pub struct QueryBody {
    sql: String,
}

pub async fn query(params: web::Query<QueryParams>, body: web::Json<QueryBody>) -> HttpResponse {
    let sql = body.sql.as_str();
    if params.live.unwrap_or(false) {
        return live_query(sql).await;
    }
    match materialize(sql).await {
        Ok((_, hash, version, _)) => HttpResponse::SeeOther()
            .insert_header(("Location", format!("/q/{hash}/{version}")))
            .insert_header(("Cache-Control", "no-store"))
            .finish(),
        Err(error) => error_response(error),
    }
}

async fn live_query(sql: &str) -> HttpResponse {
    let (query, hash, version, snapshot) = match materialize(sql).await {
        Ok(parts) => parts,
        Err(error) => return error_response(error),
    };
    let receiver =
        Di::instance()
            .live()
            .subscribe(query.sql, query.tables, hash, version, &snapshot.body);
    let stream = UnboundedReceiverStream::new(receiver)
        .map(|event| Ok::<_, actix_web::Error>(web::Bytes::from(event)));
    HttpResponse::Ok()
        .insert_header(("Cache-Control", "no-store"))
        .content_type("text/event-stream")
        .streaming(stream)
}

pub async fn cursor(path: web::Path<(String, String)>) -> HttpResponse {
    let (hash, version) = path.into_inner();
    let key = format!("{hash}:{version}");
    match Di::instance().cache().get(&key).await {
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

async fn materialize(
    sql: &str,
) -> Result<(CacheableQuery, String, u64, Arc<CachedResult>), CacheError> {
    let di = Di::instance();
    if di.replica().is_halted() {
        return Err(CacheError::Halted(
            di.replica()
                .halt_reason()
                .unwrap_or_else(|| "unknown".to_string()),
        ));
    }

    let query = di.classifier().classify(sql)?;
    let hash = format!("{:x}", query.fingerprint);
    let version = di.versions().version_of(&query.tables, &query.eq_filters).0;
    let key = format!("{hash}:{version}");
    let snapshot_sql = query.sql.clone();
    let snapshot = di
        .cache()
        .get_or_compute(key, async move {
            rows::query_json(di.db(), &snapshot_sql).await
        })
        .await?;
    Ok((query, hash, version, snapshot))
}

pub(crate) fn error_response(error: CacheError) -> HttpResponse {
    let code = match &error {
        CacheError::Rejected(_) | CacheError::Parse(_) => StatusCode::BAD_REQUEST,
        CacheError::Halted(_) => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    HttpResponse::build(code)
        .content_type("application/json")
        .body(error.envelope())
}
