//! The durable agent "brain" — the conversation history, persisted on the sprite working tree.
//!
//! This is what makes suspend/resume work: each sprite-node invocation loads the prior message
//! history, appends the new node's task, runs the loop until `step_complete`, and writes the
//! updated history back. The file rides `/workspace/.fbx/session.json` on the persistent tree; the
//! engine also captures it into the node checkpoint so a recreated VM can restore it. The history
//! is plain OpenAI message values (system/user/assistant/tool) — fully serializable, no hidden
//! in-process state — so a rehydrated process continues exactly where it left off.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

/// The serialized session: the whole conversation, plus a small step counter for observability.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Session {
    /// OpenAI message array (system/user/assistant/tool), in order.
    #[serde(default)]
    pub messages: Vec<Value>,
    /// How many workflow-node steps this session has processed.
    #[serde(default)]
    pub steps_done: usize,
}

impl Session {
    /// Load the session from `path`, or a fresh empty one if it doesn't exist yet (the first step).
    pub fn load_or_new(path: &Path) -> anyhow::Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Session::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Persist the session to `path`, creating the parent dir if needed. Written atomically (temp
    /// file + rename) so a crash mid-write can't corrupt the durable brain.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trips_and_starts_empty() {
        let dir = std::env::temp_dir().join(format!("fbx-sess-{}", std::process::id()));
        let path = dir.join("session.json");
        let _ = std::fs::remove_dir_all(&dir);

        // Missing file → fresh empty session.
        let fresh = Session::load_or_new(&path).unwrap();
        assert!(fresh.is_empty());
        assert_eq!(fresh.steps_done, 0);

        // Save then reload preserves messages + counter.
        let s = Session {
            messages: vec![json!({"role":"system","content":"you are fbx"})],
            steps_done: 2,
        };
        s.save(&path).unwrap();
        let back = Session::load_or_new(&path).unwrap();
        assert_eq!(back.steps_done, 2);
        assert_eq!(back.messages.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
