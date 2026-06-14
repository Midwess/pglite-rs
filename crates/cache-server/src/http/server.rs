use actix_web::{web, App, HttpServer};

use crate::di::Di;
use crate::error::CacheError;

pub async fn serve() -> Result<(), CacheError> {
    let bind = Di::instance().bind_addr().to_string();
    HttpServer::new(|| {
        App::new()
            .route("/healthz", web::get().to(super::health::healthz))
            .route("/query", web::post().to(super::query::query))
            .route("/q/{hash}/{version}", web::get().to(super::query::cursor))
            .route("/shape/{table}", web::get().to(super::shape::shape))
            .route("/live", web::get().to(super::live::live))
    })
    .bind(bind)
    .map_err(CacheError::Io)?
    .run()
    .await
    .map_err(CacheError::Io)?;
    Ok(())
}
