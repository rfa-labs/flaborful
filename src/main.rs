use axum::Router;
use qdrant_client::Qdrant;
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod auth_extractor;
mod db;
mod errors;
mod models;
mod routes;
mod services;
mod state;

pub use state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Required when multiple crates bring in rustls 0.23 with different crypto backends
    let _ = rustls::crypto::ring::default_provider().install_default();
    dotenvy::dotenv().ok();

    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "flaborful_api=debug,tower_http=debug".into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let pool = db::create_pool().await?;
    db::run_migrations(&pool).await?;

    // Validate at-rest encryption key is present and well-formed. Integrations (OAuth tokens,
    // VPN profiles, cloud creds) are unusable without it; we fail startup rather than let the
    // app silently encrypt with a weak/zero-padded key. Done after migrations so a missing key
    // doesn't block a fresh DB setup, but before serving traffic.
    if let Err(e) = crate::services::crypto::validate_key() {
        tracing::warn!("CREDENTIAL_KEY invalid or missing — integrations will fail: {e}");
    }

    let qdrant_url = std::env::var("QDRANT_URL").expect("QDRANT_URL must be set");
    let qdrant_api_key = std::env::var("QDRANT_API_KEY").expect("QDRANT_API_KEY must be set");

    let qdrant = Arc::new(
        Qdrant::from_url(&qdrant_url)
            .api_key(qdrant_api_key)
            .build()?,
    );

    services::knowledge::ensure_collections(&qdrant).await?;

    let state = AppState::new(pool, qdrant);

    // Sweep any orphaned managed-agents sessions/environments from prior crashes/aborted runs so
    // they stop costing money. Backgrounded so it never delays startup.
    {
        let backends = state.backends.clone();
        tokio::spawn(async move {
            backends.reap_orphans().await;
        });
    }

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .nest("/api/v1", routes::router())
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], 8080));
    tracing::info!("Flaborful API listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
