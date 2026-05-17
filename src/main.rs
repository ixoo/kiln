use kiln::{
    build_app, config::validate_runtime_secrets, execution::launcher_from_settings,
    RealGitHubClient, RuntimeConfig, Settings,
};
use std::{env, net::SocketAddr, sync::Arc};
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if let Ok(env_file) = env::var("KILN_ENV_FILE") {
        dotenvy::from_path(env_file)?;
    } else {
        dotenvy::dotenv().ok();
    }
    init_tracing();

    let config_path = env::var("KILN_CONFIG").unwrap_or_else(|_| "config/kiln.toml".to_string());
    let mut settings = Settings::load(config_path)?;
    let bind_address = settings.server.bind_address.parse::<SocketAddr>()?;
    let webhook_secret = env::var("KILN_GITHUB_WEBHOOK_SECRET")?;
    let agent_callback_secret = env::var("KILN_AGENT_CALLBACK_SECRET").ok();
    let state_secret = env::var("KILN_STATE_SECRET")?;
    let previous_state_secrets = env::var("KILN_PREVIOUS_STATE_SECRETS")
        .ok()
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|secret| !secret.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    settings.execution.callback_secret = agent_callback_secret.clone();
    validate_runtime_secrets(
        &webhook_secret,
        &state_secret,
        agent_callback_secret.as_deref(),
    )?;
    let app_id = env::var("KILN_GITHUB_APP_ID")?.parse::<u64>()?;
    let private_key_path = env::var("KILN_GITHUB_PRIVATE_KEY_PATH")?;

    let github = Arc::new(RealGitHubClient::new(app_id, private_key_path)?);
    let launcher = launcher_from_settings(&settings.execution)?;
    let app = build_app(
        RuntimeConfig {
            settings,
            webhook_secret,
            agent_callback_secret,
            state_secret,
            previous_state_secrets,
        },
        github,
        launcher,
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
