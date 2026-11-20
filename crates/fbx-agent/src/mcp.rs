//! MCP proxy tools — the likeness surface (`consult_principal` + the `vault__*` family).
//!
//! Rather than re-implement the taste/vault tools, fbx-agent proxies them to the SAME backend MCP
//! endpoints the Claude Code adapter uses (`taste_url` / `vault_url`) — Streamable-HTTP MCP
//! (stateless JSON-RPC 2.0 over POST). We `tools/list` each server at startup to discover its tools
//! + schemas, then expose each as a native OpenAI tool that forwards to `tools/call`. Discovering
//! (rather than hardcoding) keeps parity automatically: if the vault gains a tool, fbx-agent picks
//! it up. Behavioral parity with Claude Code comes from hitting the identical endpoint; only the
//! tool-name prefix differs (we own it here).

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::tool::Tool;

/// A single MCP server reachable over Streamable-HTTP JSON-RPC.
#[derive(Clone)]
pub struct McpEndpoint {
    /// Short label used to namespace tool names (e.g. `principal`, `vault`).
    pub label: String,
    pub url: String,
    http: reqwest::Client,
}

impl McpEndpoint {
    pub fn new(label: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            url: url.into(),
            http: reqwest::Client::new(),
        }
    }

    /// One JSON-RPC call. Returns the `result` value (or an error on transport / JSON-RPC error).
    async fn rpc(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        let resp = self.http.post(&self.url).json(&body).send().await?;
        let status = resp.status();
        // Notifications (e.g. initialized) return 202 with no body.
        if status.as_u16() == 202 {
            return Ok(Value::Null);
        }
        let json: Value = resp.json().await?;
        if let Some(err) = json.get("error") {
            anyhow::bail!("mcp {method} error: {err}");
        }
        Ok(json.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Discover the server's tools via `tools/list` (after the lifecycle `initialize`).
    pub async fn discover(&self) -> anyhow::Result<Vec<McpToolDef>> {
        // MCP lifecycle: initialize, then (best-effort) the initialized notification.
        let _ = self
            .rpc(
                "initialize",
                json!({
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": { "name": "fbx-agent", "version": env!("CARGO_PKG_VERSION") }
                }),
            )
            .await?;
        let _ = self.rpc("notifications/initialized", json!({})).await;

        let result = self.rpc("tools/list", json!({})).await?;
        let tools = result
            .get("tools")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(tools
            .into_iter()
            .filter_map(|t| {
                let name = t.get("name")?.as_str()?.to_string();
                Some(McpToolDef {
                    remote_name: name,
                    description: t
                        .get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("")
                        .to_string(),
                    // MCP calls it `inputSchema`; OpenAI wants `parameters`.
                    input_schema: t
                        .get("inputSchema")
                        .cloned()
                        .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
                })
            })
            .collect())
    }

    /// Invoke a discovered tool via `tools/call`, flattening the MCP text-content result.
    async fn call_remote(&self, remote_name: &str, arguments: Value) -> anyhow::Result<String> {
        let result = self
            .rpc(
                "tools/call",
                json!({ "name": remote_name, "arguments": arguments }),
            )
            .await?;
        // MCP result: { content: [{ type: "text", text }...], isError? }. Join the text blocks.
        let text = result
            .get("content")
            .and_then(|c| c.as_array())
            .map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        Ok(text)
    }
}

/// A tool definition discovered from an MCP server.
#[derive(Clone)]
pub struct McpToolDef {
    pub remote_name: String,
    pub description: String,
    pub input_schema: Value,
}

/// A native OpenAI tool that forwards to a discovered MCP tool. The exposed name is
/// `<label>_<remote_name>` (e.g. `principal_consult_principal`, `vault_search_vault`) so it's
/// unambiguous to the model; the call still hits the identical backend endpoint.
pub struct McpProxyTool {
    endpoint: McpEndpoint,
    def: McpToolDef,
    exposed_name: String,
}

impl McpProxyTool {
    pub fn new(endpoint: McpEndpoint, def: McpToolDef) -> Self {
        let exposed_name = format!("{}_{}", endpoint.label, def.remote_name);
        Self {
            endpoint,
            def,
            exposed_name,
        }
    }
}

#[async_trait]
impl Tool for McpProxyTool {
    fn name(&self) -> &str {
        &self.exposed_name
    }
    fn description(&self) -> &str {
        &self.def.description
    }
    fn parameters(&self) -> Value {
        self.def.input_schema.clone()
    }
    fn idempotent(&self) -> bool {
        // Vault writes (upsert_note/record_*) have side effects; consult/search/read don't. We
        // can't be sure from the name alone, so conservatively treat any `*note`/`record*`/`upsert*`
        // remote as non-idempotent, everything else idempotent.
        let n = &self.def.remote_name;
        !(n.contains("upsert") || n.starts_with("record") || n.contains("write"))
    }
    async fn call(&self, args_json: &str) -> anyhow::Result<String> {
        let arguments: Value = if args_json.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(args_json)?
        };
        self.endpoint
            .call_remote(&self.def.remote_name, arguments)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposed_name_namespaces_the_remote() {
        let ep = McpEndpoint::new("vault", "http://x/mcp/vault/tok");
        let def = McpToolDef {
            remote_name: "search_vault".into(),
            description: "search".into(),
            input_schema: json!({"type":"object"}),
        };
        let t = McpProxyTool::new(ep, def);
        assert_eq!(t.name(), "vault_search_vault");
        assert!(t.idempotent());
    }

    #[test]
    fn write_tools_are_non_idempotent() {
        let ep = McpEndpoint::new("vault", "http://x");
        for remote in ["upsert_note", "record_experience", "record_technical"] {
            let t = McpProxyTool::new(
                ep.clone(),
                McpToolDef {
                    remote_name: remote.into(),
                    description: String::new(),
                    input_schema: json!({}),
                },
            );
            assert!(!t.idempotent(), "{remote} should be non-idempotent");
        }
    }
}
