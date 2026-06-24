use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::types::JsonValue;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct Workflow {
    pub id: Uuid,
    pub agent_id: Uuid,
    pub aspect_id: Option<Uuid>,
    pub name: String,
    pub description: Option<String>,
    pub graph: JsonValue,     // node-graph JSON (nodes + edges)
    pub is_universal: bool,   // true = template library; false = job-specific
    pub job_id: Option<Uuid>, // set when is_universal = false
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A single node in the workflow graph, stored inside graph JSON.
/// Defined here for shared serialization between Rust engine and frontend.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WorkflowNode {
    pub id: String,
    pub node_type: NodeType,
    pub label: String,
    pub config: JsonValue, // node-specific config (e.g. GitHub repo, prompt text)
    pub position: NodePosition,
}

impl WorkflowNode {
    /// If this node performs an external side effect (a write that is NOT safe to blindly
    /// re-run — posting a comment, sending an email, creating a doc), return a stable
    /// "kind" string identifying the effect. Read/compute nodes return None.
    ///
    /// On a re-run, a side-effecting node that already succeeded becomes a decision point
    /// (reuse / update-in-place / re-fire) instead of silently duplicating the effect.
    pub fn side_effect_kind(&self) -> Option<&'static str> {
        let action = self.config.get("action").and_then(|v| v.as_str());
        let provider = self.config.get("provider").and_then(|v| v.as_str());
        match (&self.node_type, action, provider) {
            (NodeType::Github, Some("post_comment"), _) => Some("github_comment"),
            (NodeType::Slack, Some("post"), _) | (NodeType::Slack, None, _) => {
                Some("slack_message")
            }
            (NodeType::Email, Some("send"), _) | (NodeType::Email, None, _) => Some("email_send"),
            (NodeType::DocumentReadWrite, Some("write"), Some("notion")) => Some("notion_block"),
            (NodeType::DocumentReadWrite, Some("create"), Some("google_docs")) => {
                Some("google_doc")
            }
            (NodeType::JiraLinear, Some("update"), Some("linear")) => Some("linear_comment"),
            (NodeType::JiraLinear, Some("comment"), Some("jira")) => Some("jira_comment"),
            // Opening a PR is an external write; rerun goes through the side-effect gate.
            (NodeType::PrCreate, _, _) => Some("github_pr"),
            // Pushing a branch is an external write (a re-push is a refire/no-op, not editable).
            (NodeType::GitPush, _, _) => Some("git_branch"),
            _ => None,
        }
    }

    /// Whether an effect kind supports edit-in-place (update the existing artifact). Most do;
    /// email can't be unsent and a git push isn't an editable artifact (reuse/re-fire only).
    pub fn effect_supports_update(kind: &str) -> bool {
        !matches!(kind, "email_send" | "git_branch")
    }

    /// snake_case string for the node's type (matches the NodeType serde rename).
    pub fn node_type_str(&self) -> &'static str {
        match self.node_type {
            NodeType::Trigger => "trigger",
            NodeType::LlmReasoning => "llm_reasoning",
            NodeType::HumanGate => "human_gate",
            NodeType::Email => "email",
            NodeType::Slack => "slack",
            NodeType::WebSearch => "web_search",
            NodeType::DocumentReadWrite => "document_read_write",
            NodeType::HttpRequest => "http_request",
            NodeType::Schedule => "schedule",
            NodeType::ConditionalBranch => "conditional_branch",
            NodeType::Formatter => "formatter",
            NodeType::Output => "output",
            NodeType::Github => "github",
            NodeType::CodeExecution => "code_execution",
            NodeType::DiffAnalyzer => "diff_analyzer",
            NodeType::TestRunner => "test_runner",
            NodeType::JiraLinear => "jira_linear",
            NodeType::StackTraceParser => "stack_trace_parser",
            NodeType::AgentImplement => "agent_implement",
            NodeType::PrCreate => "pr_create",
            NodeType::SpriteCommand => "sprite_command",
            NodeType::SpriteTest => "sprite_test",
            NodeType::GitCommit => "git_commit",
            NodeType::GitPush => "git_push",
            NodeType::TestGate => "test_gate",
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct NodePosition {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WorkflowEdge {
    pub id: String,
    pub source: String, // node id
    pub target: String, // node id
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum NodeType {
    // Universal
    Trigger,
    LlmReasoning,
    HumanGate,
    Email,
    Slack,
    WebSearch,
    DocumentReadWrite,
    HttpRequest,
    Schedule,
    ConditionalBranch,
    Formatter,
    Output,
    // Software Engineering
    Github,
    CodeExecution,
    DiffAnalyzer,
    TestRunner,
    JiraLinear,
    StackTraceParser,
    // Work-execution (pluggable backend: Managed Agents / Sprite)
    AgentImplement, // coarse: a coding agent implements the task on a repo, returns a diff
    PrCreate,       // sprite-less: open a GitHub PR from a pushed branch
    SpriteCommand,  // run an arbitrary command on the task's sprite
    SpriteTest,     // run the project's test command on the sprite
    GitCommit,      // stage + commit on the sprite (local, idempotent)
    GitPush,        // push the branch (side-effecting)
    TestGate,       // run tests/lint; on failure loop back to the builder (bounded retries)
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkflowGraph {
    pub nodes: Vec<WorkflowNode>,
    pub edges: Vec<WorkflowEdge>,
}

#[derive(Debug, Deserialize)]
pub struct CreateWorkflowRequest {
    pub agent_id: Uuid,
    pub aspect_id: Option<Uuid>,
    pub name: String,
    pub description: Option<String>,
    pub graph: JsonValue,
    pub is_universal: bool,
    pub job_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateWorkflowRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub graph: Option<JsonValue>,
    pub is_universal: Option<bool>,
}
