use actix_web::{web, App, HttpServer};

use crate::api::AppState;
use crate::error::CacheError;

pub async fn serve(state: AppState, bind: String) -> Result<(), CacheError> {
    let data = web::Data::new(state);
    HttpServer::new(move || {
        App::new()
            .app_data(data.clone())
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
