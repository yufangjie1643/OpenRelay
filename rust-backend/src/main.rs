#[tokio::main]
async fn main() -> Result<(), openrelay::server::ServerError> {
    openrelay::server::run_from_env().await
}
