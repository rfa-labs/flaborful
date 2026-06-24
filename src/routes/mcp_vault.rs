//! MCP server for the professional's **knowledge vault** — the agent's browsable memory.
//!
//! Browsability lives in this tool layer over the `vault_notes` store (rows aren't browsable;
//! these tools are). Coding agents — both the onboarding code-ingestion run (which *writes* the
//! vault from real source) and the task builder (which *reads* it mid-work) — reach it here,
//! registered in their MCP config next to `principal` (the taste tool).
//!
//! Transport: MCP Streamable HTTP — stateless JSON-RPC over POST at `/mcp/vault/:token`. Same
//! minimal method set as `mcp_taste`. Auth is the per-task token in the path, resolved via
//! `taste::resolve_agent` to the owning `agent_id`; every tool is scoped to that agent's vault.
//!
//! Tools:
//!   - `upsert_note`   — create/overwrite a note (the write-back path: ingestion notes, MEMORY)
//!   - `list_vault`    — list notes (optionally filtered by layer/domain) → the tree/MOC view
//!   - `read_note`     — read one whole note by slug (no chunk fragmentation)
//!   - `search_vault`  — hybrid (semantic ⊕ keyword) + one related-link hop, metadata-filtered
//!   - `follow_links`  — the `related:` neighbors of a note (graph traversal)

use axum::http::StatusCode;
use axum::{
    extract::{Path, State},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};

use crate::services::execution::taste;
use crate::services::vault;
use crate::AppState;

const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

pub fn router() -> Router<AppState> {
    Router::new().route("/vault/:token", post(mcp_handler))
}

async fn mcp_handler(
    State(state): State<AppState>,
    Path(token): Path<String>,
    Json(req): Json<Value>,
) -> impl IntoResponse {
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let id = req.get("id").cloned();

    match method {
        "initialize" => json_rpc_result(
            id,
            json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "vault", "version": env!("CARGO_PKG_VERSION") },
                "instructions": "This is your knowledge vault — your durable memory of how you work. \
                                 Use search_vault and read_note to ground your work in your own past \
                                 patterns, style rules, and golden snippets; follow_links to pull in \
                                 connected notes; and upsert_note to write back what you learn."
            }),
        ),

        "notifications/initialized" | "initialized" => StatusCode::ACCEPTED.into_response(),

        "tools/list" => json_rpc_result(id, json!({ "tools": tool_specs() })),

        "tools/call" => {
            let tool = req
                .pointer("/params/name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let args = req
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or(json!({}));

            let agent_id = match taste::resolve_agent(&state.db, &token).await {
                Ok(a) => a,
                Err(e) => {
                    return tool_result(id, &format!("Vault auth failed: {e}"), true)
                        .into_response()
                }
            };

            let outcome = vault::handle_tool(
                &state.db,
                &state.qdrant,
                &state.inference,
                agent_id,
                tool,
                &args,
            )
            .await;
            match outcome {
                Ok(text) => tool_result(id, &text, false).into_response(),
                Err(e) => tool_result(id, &format!("Vault tool error: {e}"), true).into_response(),
            }
        }

        other => json_rpc_error(id, -32601, &format!("Method not found: {other}")).into_response(),
    }
}

fn tool_specs() -> Value {
    json!([
        {
            "name": "upsert_note",
            "description": "Create or overwrite a vault note (your durable memory). Use layer \
                            'feature' / 'pattern' / 'tech' / 'golden_snippet' / 'architecture' / \
                            'style_rule' / 'note'. Link related notes via their slugs in 'related' \
                            (the [[wikilinks]] that build your knowledge graph).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "slug": { "type": "string", "description": "Stable kebab-case note id (unique in your vault)." },
                    "title": { "type": "string" },
                    "body": { "type": "string", "description": "Markdown body. Use [[other-slug]] inline for links." },
                    "layer": { "type": "string", "enum": ["feature","pattern","tech","golden_snippet","architecture","style_rule","note"] },
                    "domain": { "type": "string", "enum": ["backend","frontend","infra","testing","product","general"] },
                    "related": { "type": "array", "items": { "type": "string" }, "description": "Slugs of related notes." },
                    "repo": { "type": "string", "description": "Source repo, for code-derived notes." }
                },
                "required": ["slug", "body"]
            }
        },
        {
            "name": "list_vault",
            "description": "List your vault notes (titles + slugs + layer), optionally filtered by \
                            layer or domain. Your map of content — start here to see what you know.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "layer": { "type": "string" },
                    "domain": { "type": "string" }
                }
            }
        },
        {
            "name": "read_note",
            "description": "Read one whole note by slug (the full markdown, frontmatter included).",
            "inputSchema": {
                "type": "object",
                "properties": { "slug": { "type": "string" } },
                "required": ["slug"]
            }
        },
        {
            "name": "search_vault",
            "description": "Hybrid semantic + keyword search across your vault (with one related-link \
                            hop), optionally filtered by layer/domain. Returns whole matching notes.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "layer": { "type": "string" },
                    "domain": { "type": "string" }
                },
                "required": ["query"]
            }
        },
        {
            "name": "follow_links",
            "description": "Return the notes a note links to via its 'related' slugs (graph traversal).",
            "inputSchema": {
                "type": "object",
                "properties": { "slug": { "type": "string" } },
                "required": ["slug"]
            }
        }
    ])
}

fn json_rpc_result(id: Option<Value>, result: Value) -> axum::response::Response {
    Json(json!({ "jsonrpc": "2.0", "id": id, "result": result })).into_response()
}

fn json_rpc_error(id: Option<Value>, code: i64, message: &str) -> axum::response::Response {
    Json(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }))
        .into_response()
}

fn tool_result(id: Option<Value>, text: &str, is_error: bool) -> axum::response::Response {
    json_rpc_result(
        id,
        json!({
            "content": [{ "type": "text", "text": text }],
            "isError": is_error
        }),
    )
}
