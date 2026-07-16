use crate::AppState;
use axum::{routing::get, Json, Router};
use serde_json::json;

mod agents;
mod applications;
mod audit;
mod auth;
mod billing;
mod companies;
mod company_vault;
mod dashboard;
mod jobs;
mod knowledge;
mod marketplace;
mod mcp_taste;
mod mcp_vault;
mod node_chat;
mod notifications;
mod onboarding;
mod professionals;
mod professions;
mod sse;
mod tasks;
mod workflows;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .nest("/auth", auth::router())
        .nest("/professions", professions::router())
        .nest("/professionals", professionals::router())
        .nest("/agents", agents::router())
        .nest("/companies", companies::router().merge(company_vault::router()))
        .nest("/jobs", jobs::router())
        .nest("/applications", applications::router())
        .nest("/node-chat", node_chat::router())
        .nest("/workflows", workflows::router())
        .nest("/tasks", tasks::router())
        .nest("/onboarding", onboarding::router())
        .nest("/marketplace", marketplace::router())
        .nest("/notifications", notifications::router())
        .nest("/audit", audit::router())
        .nest("/dashboard", dashboard::router())
        .nest("/knowledge", knowledge::router())
        .nest("/billing", billing::router())
        .nest("/events", sse::router())
        // Both MCP servers (taste + vault) live under /mcp; merge so the path nests once.
        .nest("/mcp", mcp_taste::router().merge(mcp_vault::router()))
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok" }))
}
