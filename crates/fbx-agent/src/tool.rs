//! Typed tools — fbx-agent's vetted action catalog.
//!
//! The agent starts with NO tools; every capability is one we define here. Because the unit of
//! workflow composition is the DAG node, the toolset is deliberately minimal and sprite-local:
//! file ops + one bash + grep (act on the working tree), DAG control (`step_complete`,
//! `report_progress`), and the MCP likeness tools (`consult_principal`, `vault__*`). No
//! external-fetch tools — research is a DAG node, not a tool. No git-mutation tools — commit/push
//! are engine-directed nodes.
//!
//! A side-effecting tool is non-idempotent and must never be silently retried/memoized — the same
//! correctness rule the optimizer's trace stage enforces.

use async_trait::async_trait;
use serde_json::Value;

/// A tool the agent may call. `name`/`description`/`parameters` produce the native OpenAI
/// tools-array entry; `call` executes it against the raw JSON args string the model emitted.
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    /// JSON-schema for the arguments object (OpenAI `function.parameters`).
    fn parameters(&self) -> Value;
    /// Whether calling this tool with the same args is safe to repeat. Read-only tools are
    /// idempotent; anything with a side effect (write_file, run_command, step_complete) is not.
    /// Consumed by the trace/memoization stage (and asserted in tests); part of the tool contract.
    fn idempotent(&self) -> bool {
        true
    }
    /// Execute the tool against the raw arguments string from the model. Returns the tool's output
    /// as a string, fed back to the model as a `tool` message.
    async fn call(&self, args_json: &str) -> anyhow::Result<String>;

    /// The native OpenAI tools-array entry for this tool.
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": self.name(),
                "description": self.description(),
                "parameters": self.parameters(),
            }
        })
    }
}

/// A vetted catalog of tools the orchestrator can dispatch by name.
#[derive(Default)]
pub struct ToolCatalog {
    tools: Vec<Box<dyn Tool>>,
}

impl ToolCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, tool: Box<dyn Tool>) -> Self {
        self.tools.push(tool);
        self
    }

    /// The native OpenAI `tools` array for all tools in the catalog.
    pub fn schemas(&self) -> Vec<Value> {
        self.tools.iter().map(|t| t.schema()).collect()
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools
            .iter()
            .find(|t| t.name() == name)
            .map(|b| b.as_ref())
    }

    /// The names of every tool in the catalog — used to give a model a helpful "did you mean" list
    /// when it calls a tool that doesn't exist.
    pub fn names(&self) -> Vec<&str> {
        self.tools.iter().map(|t| t.name()).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;
    #[async_trait]
    impl Tool for Echo {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "echo the text back"
        }
        fn parameters(&self) -> Value {
            serde_json::json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"]
            })
        }
        async fn call(&self, args_json: &str) -> anyhow::Result<String> {
            let v: Value = serde_json::from_str(args_json)?;
            Ok(format!(
                "echo: {}",
                v.get("text").and_then(|t| t.as_str()).unwrap_or("")
            ))
        }
    }

    #[test]
    fn schema_is_native_openai_tool_shape() {
        let s = Echo.schema();
        assert_eq!(s["type"], "function");
        assert_eq!(s["function"]["name"], "echo");
        assert_eq!(s["function"]["parameters"]["required"][0], "text");
    }

    #[tokio::test]
    async fn catalog_dispatches_typed_args() {
        let cat = ToolCatalog::new().with(Box::new(Echo));
        assert_eq!(cat.schemas().len(), 1);
        let out = cat
            .get("echo")
            .unwrap()
            .call(r#"{"text":"hi"}"#)
            .await
            .unwrap();
        assert_eq!(out, "echo: hi");
        assert!(cat.get("nope").is_none());
        assert!(cat.get("echo").unwrap().call("not json").await.is_err());
    }
}
