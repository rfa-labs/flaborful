//! Serves the node manifests that drive the workflow builder's palette + node/field rendering.
//! Public metadata (no per-user data), so it's unauthenticated — the builder fetches it once on load.

use crate::services::node_registry;
use crate::AppState;
use axum::{routing::get, Json, Router};

pub fn router() -> Router<AppState> {
    Router::new().route("/", get(list_manifests))
}

async fn list_manifests() -> Json<Vec<node_registry::NodeManifest>> {
    Json(node_registry::manifests())
}
