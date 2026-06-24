//! MCP server for the shared "taste tool". Sprite-hosted coding agents (Claude Code, Codex, …)
//! reach the professional's LoRA through this endpoint, registered in their MCP config. The agent
//! calls the single tool `consult_principal(question)` to ask how the professional it represents
//! would implement/approach something — answered in their voice via the persona + LoRA.
//!
//! Transport: MCP Streamable HTTP — stateless JSON-RPC over POST at `/mcp/taste/:token`. We
//! implement the minimal method set Claude Code needs: `initialize`, `notifications/initialized`,
//! `tools/list`, `tools/call`. Auth is the per-task token in the path (the sprite has no
//! Flaborful JWT), which resolves to the agent's persona + cf_finetune_id.
//!
//! The Managed Agents backend calls `taste::consult` directly (its native custom-tool flow); both
//! converge on the same handler.

use axum::http::StatusCode;
use axum::{
    extract::{Path, State},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};

use crate::services::execution::taste;
use crate::AppState;

/// MCP protocol version we advertise. Claude Code negotiates; we echo a supported version.
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

pub fn router() -> Router<AppState> {
    Router::new().route("/taste/:token", post(mcp_handler))
}

/// POST /mcp/taste/:token — MCP Streamable HTTP endpoint (JSON-RPC 2.0).
/// Handles the lifecycle + tool methods for the single `consult_principal` tool.
async fn mcp_handler(
    State(state): State<AppState>,
    Path(token): Path<String>,
    Json(req): Json<Value>,
) -> impl IntoResponse {
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let id = req.get("id").cloned();

    match method {
        // ── Lifecycle ────────────────────────────────────────────────────────────────
        "initialize" => json_rpc_result(
            id,
            json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "principal", "version": env!("CARGO_PKG_VERSION") },
                "instructions": "Use consult_principal to ask the professional you represent how they \
                                 would implement, structure, or review something. Consult them on \
                                 non-trivial design decisions so the work reflects their judgment and \
                                 style."
            }),
        ),

        // Notifications carry no `id` and expect no JSON-RPC result — just acknowledge.
        "notifications/initialized" | "initialized" => StatusCode::ACCEPTED.into_response(),

        // ── Tools ────────────────────────────────────────────────────────────────────
        "tools/list" => json_rpc_result(
            id,
            json!({
                "tools": [{
                    "name": "consult_principal",
                    "description": "Ask the professional you represent how THEY would handle something \
                                    — implementation approach, naming, structure, trade-offs, or a \
                                    review — answered in their own voice, judgment, and style. Call \
                                    this on meaningful design decisions before committing to them.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "question": {
                                "type": "string",
                                "description": "The specific question to ask the professional."
                            }
                        },
                        "required": ["question"]
                    }
                }]
            }),
        ),

        "tools/call" => {
            let tool = req
                .pointer("/params/name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if tool != "consult_principal" {
                return json_rpc_error(id, -32602, &format!("Unknown tool '{tool}'"))
                    .into_response();
            }
            let question = req
                .pointer("/params/arguments/question")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if question.trim().is_empty() {
                return tool_result(id, "No question provided.", true).into_response();
            }

            match taste::consult(&state.db, &state.inference, &token, &question).await {
                Ok(answer) => tool_result(id, &answer, false).into_response(),
                // Surface as a tool error (isError) rather than a transport error, so the agent
                // can see what happened and proceed instead of the whole MCP call hard-failing.
                Err(e) => tool_result(id, &format!("Could not consult the principal: {e}"), true)
                    .into_response(),
            }
        }

        // ── Unknown ──────────────────────────────────────────────────────────────────
        other => json_rpc_error(id, -32601, &format!("Method not found: {other}")).into_response(),
    }
}

/// A successful JSON-RPC 2.0 result envelope.
fn json_rpc_result(id: Option<Value>, result: Value) -> axum::response::Response {
    Json(json!({ "jsonrpc": "2.0", "id": id, "result": result })).into_response()
}

/// A JSON-RPC 2.0 error envelope (protocol-level failure).
fn json_rpc_error(id: Option<Value>, code: i64, message: &str) -> axum::response::Response {
    Json(json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    }))
    .into_response()
}

/// An MCP `tools/call` result: a single text content block, with isError set as needed. A failed
/// tool is still a JSON-RPC *success* whose result carries `isError: true` (per MCP).
fn tool_result(id: Option<Value>, text: &str, is_error: bool) -> axum::response::Response {
    json_rpc_result(
        id,
        json!({
            "content": [{ "type": "text", "text": text }],
            "isError": is_error
        }),
    )
}
