//! fbx-agent's sprite-local toolset + DAG-control tools.
//!
//! All file/command tools are scoped to the repo dir (the sprite's `/workspace/repo` working
//! tree). `run_command` runs bash there for tests/build/lint/read-only git — it deliberately does
//! NOT do git commit/push/PR (those are engine-directed DAG nodes). The DAG-control tools
//! (`step_complete`, `report_progress`) talk to the workflow engine: `step_complete` ends the
//! node's step and hands output back; `report_progress` streams a status line.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tokio::process::Command;

use crate::orchestrator::{StepControl, StepOutput};
use crate::tool::Tool;

/// Resolve a model-supplied relative path against the repo root, rejecting escapes (`..`,
/// absolute paths) so a tool call can't touch anything outside the working tree.
fn safe_join(repo_dir: &str, rel: &str) -> anyhow::Result<PathBuf> {
    let rel_path = Path::new(rel);
    if rel_path.is_absolute() {
        anyhow::bail!("path must be relative to the repo root: {rel}");
    }
    if rel_path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        anyhow::bail!("path may not contain '..': {rel}");
    }
    Ok(Path::new(repo_dir).join(rel_path))
}

// ---- read_file -------------------------------------------------------------------------------

pub struct ReadFile {
    pub repo_dir: String,
}
#[derive(Deserialize)]
struct ReadArgs {
    path: String,
}
#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &str {
        "read_file"
    }
    fn description(&self) -> &str {
        "Read a UTF-8 text file from the repo (path relative to the repo root)."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "path": { "type": "string", "description": "relative path" } },
            "required": ["path"]
        })
    }
    async fn call(&self, args_json: &str) -> anyhow::Result<String> {
        let a: ReadArgs = serde_json::from_str(args_json)?;
        let p = safe_join(&self.repo_dir, &a.path)?;
        // Return clear, RECOVERABLE messages (not raw OS errors). A missing file is normal
        // information the model should act on ("it doesn't exist, so create it or move on"), not a
        // crash that derails the loop.
        match tokio::fs::read_to_string(&p).await {
            Ok(contents) => Ok(contents),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(format!(
                "(file '{}' does not exist yet — nothing to read. Create it with write_file if needed.)",
                a.path
            )),
            Err(e) => Ok(format!("(could not read '{}': {})", a.path, e)),
        }
    }
}

// ---- write_file ------------------------------------------------------------------------------

pub struct WriteFile {
    pub repo_dir: String,
}
#[derive(Deserialize)]
struct WriteArgs {
    path: String,
    content: String,
}
#[async_trait]
impl Tool for WriteFile {
    fn name(&self) -> &str {
        "write_file"
    }
    fn description(&self) -> &str {
        "Create or overwrite a file with the given content (path relative to the repo root)."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "content": { "type": "string" }
            },
            "required": ["path", "content"]
        })
    }
    fn idempotent(&self) -> bool {
        false // mutates the working tree
    }
    async fn call(&self, args_json: &str) -> anyhow::Result<String> {
        let a: WriteArgs = serde_json::from_str(args_json)?;
        let p = safe_join(&self.repo_dir, &a.path)?;
        if let Some(parent) = p.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&p, a.content.as_bytes()).await?;
        Ok(format!("wrote {} bytes to {}", a.content.len(), a.path))
    }
}

// ---- edit_file (targeted replace) ------------------------------------------------------------

pub struct EditFile {
    pub repo_dir: String,
}
#[derive(Deserialize)]
struct EditArgs {
    path: String,
    old: String,
    new: String,
}
#[async_trait]
impl Tool for EditFile {
    fn name(&self) -> &str {
        "edit_file"
    }
    fn description(&self) -> &str {
        "Replace an exact substring `old` with `new` in a file. `old` must appear exactly once."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "old": { "type": "string", "description": "exact text to replace (must be unique)" },
                "new": { "type": "string" }
            },
            "required": ["path", "old", "new"]
        })
    }
    fn idempotent(&self) -> bool {
        false
    }
    async fn call(&self, args_json: &str) -> anyhow::Result<String> {
        let a: EditArgs = serde_json::from_str(args_json)?;
        let p = safe_join(&self.repo_dir, &a.path)?;
        let content = tokio::fs::read_to_string(&p).await?;
        let count = content.matches(&a.old).count();
        if count == 0 {
            anyhow::bail!("`old` not found in {}", a.path);
        }
        if count > 1 {
            anyhow::bail!("`old` appears {count} times in {}; make it unique", a.path);
        }
        let updated = content.replacen(&a.old, &a.new, 1);
        tokio::fs::write(&p, updated.as_bytes()).await?;
        Ok(format!("edited {}", a.path))
    }
}

// ---- list_dir --------------------------------------------------------------------------------

pub struct ListDir {
    pub repo_dir: String,
}
#[derive(Deserialize)]
struct ListArgs {
    #[serde(default)]
    path: String,
}
#[async_trait]
impl Tool for ListDir {
    fn name(&self) -> &str {
        "list_dir"
    }
    fn description(&self) -> &str {
        "List the entries of a directory in the repo (path relative to the repo root; default root)."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "path": { "type": "string", "description": "relative dir (optional)" } }
        })
    }
    async fn call(&self, args_json: &str) -> anyhow::Result<String> {
        let a: ListArgs = serde_json::from_str(args_json).unwrap_or(ListArgs {
            path: String::new(),
        });
        let rel = if a.path.is_empty() { "." } else { &a.path };
        let p = safe_join(&self.repo_dir, rel)?;
        let mut rd = match tokio::fs::read_dir(&p).await {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(format!("(directory '{}' does not exist)", rel))
            }
            Err(e) => return Ok(format!("(could not list '{}': {})", rel, e)),
        };
        let mut entries = Vec::new();
        while let Some(e) = rd.next_entry().await? {
            let name = e.file_name().to_string_lossy().to_string();
            let suffix = if e.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
                "/"
            } else {
                ""
            };
            entries.push(format!("{name}{suffix}"));
        }
        entries.sort();
        if entries.is_empty() {
            Ok(format!("(directory '{}' is empty)", rel))
        } else {
            Ok(entries.join("\n"))
        }
    }
}

// ---- grep ------------------------------------------------------------------------------------

pub struct Grep {
    pub repo_dir: String,
}
#[derive(Deserialize)]
struct GrepArgs {
    pattern: String,
}
#[async_trait]
impl Tool for Grep {
    fn name(&self) -> &str {
        "grep"
    }
    fn description(&self) -> &str {
        "Search the repo for a regular expression, returning matching file:line:text (via ripgrep/grep)."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "pattern": { "type": "string" } },
            "required": ["pattern"]
        })
    }
    async fn call(&self, args_json: &str) -> anyhow::Result<String> {
        let a: GrepArgs = serde_json::from_str(args_json)?;
        // Prefer ripgrep; fall back to grep -rn. Pass the pattern as a single arg (no shell).
        let out = match Command::new("rg")
            .args(["-n", "--no-heading", &a.pattern, "."])
            .current_dir(&self.repo_dir)
            .output()
            .await
        {
            Ok(o) => o,
            Err(_) => {
                Command::new("grep")
                    .args(["-rn", &a.pattern, "."])
                    .current_dir(&self.repo_dir)
                    .output()
                    .await?
            }
        };
        let text = String::from_utf8_lossy(&out.stdout);
        Ok(if text.trim().is_empty() {
            "(no matches)".into()
        } else {
            // Cap output so a huge match set can't blow the context window.
            text.lines().take(200).collect::<Vec<_>>().join("\n")
        })
    }
}

// ---- run_command -----------------------------------------------------------------------------

pub struct RunCommand {
    pub repo_dir: String,
}
#[derive(Deserialize)]
struct RunArgs {
    command: String,
}
#[async_trait]
impl Tool for RunCommand {
    fn name(&self) -> &str {
        "run_command"
    }
    fn description(&self) -> &str {
        "Run a shell command in the repo dir (tests/build/lint/read-only git). Do NOT git commit or \
         push — the workflow does that. Returns combined stdout+stderr and the exit code."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "command": { "type": "string" } },
            "required": ["command"]
        })
    }
    fn idempotent(&self) -> bool {
        false // arbitrary command — assume side effects
    }
    async fn call(&self, args_json: &str) -> anyhow::Result<String> {
        let a: RunArgs = serde_json::from_str(args_json)?;
        let out = Command::new("bash")
            .arg("-lc")
            .arg(&a.command)
            .current_dir(&self.repo_dir)
            .output()
            .await?;
        let mut s = String::new();
        s.push_str(&String::from_utf8_lossy(&out.stdout));
        s.push_str(&String::from_utf8_lossy(&out.stderr));
        let code = out.status.code().unwrap_or(-1);
        Ok(format!("[exit {code}]\n{}", s.trim_end()))
    }
}

// ---- step_complete (DAG control) -------------------------------------------------------------

/// The tool the agent calls to declare this node's sprite work finished. It records the output +
/// summary into the shared `StepControl`, which ends the loop; the process then persists the
/// session and prints the output for the engine to lift into `ctx.vars`.
pub struct StepComplete {
    pub control: StepControl,
}
#[derive(Deserialize)]
struct StepCompleteArgs {
    /// Structured output for the workflow blackboard (any JSON).
    #[serde(default)]
    output: Value,
    /// A short human summary of what this step did.
    summary: String,
}
#[async_trait]
impl Tool for StepComplete {
    fn name(&self) -> &str {
        "step_complete"
    }
    fn description(&self) -> &str {
        "Call when THIS workflow node's sprite work is done. Hands `output` (JSON) back to the \
         workflow to pass downstream, plus a short `summary`. After this the workflow advances to \
         the next node; you'll be resumed later if a further sprite node needs you."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "output": { "description": "structured result for the workflow (any JSON)" },
                "summary": { "type": "string", "description": "one-line summary of what you did" }
            },
            "required": ["summary"]
        })
    }
    fn idempotent(&self) -> bool {
        false
    }
    async fn call(&self, args_json: &str) -> anyhow::Result<String> {
        let a: StepCompleteArgs = serde_json::from_str(args_json)?;
        self.control.complete(StepOutput {
            output: a.output,
            summary: a.summary.clone(),
        });
        Ok(format!("step complete: {}", a.summary))
    }
}

// ---- report_progress (DAG control) -----------------------------------------------------------

/// Streams a status line to stdout (the sprite transcript sink surfaces it in the node log).
pub struct ReportProgress;
#[derive(Deserialize)]
struct ProgressArgs {
    message: String,
}
#[async_trait]
impl Tool for ReportProgress {
    fn name(&self) -> &str {
        "report_progress"
    }
    fn description(&self) -> &str {
        "Emit a short progress note so the human watching the node sees what you're doing."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "message": { "type": "string" } },
            "required": ["message"]
        })
    }
    async fn call(&self, args_json: &str) -> anyhow::Result<String> {
        let a: ProgressArgs = serde_json::from_str(args_json)?;
        println!("[fbx-progress] {}", a.message);
        Ok("noted".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_join_rejects_escapes() {
        assert!(safe_join("/repo", "../etc/passwd").is_err());
        assert!(safe_join("/repo", "/etc/passwd").is_err());
        assert!(safe_join("/repo", "src/main.rs").is_ok());
    }

    #[tokio::test]
    async fn write_then_read_and_edit() {
        let dir = std::env::temp_dir().join(format!("fbx-tools-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let repo = dir.to_string_lossy().to_string();

        let w = WriteFile {
            repo_dir: repo.clone(),
        };
        w.call(r#"{"path":"a.txt","content":"hello world"}"#)
            .await
            .unwrap();

        let r = ReadFile {
            repo_dir: repo.clone(),
        };
        assert_eq!(r.call(r#"{"path":"a.txt"}"#).await.unwrap(), "hello world");

        let e = EditFile {
            repo_dir: repo.clone(),
        };
        e.call(r#"{"path":"a.txt","old":"world","new":"fbx"}"#)
            .await
            .unwrap();
        assert_eq!(r.call(r#"{"path":"a.txt"}"#).await.unwrap(), "hello fbx");

        // Non-unique `old` is rejected.
        w.call(r#"{"path":"b.txt","content":"x x"}"#).await.unwrap();
        assert!(e
            .call(r#"{"path":"b.txt","old":"x","new":"y"}"#)
            .await
            .is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn step_complete_sets_control() {
        let control = StepControl::new();
        let sc = StepComplete {
            control: control.clone(),
        };
        assert!(control.taken().is_none());
        sc.call(r#"{"output":{"branch":"x"},"summary":"did the thing"}"#)
            .await
            .unwrap();
        let out = control.taken().unwrap();
        assert_eq!(out.summary, "did the thing");
        assert_eq!(out.output["branch"], "x");
    }
}
