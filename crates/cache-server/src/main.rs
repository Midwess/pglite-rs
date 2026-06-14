use cache_server::ServerConfig;

#[actix_web::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    cache_server::run(ServerConfig::from_env()?).await?;
    Ok(())
}
