use kiln::{build_app, RealGitHubClient, RuntimeConfig, Settings};
use std::{env, net::SocketAddr, sync::Arc};
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if let Ok(env_file) = env::var("KILN_ENV_FILE") {
        dotenvy::from_path(env_file).ok();
    } else {
        dotenvy::dotenv().ok();
    }
    init_tracing();

    let config_path = env::var("KILN_CONFIG").unwrap_or_else(|_| "config/kiln.toml".to_string());
    let settings = Settings::load(config_path)?;
    let bind_address = settings.server.bind_address.parse::<SocketAddr>()?;
    let webhook_secret = env::var("KILN_GITHUB_WEBHOOK_SECRET")?;
    let app_id = env::var("KILN_GITHUB_APP_ID")?.parse::<u64>()?;
    let private_key_path = env::var("KILN_GITHUB_PRIVATE_KEY_PATH")?;

    let github = Arc::new(RealGitHubClient::new(app_id, private_key_path)?);
    let app = build_app(
        RuntimeConfig {
            settings,
            webhook_secret,
        },
        github,
    );

    let listener = tokio::net::TcpListener::bind(bind_address).await?;
    info!(%bind_address, "kiln listening");
    axum::serve(listener, app).await?;

    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("kiln=info,tower_http=info"));

    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .init();
}
