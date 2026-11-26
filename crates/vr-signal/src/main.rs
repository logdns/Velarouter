//! Headless entry point for the self-hosted deployment. No capture or GUI dependencies.
use anyhow::{ensure, Context, Result};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let token = std::env::var("VELAROUTER_ADMIN_TOKEN").context("set VELAROUTER_ADMIN_TOKEN")?;
    ensure!(token.len() >= 32, "admin token must be at least 32 bytes");
    vr_signal::serve(vr_signal::SignalConfig {
        public_url: std::env::var("VELAROUTER_PUBLIC_URL").context("set VELAROUTER_PUBLIC_URL")?,
        admin_token: Some(token),
        ..Default::default()
    })
    .await
}
