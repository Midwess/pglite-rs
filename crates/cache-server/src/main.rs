use cache_server::{CacheServer, ServerConfig};

#[actix_web::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ServerConfig::from_env()?;
    let server = CacheServer::boot(config).await?;
    server.run().await?;
    Ok(())
}
