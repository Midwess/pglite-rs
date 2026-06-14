mod cache;
mod cdc;
mod classify;
mod di;
mod diff;
mod error;
mod http;
mod live;
mod rows;
mod setup;
mod version;

#[cfg(test)]
mod tests;

pub use di::{Di, ServerConfig, UpstreamConfig};
pub use error::CacheError;

pub async fn run(config: ServerConfig) -> Result<(), CacheError> {
    Di::init(config).await?;
    http::server::serve().await
}

pub async fn init(upstream: UpstreamConfig) -> Result<(), CacheError> {
    setup::prepare(&upstream).await
}
