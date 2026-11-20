use axum::response::Html;
use axum::routing::get;
use axum::Router;
use qdrant_client::Qdrant;
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeDir;
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

    // Fail fast on the two secrets that must never fall back to a default. Done after migrations
    // so a missing key doesn't block a fresh DB setup, but before we serve any traffic.
    //
    // JWT_SECRET signs and verifies every session token. It used to default to the literal
    // "dev_secret", so a deploy that forgot the var would accept forged tokens for any user.
    // CREDENTIAL_KEY encrypts OAuth tokens / VPN profiles / cloud creds at rest; without it
    // integrations are unusable and we must not silently encrypt with a weak key.
    crate::services::auth::validate_jwt_secret()?;
    crate::services::crypto::validate_key()?;

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

    // Reflection scheduler: periodically self-reviews agents with new activity and files
    // improvement proposals into the review queue. Backgrounded; runs for the process lifetime.
    {
        let db = state.db.clone();
        let inference = state.inference.clone();
        tokio::spawn(async move {
            services::reflections::run_scheduler(db, inference).await;
        });
    }

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    // The bundled SvelteKit frontend, built by `web/` into a plain folder and served
    // from this same binary (one image, one Fly app — the UMS/optimizer pattern). Because
    // the SPA is same-origin with the API, the web client's relative `BASE = '/api/v1'`
    // resolves without a proxy and CORS never enters the picture.
    //
    // WEB_DIR points at the build output (the default suits local dev: run from the repo
    // root after `npm run build` in `web/`). A path the file server can't match gets
    // `200.html` — the SPA shell — with a 200, so client-routed deep links like
    // /studio/jobs/:id resolve instead of 404ing. The shell is read once at startup; if
    // it's absent (a pure-API deploy with no WEB_DIR) we degrade to a plain 404 rather
    // than failing to boot.
    let web_dir = std::env::var("WEB_DIR").unwrap_or_else(|_| "web/build".into());
    let spa_shell: Arc<String> =
        Arc::new(std::fs::read_to_string(format!("{web_dir}/200.html")).unwrap_or_default());
    if spa_shell.is_empty() {
        tracing::warn!("no SPA shell at {web_dir}/200.html — serving API only");
    }
    let static_service = ServeDir::new(&web_dir).fallback(get({
        let spa_shell = spa_shell.clone();
        move || {
            let spa_shell = spa_shell.clone();
            async move {
                if spa_shell.is_empty() {
                    (axum::http::StatusCode::NOT_FOUND, Html(String::new()))
                } else {
                    (axum::http::StatusCode::OK, Html(spa_shell.to_string()))
                }
            }
        }
    }));

    // The API is registered first; `fallback_service` only catches paths the API doesn't,
    // so the JSON endpoints are never shadowed by the file server.
    //
    // The nested API router carries its OWN fallback so an unmatched /api/* path 404s as
    // JSON here instead of bubbling out to the SPA fallback below. Without it a typo'd or
    // retired endpoint would hand the web client an HTML shell with a 200, and its
    // `res.json()` would blow up on a parse error rather than surfacing a clean 404.
    let api = routes::router().fallback(|| async {
        (
            axum::http::StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({ "error": "Not found" })),
        )
    });

    let app = Router::new()
        .nest("/api/v1", api)
        .fallback_service(static_service)
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], 8080));
    tracing::info!("Flaborful API listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
