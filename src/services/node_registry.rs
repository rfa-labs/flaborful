//! Data-driven node manifest & registry.
//!
//! Historically a node's identity was scattered across four hand-maintained places that had to be
//! edited in lockstep to add one node: the `NodeType` enum (`models/workflow.rs`), and the palette
//! (`NODE_TYPES`), config fields (`CONFIG_FIELDS`), and dropdowns (`SELECT_OPTIONS`) in the one
//! 540-line builder Svelte file — plus the engine dispatch match in `workflow_engine.rs`.
//!
//! This module makes the *description* of a node — its category, label, help, per-node Loop Mode,
//! its typed input fields (rendered on the node body), and its named+typed output ports — a single
//! piece of data (`NodeManifest`) authored here in Rust and served to the frontend via
//! `GET /node-manifests`. The builder renders the palette and every node body/field generically
//! from these manifests, so adding a node is: author one manifest here + one handler in the engine.
//!
//! See the vault note `[[Flaborful Node Library]]` for the design and the roadmap of nodes still to
//! build on this registry.

use serde::Serialize;

/// The type a field's value is edited/validated as. Drives which control the frontend renders and
/// what `validate_config` accepts.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    /// Single-line string.
    Text,
    /// Multi-line string.
    Textarea,
    /// Numeric (optionally bounded via `min`/`max`, rendered as a slider when both are set).
    Number,
    /// On/off toggle.
    Boolean,
    /// One value from `options`.
    Enum,
    /// A file reference (upload / storage id / dynamic input). Reserved for roadmapped File/PDF/CSV
    /// nodes (see `[[Flaborful Node Library]]`); not used by the first batch.
    #[allow(dead_code)]
    File,
    /// A list of `{ key, value }` pairs (e.g. HTTP headers).
    KeyValue,
    /// A repeatable group of sub-fields (`item`) — user-defined schema rows, e.g. Extract Data's
    /// list of `{name, type, description}`, Router's routes, Categorizer's categories.
    Repeater,
}

/// The data type carried by an output port (and shown on a promoted input port). Purely advisory
/// today — used by the builder to color/label ports; the engine's `vars` are untyped JSON.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PortType {
    Text,
    /// Reserved for nodes that emit a list output (arrives with Loop Mode / roadmapped list nodes).
    #[allow(dead_code)]
    List,
    Any,
}

/// One editable field on a node. Serialized to the frontend, which renders the matching control.
#[derive(Debug, Clone, Serialize)]
pub struct FieldSpec {
    /// The `config` key this field reads/writes.
    pub key: &'static str,
    pub label: &'static str,
    #[serde(rename = "type")]
    pub field_type: FieldType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<&'static str>,
    /// Whether the field must be set (checked by `validate_config`, unless the field is promoted to
    /// a wired input port — a wired value satisfies `required`).
    pub required: bool,
    /// Default applied by the engine when the field is absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    /// `enum` options.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<&'static str>,
    /// Bounds for `number`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// Hidden behind "Show More Options".
    pub advanced: bool,
    /// Can be flipped to a wired input port ("Show as Input").
    pub dynamic: bool,
    /// Only shown when `config[dep_key] == dep_value` (conditional field, e.g. Website Scraper's
    /// Actions appearing only when "Take Action" is on).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visible_when: Option<(&'static str, &'static str)>,
    /// Sub-fields for a `repeater`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub item: Vec<FieldSpec>,
}

impl FieldSpec {
    /// A required single-line text field.
    fn text(key: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            field_type: FieldType::Text,
            help: None,
            placeholder: None,
            required: false,
            default: None,
            options: Vec::new(),
            min: None,
            max: None,
            advanced: false,
            dynamic: false,
            visible_when: None,
            item: Vec::new(),
        }
    }

    fn textarea(key: &'static str, label: &'static str) -> Self {
        Self {
            field_type: FieldType::Textarea,
            ..Self::text(key, label)
        }
    }

    fn boolean(key: &'static str, label: &'static str) -> Self {
        Self {
            field_type: FieldType::Boolean,
            ..Self::text(key, label)
        }
    }

    fn number(key: &'static str, label: &'static str) -> Self {
        Self {
            field_type: FieldType::Number,
            ..Self::text(key, label)
        }
    }

    fn enumeration(key: &'static str, label: &'static str, options: Vec<&'static str>) -> Self {
        Self {
            field_type: FieldType::Enum,
            options,
            ..Self::text(key, label)
        }
    }

    fn required(mut self) -> Self {
        self.required = true;
        self
    }

    fn dynamic(mut self) -> Self {
        self.dynamic = true;
        self
    }

    fn advanced(mut self) -> Self {
        self.advanced = true;
        self
    }

    fn placeholder(mut self, p: &'static str) -> Self {
        self.placeholder = Some(p);
        self
    }

    fn help(mut self, h: &'static str) -> Self {
        self.help = Some(h);
        self
    }

    fn range(mut self, min: f64, max: f64) -> Self {
        self.min = Some(min);
        self.max = Some(max);
        self
    }

    fn visible_when(mut self, key: &'static str, value: &'static str) -> Self {
        self.visible_when = Some((key, value));
        self
    }

    fn repeater(key: &'static str, label: &'static str, item: Vec<FieldSpec>) -> Self {
        Self {
            field_type: FieldType::Repeater,
            item,
            ..Self::text(key, label)
        }
    }
}

/// A named, typed output port (draggable in the builder).
#[derive(Debug, Clone, Serialize)]
pub struct PortSpec {
    pub key: &'static str,
    pub label: &'static str,
    #[serde(rename = "type")]
    pub port_type: PortType,
}

impl PortSpec {
    fn new(key: &'static str, label: &'static str, port_type: PortType) -> Self {
        Self {
            key,
            label,
            port_type,
        }
    }
}

/// The full description of one node type. `type` matches the `NodeType` serde string.
#[derive(Debug, Clone, Serialize)]
pub struct NodeManifest {
    #[serde(rename = "type")]
    pub node_type: &'static str,
    /// Palette taxonomy bucket (drives the categorized/searchable palette).
    pub category: Category,
    pub label: &'static str,
    /// Emoji/icon hint the frontend can render.
    pub icon: &'static str,
    /// Accent color (hex) — preserves the existing per-node colors.
    pub color: &'static str,
    pub help: &'static str,
    /// Shows the per-node Loop Mode toggle (fan-out over a list — see Subflows & Loop Mode phase).
    pub loopable: bool,
    /// Manifest version for this node type. A stored node records the version it was authored at
    /// (`config._node_version`); when this current version is higher, the builder offers an
    /// opt-in "upgrade" (Gumloop-style node versioning). Bump when a node type's contract changes.
    pub version: u32,
    pub inputs: Vec<FieldSpec>,
    pub outputs: Vec<PortSpec>,
}

/// Palette categories, mirroring Gumloop's Core-Nodes taxonomy plus flaborful's SWE nodes.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    FlowBasics,
    UsingAi,
    Utility,
    Mcp,
    Tools,
    SoftwareEngineering,
    WorkExecution,
}

// ── shared field fragments ──────────────────────────────────────────────────────────────────────

/// `output_var` — the ctx.vars key a node writes. Present on almost every flaborful node.
fn output_var(default_hint: &'static str) -> FieldSpec {
    FieldSpec::text("output_var", "Output variable").placeholder(default_hint)
}

/// AI model selector shared by the Using-AI nodes.
fn model_field() -> FieldSpec {
    FieldSpec::text("model_preference", "AI model")
        .advanced()
        .placeholder("auto-select")
        .help("Leave blank to use the professional's trained model")
}

fn temperature_field() -> FieldSpec {
    FieldSpec::number("temperature", "Temperature")
        .range(0.0, 1.0)
        .advanced()
}

// ── the manifests ───────────────────────────────────────────────────────────────────────────────

/// The full registry. One entry per node type. Ordered by category for the palette.
///
/// This intentionally documents the *existing* flaborful node types (so the manifest-driven builder
/// is a drop-in replacement with no behavioral regression) plus the Phase-1 first batch of new
/// Gumloop-parity nodes.
pub fn manifests() -> Vec<NodeManifest> {
    let mut m = Vec::new();

    // ── Flow Basics ──────────────────────────────────────────────────────────────────────────
    m.push(NodeManifest {
        node_type: "trigger",
        category: Category::FlowBasics,
        label: "Trigger",
        icon: "▶",
        color: "#7c4dff",
        help: "Entry point. Passes the task input through to downstream nodes.",
        loopable: false,
        version: 1,
        inputs: vec![],
        outputs: vec![PortSpec::new("trigger_output", "Output", PortType::Any)],
    });
    m.push(NodeManifest {
        node_type: "output",
        category: Category::FlowBasics,
        label: "Output",
        icon: "⏹",
        color: "#10b981",
        help: "Terminal node. Delivers the named variable as the final result.",
        loopable: false,
        version: 1,
        inputs: vec![field_output_deliver()],
        outputs: vec![],
    });
    m.push(NodeManifest {
        node_type: "human_gate",
        category: Category::FlowBasics,
        label: "Human Gate",
        icon: "✋",
        color: "#f59e0b",
        help: "Pauses the run for the owner to review the draft before continuing.",
        loopable: false,
        version: 1,
        inputs: vec![FieldSpec::text("input_var", "Variable to show")
            .placeholder("llm_output")
            .dynamic()],
        outputs: vec![PortSpec::new("approved", "Approved", PortType::Any)],
    });
    m.push(NodeManifest {
        node_type: "conditional_branch",
        category: Category::FlowBasics,
        label: "Branch",
        icon: "⑃",
        color: "#7c4dff",
        help: "Routes to a downstream branch based on a variable's value.",
        loopable: false,
        version: 1,
        inputs: vec![
            FieldSpec::text("condition_var", "Variable to evaluate")
                .required()
                .placeholder("llm_output")
                .dynamic(),
            FieldSpec::textarea("conditions", "Conditions (JSON)")
                .placeholder(r#"{"if_contains_bug": "yes", "otherwise": "no"}"#),
        ],
        outputs: vec![PortSpec::new("branch", "Branch", PortType::Any)],
    });
    m.push(NodeManifest {
        node_type: "filter",
        category: Category::FlowBasics,
        label: "Filter",
        icon: "⑂",
        color: "#7c4dff",
        help: "Passes the input through only when a condition holds — an if-statement for data.",
        loopable: true,
        version: 1,
        inputs: vec![
            FieldSpec::text("filter_by", "Filter by")
                .required()
                .dynamic()
                .help("The value evaluated against the condition"),
            FieldSpec::text("input_var", "Input to pass")
                .required()
                .dynamic()
                .placeholder("the value forwarded when the condition passes"),
            FieldSpec::enumeration(
                "condition",
                "Condition",
                vec![
                    "greater_than",
                    "less_than",
                    "equals",
                    "contains",
                    "is_in",
                    "starts_with",
                    "ends_with",
                    "is_empty",
                    "is_not_empty",
                    "is_true",
                    "is_false",
                ],
            )
            .required(),
            FieldSpec::text("conditional_value", "Conditional value").dynamic(),
            FieldSpec::boolean("output_blank", "Output blank when filtered out").advanced(),
            output_var("filtered_output"),
        ],
        outputs: vec![PortSpec::new(
            "filtered_output",
            "Filtered Output",
            PortType::Any,
        )],
    });
    m.push(NodeManifest {
        node_type: "router",
        category: Category::FlowBasics,
        label: "Router",
        icon: "🗺",
        color: "#7c4dff",
        help: "Splits the flow into up to 8 conditional routes, by AI or by rules.",
        loopable: true,
        version: 1,
        inputs: vec![
            FieldSpec::text("input_var", "Input to evaluate")
                .required()
                .dynamic(),
            FieldSpec::boolean("route_with_ai", "Route with AI")
                .help("AI interprets the input to pick a route; off = deterministic rule matching"),
            FieldSpec::repeater(
                "routes",
                "Routes",
                vec![
                    FieldSpec::text("name", "Route name").required(),
                    FieldSpec::text("condition", "Condition")
                        .help("Natural language (AI mode) or a rule expression"),
                ],
            )
            .required(),
            model_field(),
        ],
        outputs: vec![PortSpec::new("route", "Chosen route", PortType::Text)],
    });
    m.push(NodeManifest {
        node_type: "join_paths",
        category: Category::FlowBasics,
        label: "Join Paths",
        icon: "⋈",
        color: "#7c4dff",
        help: "Re-merges branches from a Router/Branch/Error Shield back into one path.",
        loopable: false,
        version: 1,
        inputs: vec![
            FieldSpec::text("inputs", "Branch variables (comma-separated)")
                .required()
                .placeholder("path_a, path_b")
                .help("The variable from whichever upstream branch actually ran passes through"),
            output_var("joined"),
        ],
        outputs: vec![PortSpec::new("joined", "Joined", PortType::Any)],
    });
    m.push(NodeManifest {
        node_type: "error_shield",
        category: Category::FlowBasics,
        label: "Error Shield",
        icon: "🛡",
        color: "#7c4dff",
        help: "Wraps a node so a failure routes to an error path instead of failing the run.",
        loopable: true,
        version: 1,
        inputs: vec![
            FieldSpec::text("input_var", "Input").required().dynamic(),
            FieldSpec::boolean("pass_inputs_through", "Pass inputs through on error")
                .help("Send the original input to the error path (to log/retry failed items)"),
            FieldSpec::text("success_var", "Success output variable").placeholder("shielded"),
            FieldSpec::text("error_var", "Error output variable").placeholder("shield_error"),
        ],
        outputs: vec![
            PortSpec::new("success", "Success", PortType::Any),
            PortSpec::new("error", "Error", PortType::Any),
        ],
    });

    m.push(NodeManifest {
        node_type: "subflow_input",
        category: Category::FlowBasics,
        label: "Input",
        icon: "⇥",
        color: "#12a594",
        help: "Entry point for a reusable workflow (subflow). Names a parameter the parent passes in.",
        loopable: false,
        version: 1,
        inputs: vec![
            FieldSpec::text("name", "Input name")
                .required()
                .placeholder("e.g. url")
                .help("The parameter name the parent maps a value onto"),
            output_var("input"),
        ],
        outputs: vec![PortSpec::new("value", "Value", PortType::Any)],
    });
    m.push(NodeManifest {
        node_type: "subflow_output",
        category: Category::FlowBasics,
        label: "Output",
        icon: "⇤",
        color: "#12a594",
        help: "Exit point for a subflow. Its value is lifted back into the parent workflow.",
        loopable: false,
        version: 1,
        inputs: vec![
            FieldSpec::text("name", "Output name")
                .required()
                .placeholder("e.g. result"),
            FieldSpec::text("value_var", "Value variable")
                .dynamic()
                .placeholder("the variable to return"),
        ],
        outputs: vec![],
    });
    m.push(NodeManifest {
        node_type: "subflow",
        category: Category::FlowBasics,
        label: "Subflow",
        icon: "▤",
        color: "#12a594",
        help: "Run another workflow as a node. Map inputs in, get its outputs back. Enable Loop Mode to run it once per item in a list.",
        loopable: true,
        version: 1,
        inputs: vec![
            FieldSpec::text("workflow_id", "Workflow")
                .required()
                .help("The workflow to run as a subflow"),
            FieldSpec::textarea("input_map", "Input mapping (JSON)")
                .placeholder(r#"{ "url": "current_url" }"#)
                .help("Maps each subflow Input name to a parent variable (or literal)"),
            FieldSpec::number("max_parallel", "Max parallel (loop mode)")
                .advanced()
                .placeholder("4"),
            output_var("subflow_result"),
        ],
        outputs: vec![PortSpec::new("subflow_result", "Result", PortType::Any)],
    });

    // ── Using AI ─────────────────────────────────────────────────────────────────────────────
    m.push(NodeManifest {
        node_type: "llm_reasoning",
        category: Category::UsingAi,
        label: "LLM Reasoning",
        icon: "✨",
        color: "#6a34ff",
        help: "Runs the professional's trained model over a prompt template.",
        loopable: true,
        version: 1,
        inputs: vec![
            FieldSpec::textarea("prompt", "Prompt template")
                .required()
                .placeholder("Analyze this: {{input}}")
                .dynamic(),
            FieldSpec::text("input_var", "Input variable").placeholder("trigger_output"),
            FieldSpec::boolean(
                "document_author",
                "Author a document (emit structured doc JSON for a Google Doc node)",
            ),
            output_var("llm_output"),
        ],
        outputs: vec![PortSpec::new("llm_output", "Response", PortType::Text)],
    });
    m.push(NodeManifest {
        node_type: "ask_ai",
        category: Category::UsingAi,
        label: "Ask AI",
        icon: "🤖",
        color: "#6a34ff",
        help: "Prompt an AI model and get a text response.",
        loopable: true,
        version: 1,
        inputs: vec![
            FieldSpec::textarea("prompt", "Prompt")
                .required()
                .placeholder("The main instruction or question for the AI")
                .dynamic(),
            model_field(),
            temperature_field(),
            FieldSpec::number("max_tokens", "Maximum tokens").advanced(),
            output_var("ai_response"),
        ],
        outputs: vec![PortSpec::new("ai_response", "Response", PortType::Text)],
    });
    m.push(NodeManifest {
        node_type: "build_blueprint",
        category: Category::UsingAi,
        label: "Build Blueprint",
        icon: "🧭",
        color: "#6a34ff",
        help: "Distills the professional's knowledge vault (and the company repo context) into a \
               task-scoped brief in their voice, for a downstream coding agent to follow. Wire it \
               into an Agent: Implement node's blueprint input to avoid re-distilling.",
        loopable: true,
        version: 1,
        inputs: vec![
            FieldSpec::textarea("task", "Task")
                .placeholder("Describe the task (or leave blank to use the task variable)")
                .dynamic(),
            FieldSpec::text("task_var", "Task variable").placeholder("input"),
            FieldSpec::text("repo", "Repo (owner/name)")
                .placeholder("inherits from ctx.vars[repo]; scopes the company vault context"),
            // No model field: the distill is a likeness step and must use the professional's
            // trained model (their LoRA) — a model override would undermine that.
            output_var("blueprint"),
        ],
        outputs: vec![PortSpec::new("blueprint", "Blueprint", PortType::Text)],
    });
    m.push(NodeManifest {
        node_type: "extract_data",
        category: Category::UsingAi,
        label: "Extract Data",
        icon: "📤",
        color: "#6a34ff",
        help: "Pull structured fields out of unstructured text using AI.",
        loopable: true,
        version: 1,
        inputs: vec![
            FieldSpec::textarea("text", "Text")
                .required()
                .dynamic()
                .help("The source content to extract from"),
            FieldSpec::repeater(
                "fields",
                "Data fields to extract",
                vec![
                    FieldSpec::text("name", "Name").required(),
                    FieldSpec::enumeration("type", "Type", vec!["text", "number", "boolean"]),
                    FieldSpec::text("description", "Description")
                        .help("Guidance for the AI on what to extract"),
                ],
            )
            .required(),
            FieldSpec::boolean("extract_list", "Extract a list (multiple items)"),
            model_field(),
            temperature_field(),
            output_var("extracted"),
        ],
        outputs: vec![PortSpec::new("extracted", "Extracted Data", PortType::Any)],
    });
    m.push(NodeManifest {
        node_type: "categorizer",
        category: Category::UsingAi,
        label: "Categorizer",
        icon: "🏷",
        color: "#6a34ff",
        help: "Classify input text into one of your defined categories.",
        loopable: true,
        version: 1,
        inputs: vec![
            FieldSpec::textarea("input_var", "Input")
                .required()
                .dynamic(),
            FieldSpec::repeater(
                "categories",
                "Categories",
                vec![
                    FieldSpec::text("name", "Name").required(),
                    FieldSpec::text("description", "Description")
                        .help("What belongs in this category"),
                ],
            )
            .required(),
            FieldSpec::boolean("include_justification", "Include justification"),
            model_field(),
            output_var("category"),
        ],
        outputs: vec![
            PortSpec::new("category", "Selected category", PortType::Text),
            PortSpec::new("justification", "Justification", PortType::Text),
        ],
    });
    m.push(NodeManifest {
        node_type: "scorer",
        category: Category::UsingAi,
        label: "Scorer",
        icon: "💯",
        color: "#6a34ff",
        help: "Rate content 0–100 against a custom rubric.",
        loopable: true,
        version: 1,
        inputs: vec![
            FieldSpec::textarea("item", "Item to evaluate")
                .required()
                .dynamic(),
            FieldSpec::textarea("criteria", "Criteria")
                .required()
                .placeholder("Clarity: 0-30, Accuracy: 0-40, Engagement: 0-30"),
            FieldSpec::boolean("include_justification", "Include justification"),
            model_field(),
            output_var("score"),
        ],
        outputs: vec![
            PortSpec::new("score", "Score", PortType::Text),
            PortSpec::new("justification", "Justification", PortType::Text),
        ],
    });

    // ── Utility ──────────────────────────────────────────────────────────────────────────────
    m.push(NodeManifest {
        node_type: "formatter",
        category: Category::Utility,
        label: "Formatter",
        icon: "🔤",
        color: "#7c4dff",
        help: "Reformats a variable (markdown / json / email / plain).",
        loopable: true,
        version: 1,
        inputs: vec![
            FieldSpec::text("input_var", "Input variable").placeholder("llm_output"),
            FieldSpec::enumeration(
                "format",
                "Format",
                vec!["markdown", "json", "email", "plain"],
            ),
            output_var("formatted_output"),
        ],
        outputs: vec![PortSpec::new(
            "formatted_output",
            "Formatted",
            PortType::Text,
        )],
    });
    m.push(NodeManifest {
        node_type: "combine_text",
        category: Category::Utility,
        label: "Combine Text",
        icon: "🔗",
        color: "#7c4dff",
        help: "Merge values into a template with {placeholder} substitution.",
        loopable: true,
        version: 1,
        inputs: vec![
            FieldSpec::textarea("format", "Format")
                .required()
                .placeholder("Hi {name}, your score is {score}.")
                .help("Use {var_name} placeholders that match upstream variables"),
            output_var("combined"),
        ],
        outputs: vec![PortSpec::new("combined", "Combined Text", PortType::Text)],
    });
    m.push(NodeManifest {
        node_type: "read_json",
        category: Category::Utility,
        label: "Read JSON Values",
        icon: "{}",
        color: "#7c4dff",
        help: "Extract values from JSON by dot-notation path.",
        loopable: true,
        version: 1,
        inputs: vec![
            FieldSpec::textarea("json", "JSON string")
                .required()
                .dynamic(),
            FieldSpec::text("keys", "Keys (comma-separated)")
                .required()
                .placeholder("data.author, items.0.id"),
            output_var("json_values"),
        ],
        outputs: vec![PortSpec::new("json_values", "Key Values", PortType::Any)],
    });
    m.push(NodeManifest {
        node_type: "write_json",
        category: Category::Utility,
        label: "Write JSON Value",
        icon: "{+}",
        color: "#7c4dff",
        help: "Set a value at a dot-notation path in a JSON object.",
        loopable: true,
        version: 1,
        inputs: vec![
            FieldSpec::textarea("json", "JSON string").dynamic(),
            FieldSpec::text("key", "Key (dot path)")
                .required()
                .placeholder("data.status"),
            FieldSpec::text("value", "Value").dynamic(),
            output_var("json_result"),
        ],
        outputs: vec![PortSpec::new("json_result", "Updated JSON", PortType::Text)],
    });
    m.push(NodeManifest {
        node_type: "http_request",
        category: Category::Utility,
        label: "HTTP Request",
        icon: "🌐",
        color: "#0ea5e9",
        help: "Make an HTTP request to a URL.",
        loopable: true,
        version: 1,
        inputs: vec![
            FieldSpec::text("url", "URL")
                .required()
                .placeholder("https://api.example.com/...")
                .dynamic(),
            FieldSpec::enumeration(
                "method",
                "Method",
                vec!["GET", "POST", "PUT", "PATCH", "DELETE"],
            ),
            output_var("http_response"),
        ],
        outputs: vec![PortSpec::new("http_response", "Response", PortType::Text)],
    });
    m.push(NodeManifest {
        node_type: "call_api",
        category: Category::Utility,
        label: "Call API",
        icon: "📡",
        color: "#0ea5e9",
        help: "Generic HTTP API call with headers and a body — reach any service.",
        loopable: true,
        version: 1,
        inputs: vec![
            FieldSpec::enumeration(
                "method",
                "Method",
                vec!["GET", "POST", "PUT", "PATCH", "DELETE"],
            )
            .required(),
            FieldSpec::text("url", "URL")
                .required()
                .placeholder("https://api.example.com/v1/resource")
                .dynamic(),
            FieldSpec {
                field_type: FieldType::KeyValue,
                ..FieldSpec::text("headers", "Headers")
            },
            FieldSpec::textarea("body", "Body")
                .dynamic()
                .visible_when("method", "POST"),
            output_var("api_response"),
        ],
        outputs: vec![PortSpec::new(
            "api_response",
            "API Response",
            PortType::Text,
        )],
    });

    // ── MCP ──────────────────────────────────────────────────────────────────────────────────
    m.push(NodeManifest {
        node_type: "mcp_server",
        category: Category::Mcp,
        label: "Custom MCP Server",
        icon: "🔌",
        color: "#5a27e0",
        help: "Attach any MCP server as a tool source and call one of its tools.",
        loopable: true,
        version: 1,
        inputs: vec![
            FieldSpec::text("server_url", "MCP server URL")
                .required()
                .placeholder("https://…/mcp"),
            FieldSpec::text("tool", "Tool name").required(),
            FieldSpec::textarea("arguments", "Arguments (JSON)")
                .dynamic()
                .placeholder(r#"{ "query": "…" }"#),
            output_var("mcp_result"),
        ],
        outputs: vec![PortSpec::new("mcp_result", "Result", PortType::Any)],
    });

    // ── Tools (existing integration/analysis nodes) ────────────────────────────────────────────
    m.push(NodeManifest {
        node_type: "github",
        category: Category::Tools,
        label: "GitHub",
        icon: "🐙",
        color: "#1f2937",
        help: "Read a diff or post a comment on GitHub.",
        loopable: false,
        version: 1,
        inputs: vec![
            FieldSpec::enumeration("action", "Action", vec!["fetch_diff", "post_comment"])
                .required(),
            output_var("diff"),
        ],
        outputs: vec![PortSpec::new("diff", "Output", PortType::Text)],
    });
    m.push(NodeManifest {
        node_type: "diff_analyzer",
        category: Category::SoftwareEngineering,
        label: "Diff Analyzer",
        icon: "🔍",
        color: "#1f2937",
        help: "Summarize and analyze a code diff.",
        loopable: false,
        version: 1,
        inputs: vec![
            FieldSpec::text("diff_var", "Diff variable")
                .placeholder("diff")
                .dynamic(),
            output_var("diff_analysis"),
        ],
        outputs: vec![PortSpec::new("diff_analysis", "Analysis", PortType::Text)],
    });
    m.push(NodeManifest {
        node_type: "stack_trace_parser",
        category: Category::SoftwareEngineering,
        label: "Stack Trace",
        icon: "🧵",
        color: "#1f2937",
        help: "Parse a stack trace into structured frames.",
        loopable: false,
        version: 1,
        inputs: vec![
            FieldSpec::text("input_var", "Input variable")
                .placeholder("input")
                .dynamic(),
            output_var("parsed_trace"),
        ],
        outputs: vec![PortSpec::new("parsed_trace", "Parsed", PortType::Any)],
    });
    m.push(NodeManifest {
        node_type: "web_search",
        category: Category::Tools,
        label: "Web Search",
        icon: "🔎",
        color: "#34a853",
        help: "Search the web for current information.",
        loopable: true,
        version: 1,
        inputs: vec![
            FieldSpec::text("query_var", "Query variable").dynamic(),
            output_var("search_results"),
        ],
        outputs: vec![PortSpec::new("search_results", "Results", PortType::Any)],
    });
    m.push(NodeManifest {
        node_type: "google_docs",
        category: Category::Tools,
        label: "Google Doc",
        icon: "📄",
        color: "#1a73e8",
        help: "Create a formatted Google Doc from a structured document model. Feed from an LLM Reasoning node with \"Author a document\" enabled.",
        loopable: false,
        version: 1,
        inputs: vec![
            FieldSpec::text("model_var", "Document model variable").placeholder("llm_output"),
            FieldSpec::text("file_name", "File name").placeholder("defaults to the doc title"),
            FieldSpec::text("shared_drive_id", "Shared Drive ID")
                .advanced()
                .placeholder("inherits from job grant if blank"),
            FieldSpec::text("parent_folder_id", "Folder ID")
                .advanced()
                .placeholder("drive root if blank"),
            output_var("created_doc"),
        ],
        outputs: vec![PortSpec::new("created_doc", "Created Doc", PortType::Any)],
    });

    // ── Software Engineering / Work-execution (existing) ───────────────────────────────────────
    m.push(NodeManifest {
        node_type: "agent_implement",
        category: Category::WorkExecution,
        label: "Agent: Implement",
        icon: "🛠",
        color: "#0f766e",
        help: "A coding agent implements the task on the repo and returns a diff.",
        loopable: false,
        version: 1,
        inputs: vec![
            FieldSpec::textarea("task", "Task for the agent")
                .placeholder("Describe the change (or leave blank to use task_var)"),
            FieldSpec::text("task_var", "Task variable").placeholder("input"),
            FieldSpec::textarea("instructions", "Agent instructions").placeholder(
                "e.g. Consult the principal on design decisions. Write and run tests.",
            ),
            FieldSpec::text("repo", "Repo (owner/name)")
                .placeholder("inherits from ctx.vars[repo]"),
            FieldSpec::text("base_branch", "Base branch").placeholder("main"),
            FieldSpec::text("branch", "Work branch").placeholder("auto: flaborful/task-…"),
            FieldSpec::text("blueprint_var", "Blueprint variable")
                .advanced()
                .placeholder(
                    "blueprint (from an upstream Build Blueprint node; else built inline)",
                ),
            // NOTE: the execution environment (managed / Claude Code / optimizer fbx-agent) is a
            // WORKFLOW-level setting (Workflow Settings), not per-node — every sprite node in a run
            // uses the same one so the fbx-agent session spans the whole DAG. So there is no per-node
            // environment/backend field here.
            output_var("implementation"),
        ],
        outputs: vec![PortSpec::new(
            "implementation",
            "Implementation",
            PortType::Any,
        )],
    });
    m.push(NodeManifest {
        node_type: "sprite_command",
        category: Category::WorkExecution,
        label: "Run Command",
        icon: "⌨",
        color: "#0e7490",
        help: "Run an arbitrary command on the task's sprite.",
        loopable: false,
        version: 1,
        inputs: vec![
            FieldSpec::textarea("command", "Command")
                .placeholder("e.g. npm run build (or leave blank to use command_var)"),
            FieldSpec::text("command_var", "Command variable").placeholder("command"),
            FieldSpec::number("timeout_s", "Timeout (seconds)")
                .placeholder("300")
                .advanced(),
            output_var("command_result"),
        ],
        outputs: vec![PortSpec::new("command_result", "Result", PortType::Text)],
    });
    m.push(NodeManifest {
        node_type: "sprite_test",
        category: Category::WorkExecution,
        label: "Run Tests",
        icon: "🧪",
        color: "#0891b2",
        help: "Run the project's test command on the sprite.",
        loopable: false,
        version: 1,
        inputs: vec![
            FieldSpec::text("test_command", "Test command").placeholder("npm test"),
            FieldSpec::number("timeout_s", "Timeout (seconds)")
                .placeholder("600")
                .advanced(),
            output_var("test_results"),
        ],
        outputs: vec![PortSpec::new("test_results", "Results", PortType::Text)],
    });
    m.push(NodeManifest {
        node_type: "git_commit",
        category: Category::WorkExecution,
        label: "Git Commit",
        icon: "✔",
        color: "#374151",
        help: "Stage and commit on the sprite (local, idempotent).",
        loopable: false,
        version: 1,
        inputs: vec![
            FieldSpec::textarea("message", "Commit message")
                .placeholder("Changes by your Flaborful agent (or use message_var)"),
            FieldSpec::text("message_var", "Message variable").placeholder("commit_message"),
            FieldSpec::text("add_paths", "Paths to stage")
                .placeholder("-A")
                .advanced(),
            output_var("commit_result"),
        ],
        outputs: vec![PortSpec::new("commit_result", "Result", PortType::Text)],
    });
    m.push(NodeManifest {
        node_type: "git_push",
        category: Category::WorkExecution,
        label: "Git Push",
        icon: "⬆",
        color: "#111827",
        help: "Push the branch (side-effecting).",
        loopable: false,
        version: 1,
        inputs: vec![
            FieldSpec::text("branch", "Branch").placeholder("inherits from implementation.branch"),
            FieldSpec::boolean("force", "Force push").advanced(),
            output_var("push_result"),
        ],
        outputs: vec![PortSpec::new("push_result", "Result", PortType::Text)],
    });
    m.push(NodeManifest {
        node_type: "test_gate",
        category: Category::WorkExecution,
        label: "Test Gate",
        icon: "🚦",
        color: "#b45309",
        help: "Run tests/lint; on failure loop back to the builder (bounded retries).",
        loopable: false,
        version: 1,
        inputs: vec![
            FieldSpec::text("test_command", "Test command").placeholder("npm test"),
            FieldSpec::text("lint_command", "Lint command").placeholder("npm run lint"),
            FieldSpec::number("max_retries", "Max fix attempts").placeholder("2"),
            FieldSpec::number("timeout_s", "Timeout (seconds)")
                .placeholder("600")
                .advanced(),
            // Execution environment is workflow-level (see agent_implement note), not per node.
            output_var("gate_result"),
        ],
        outputs: vec![PortSpec::new("gate_result", "Result", PortType::Any)],
    });
    m.push(NodeManifest {
        node_type: "pr_create",
        category: Category::WorkExecution,
        label: "Create PR",
        icon: "🔀",
        color: "#1f2937",
        help: "Open a GitHub PR from a pushed branch.",
        loopable: false,
        version: 1,
        inputs: vec![
            FieldSpec::text("repo", "Repo (owner/name)")
                .placeholder("inherits from ctx.vars[repo]"),
            FieldSpec::text("source_var", "Branch source variable").placeholder("push_result"),
            FieldSpec::text("base", "Base branch").placeholder("main"),
            FieldSpec::text("title", "PR title").placeholder("Changes by your Flaborful agent"),
            FieldSpec::text("body_var", "PR body variable").placeholder("llm_output"),
            FieldSpec::boolean("draft", "Open as draft").advanced(),
            output_var("pr_result"),
        ],
        outputs: vec![PortSpec::new("pr_result", "Result", PortType::Any)],
    });

    m
}

/// `output` node's deliver field (declared as a const so the manifest table stays readable).
/// The `output` node's deliver field.
fn field_output_deliver() -> FieldSpec {
    FieldSpec::text("output_var", "Variable to deliver")
        .required()
        .dynamic()
        .placeholder("llm_output")
}

/// Look up a single manifest by node-type string.
pub fn manifest_for(node_type: &str) -> Option<NodeManifest> {
    manifests().into_iter().find(|m| m.node_type == node_type)
}

/// Validate a node's `config` object against its manifest: required fields present, enum values in
/// range, numbers parseable. A field that is wired as an input port (its key present in
/// `wired_keys`) satisfies `required` even if absent from `config`. Returns the list of problems
/// (empty = valid). Kept permissive on types the frontend already constrains.
pub fn validate_config(
    node_type: &str,
    config: &serde_json::Value,
    wired_keys: &[String],
) -> Vec<String> {
    let Some(manifest) = manifest_for(node_type) else {
        return vec![format!("unknown node type: {node_type}")];
    };
    let mut problems = Vec::new();
    for field in manifest.inputs {
        let present = config.get(field.key).map(|v| !v.is_null()).unwrap_or(false);
        let wired = wired_keys.iter().any(|k| k == field.key);
        if field.required && !present && !wired {
            problems.push(format!("{}: required field missing", field.key));
            continue;
        }
        if !present {
            continue;
        }
        let value = &config[field.key];
        match field.field_type {
            FieldType::Enum => {
                if let Some(s) = value.as_str() {
                    if !s.is_empty() && !field.options.contains(&s) {
                        problems.push(format!(
                            "{}: '{}' is not one of {:?}",
                            field.key, s, field.options
                        ));
                    }
                }
            }
            FieldType::Number => {
                let n = value
                    .as_f64()
                    .or_else(|| value.as_str().and_then(|s| s.parse().ok()));
                match n {
                    None => problems.push(format!("{}: expected a number", field.key)),
                    Some(n) => {
                        if let Some(min) = field.min {
                            if n < min {
                                problems.push(format!("{}: below minimum {}", field.key, min));
                            }
                        }
                        if let Some(max) = field.max {
                            if n > max {
                                problems.push(format!("{}: above maximum {}", field.key, max));
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_manifest_serializes_and_has_unique_type() {
        let all = manifests();
        let mut seen = std::collections::HashSet::new();
        for m in &all {
            assert!(
                seen.insert(m.node_type),
                "duplicate node_type: {}",
                m.node_type
            );
            // serialization must not panic
            serde_json::to_value(m).unwrap();
        }
        assert!(all.len() > 20, "expected the full node catalog");
    }

    #[test]
    fn validate_catches_missing_required_and_bad_enum() {
        // extract_data requires `text` and `fields`
        let problems = validate_config("extract_data", &json!({}), &[]);
        assert!(problems.iter().any(|p| p.contains("text")));
        assert!(problems.iter().any(|p| p.contains("fields")));

        // a wired input satisfies required
        let problems = validate_config(
            "extract_data",
            &json!({ "fields": [] }),
            &["text".to_string()],
        );
        assert!(!problems.iter().any(|p| p.starts_with("text")));

        // bad enum value
        let problems = validate_config(
            "call_api",
            &json!({ "method": "FETCH", "url": "https://x" }),
            &[],
        );
        assert!(problems.iter().any(|p| p.contains("method")));
    }

    #[test]
    fn build_blueprint_node_is_registered_and_needs_no_required_config() {
        // The node exists in the manifest, under Using AI, with a `blueprint` output port.
        let m = manifest_for("build_blueprint").expect("build_blueprint manifest missing");
        assert_eq!(m.label, "Build Blueprint");
        assert!(m.outputs.iter().any(|p| p.key == "blueprint"));
        // All inputs default (task/task_var/repo/output_var) — it's droppable with zero config.
        assert!(validate_config("build_blueprint", &json!({}), &[]).is_empty());
        // It must NOT expose a model override — the distill is a likeness (LoRA) step.
        assert!(!m.inputs.iter().any(|f| f.key == "model_preference"));
    }

    #[test]
    fn json_shape_matches_frontend_contract() {
        // The Svelte builder (nodeManifests.ts) reads these exact keys.
        let extract = manifest_for("extract_data").unwrap();
        let v = serde_json::to_value(&extract).unwrap();
        assert_eq!(v["type"], "extract_data");
        assert_eq!(v["category"], "using_ai");
        assert_eq!(v["loopable"], true);
        // repeater field carries a nested `item` array with typed sub-fields
        let fields = v["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["key"] == "fields")
            .unwrap();
        assert_eq!(fields["type"], "repeater");
        assert!(fields["item"].as_array().unwrap().len() >= 2);
        // output ports are named + typed
        assert_eq!(v["outputs"][0]["type"], "any");

        // conditional field serializes `visible_when` as a [dep, val] tuple
        let call_api = serde_json::to_value(manifest_for("call_api").unwrap()).unwrap();
        let body = call_api["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["key"] == "body")
            .unwrap();
        assert_eq!(body["visible_when"], serde_json::json!(["method", "POST"]));
    }

    #[test]
    fn temperature_bounds_enforced() {
        let problems = validate_config("ask_ai", &json!({ "prompt": "hi", "temperature": 5 }), &[]);
        assert!(problems.iter().any(|p| p.contains("temperature")));
    }
}
