use actix_web::http::StatusCode;
use actix_web::{web, HttpResponse};

use crate::di::Di;
use crate::error::CacheError;
use crate::rows;

pub async fn query(body: String) -> HttpResponse {
    match run_query(&body).await {
        Ok((etag, json)) => HttpResponse::Ok()
            .insert_header(("ETag", etag))
            .insert_header(("Cache-Control", "public, max-age=0, must-revalidate"))
            .content_type("application/json")
            .body(json),
        Err(error) => error_response(error),
    }
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

async fn run_query(sql: &str) -> Result<(String, String), CacheError> {
    let di = Di::instance();
    if di.replica().is_halted() {
        return Err(CacheError::Halted(
            di.replica()
                .halt_reason()
                .unwrap_or_else(|| "unknown".to_string()),
        ));
    }

    let query = di.classifier().classify(sql)?;
    let version = di.versions().version_of(&query.tables, &query.eq_filters);
    let key = format!("{:x}:{}", query.fingerprint, version.0);
    let sql = query.sql;
    let result = di
        .cache()
        .get_or_compute(key, async move { rows::query_json(di.db(), &sql).await })
        .await?;
    Ok((result.etag.clone(), result.body.clone()))
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
