#[tokio::main]
async fn main() -> anyhow::Result<()> {
  spotify_api::driver().await
}
