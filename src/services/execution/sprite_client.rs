//! Thin client over the Fly.io Sprites REST API (sprites.dev).
//!
//! Base: https://api.sprites.dev/v1 ; auth: `Authorization: Bearer $SPRITES_TOKEN`.
//!
//! Wire formats verified against the official API reference (sprites.dev/api):
//! - create / get / destroy / set_egress: plain REST JSON.
//! - exec: WSS `/v1/sprites/{name}/exec` with query params + binary frame protocol
//!   (stream-id byte + payload: 0=stdin, 1=stdout, 2=stderr, 3=exit). Sessions are
//!   detachable and reattachable via `WSS /v1/sprites/{name}/exec/{session_id}`.
//! - checkpoint: `POST /v1/sprites/{name}/checkpoint` (singular), NDJSON streaming response.
//! - restore: `POST /v1/sprites/{name}/checkpoints/{id}/restore` (id in path), NDJSON.
//!
//! Disabled gracefully (like SANDBOX_URL) when `SPRITES_TOKEN` is unset.

use anyhow::{anyhow, Context, Result};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::json;
use std::time::{Duration, Instant};
use tokio_tungstenite::{
    connect_async, tungstenite::client::IntoClientRequest, tungstenite::Message, MaybeTlsStream,
    WebSocketStream,
};

const API: &str = "https://api.sprites.dev/v1";
const WSS_API: &str = "wss://api.sprites.dev/v1";

/// Stream-id bytes for the Sprites exec binary frame protocol (non-PTY mode).
#[allow(dead_code)] // documents the protocol; stdin frames not yet sent
const STREAM_STDIN: u8 = 0;
const STREAM_STDOUT: u8 = 1;
const STREAM_STDERR: u8 = 2;
const STREAM_EXIT: u8 = 3;

type WsStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

#[derive(Clone)]
pub struct SpriteClient {
    http: reqwest::Client,
    token: String,
    org: Option<String>,
    enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SpriteHandle {
    pub name: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ExecOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

impl SpriteClient {
    pub fn from_env() -> Self {
        let token = std::env::var("SPRITES_TOKEN").unwrap_or_default();
        let org = std::env::var("SPRITES_ORG").ok();
        Self {
            http: reqwest::Client::new(),
            enabled: !token.is_empty(),
            token,
            org,
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let mut rb = self
            .http
            .request(method, format!("{}{}", API, path))
            .bearer_auth(&self.token);
        if let Some(org) = &self.org {
            rb = rb.header("X-Sprites-Org", org);
        }
        rb
    }

    /// Create a sprite (waits for capacity). Returns its name + url.
    pub async fn create(&self, name: &str) -> Result<SpriteHandle> {
        let v: SpriteHandle = self
            .req(reqwest::Method::POST, "/sprites")
            .json(&json!({ "name": name, "wait_for_capacity": true }))
            .send()
            .await?
            .error_for_status()
            .context("Sprites create")?
            .json()
            .await?;
        Ok(v)
    }

    /// Current status. Errors if the sprite is gone (404).
    pub async fn get(&self, name: &str) -> Result<SpriteHandle> {
        let resp = self
            .req(reqwest::Method::GET, &format!("/sprites/{}", name))
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(anyhow!("sprite gone"));
        }
        Ok(resp.error_for_status()?.json().await?)
    }

    pub async fn destroy(&self, name: &str) -> Result<()> {
        let resp = self
            .req(reqwest::Method::DELETE, &format!("/sprites/{}", name))
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND || resp.status().is_success() {
            return Ok(());
        }
        resp.error_for_status()?;
        Ok(())
    }

    /// Capture a checkpoint of the sprite's filesystem state and return its id.
    ///
    /// Wire: `POST /v1/sprites/{name}/checkpoint` (singular) with body `{"comment": "..."}`.
    /// Response is NDJSON streaming: info/error/complete events. The checkpoint id is
    /// embedded in an info event's data string (`"  ID: v2"`) and in the complete event
    /// (`"Checkpoint v2 created successfully"`). We parse both for robustness.
    pub async fn checkpoint(&self, name: &str) -> Result<String> {
        let resp = self
            .req(
                reqwest::Method::POST,
                &format!("/sprites/{}/checkpoint", name),
            )
            .json(&json!({ "comment": "flaborful pre-node snapshot" }))
            .send()
            .await?
            .error_for_status()
            .context("Sprites checkpoint")?;

        let body = resp.text().await.context("reading checkpoint NDJSON")?;
        let mut checkpoint_id: Option<String> = None;

        for line in body.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let v: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
            let data = v.get("data").and_then(|d| d.as_str()).unwrap_or("");

            match ty {
                "error" => {
                    return Err(anyhow!("checkpoint error: {}", data));
                }
                "info" => {
                    // Extract id from "  ID: v2" lines.
                    if let Some(rest) = data.find("ID:") {
                        let after = data[rest + 3..].trim();
                        if !after.is_empty() {
                            checkpoint_id = Some(after.to_string());
                        }
                    }
                }
                "complete" => {
                    // "Checkpoint v2 created successfully" — extract the id after "Checkpoint ".
                    if checkpoint_id.is_none() {
                        if let Some(rest) = data.strip_prefix("Checkpoint ") {
                            if let Some(end) = rest.find(' ') {
                                let id = &rest[..end];
                                if !id.is_empty() {
                                    checkpoint_id = Some(id.to_string());
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        checkpoint_id.ok_or_else(|| anyhow!("checkpoint response did not contain an id"))
    }

    /// Restore the sprite's filesystem to a previously captured checkpoint.
    ///
    /// Wire: `POST /v1/sprites/{name}/checkpoints/{checkpoint_id}/restore` (id in path, no body).
    /// Response is NDJSON streaming. We consume to completion and surface errors.
    pub async fn restore(&self, name: &str, checkpoint_id: &str) -> Result<()> {
        let resp = self
            .req(
                reqwest::Method::POST,
                &format!("/sprites/{}/checkpoints/{}/restore", name, checkpoint_id),
            )
            .send()
            .await?
            .error_for_status()
            .context("Sprites restore")?;

        let body = resp.text().await.context("reading restore NDJSON")?;
        for line in body.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let v: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
            let data = v.get("data").and_then(|d| d.as_str()).unwrap_or("");
            if ty == "error" {
                return Err(anyhow!("restore error: {}", data));
            }
        }
        Ok(())
    }

    /// Restrict the sprite's outbound network to an allowlist of hosts (egress strategy).
    ///
    /// Wire: `POST /v1/sprites/{name}/policy/network` with `{rules: [{action, domain}…]}`.
    /// We map each allowed host to an `allow` rule and append a trailing `{deny, "*"}` so
    /// everything else is blocked. An empty allowlist is a no-op (no policy = unrestricted).
    ///
    /// NOTE: applying any network policy makes Fly's egress proxy block ALL UDP, which would
    /// kill a WireGuard/OpenVPN tunnel. Callers MUST NOT call this for a `network` (VPN) job —
    /// that exclusion is enforced upstream in `sprite_env`/`sprite::ensure_sprite`.
    pub async fn set_egress(&self, name: &str, allowlist: &[String]) -> Result<()> {
        if allowlist.is_empty() {
            return Ok(());
        }
        let mut rules: Vec<serde_json::Value> = allowlist
            .iter()
            .map(|host| json!({ "action": "allow", "domain": host }))
            .collect();
        rules.push(json!({ "action": "deny", "domain": "*" }));
        self.req(
            reqwest::Method::POST,
            &format!("/sprites/{}/policy/network", name),
        )
        .json(&json!({ "rules": rules }))
        .send()
        .await?
        .error_for_status()
        .context("Sprites set_egress (policy/network)")?;
        Ok(())
    }

    /// Run a command in the sprite and return its output.
    ///
    /// Wire: `WSS /v1/sprites/{name}/exec` with query params (`path`, `cmd`, `dir`, `env`,
    /// `max_run_after_disconnect`) and the Sprites binary frame protocol (stream-id byte +
    /// payload). We wrap the command in `bash -c` so callers can pass shell pipelines.
    ///
    /// Sessions are detachable: if the WSS connection drops before we see an exit frame, we
    /// reattach via `WSS /v1/sprites/{name}/exec/{session_id}`. On reattach, the server replays
    /// the session's scrollback as stdout/stderr frames; we dedup by skipping the bytes we
    /// already received. This survives brief network blips and long-running commands (up to
    /// `max_run_after_disconnect`).
    ///
    /// Env note: per the docs, setting `env` *replaces* the default environment (including
    /// PATH). Callers that need PATH should include it explicitly or set vars inline in the
    /// command string.
    pub async fn exec(
        &self,
        name: &str,
        command: &str,
        cwd: Option<&str>,
        env: &[(String, String)],
        timeout_secs: u64,
    ) -> Result<ExecOutput> {
        self.exec_streaming(name, command, cwd, env, timeout_secs, None)
            .await
    }

    /// Like `exec`, but if `sink` is provided, stdout/stderr chunks are pushed to it live as the
    /// command runs (so a long-running coding agent's transcript streams into the node logs in
    /// real time instead of arriving all at once when the command finishes).
    pub async fn exec_streaming(
        &self,
        name: &str,
        command: &str,
        cwd: Option<&str>,
        env: &[(String, String)],
        timeout_secs: u64,
        sink: Option<&dyn super::TranscriptSink>,
    ) -> Result<ExecOutput> {
        // Build query params. We run `bash -c "<command>"` so shell operators (&&, ||, |) work.
        // NOTE: the WebSocket exec endpoint has NO `dir`/`workdir` param (that's POST-only), so we
        // thread the working directory into the script with a `cd` prefix instead. Passing `dir`
        // here was silently ineffective and left commands running in the wrong cwd.
        let script = match cwd {
            Some(dir) => format!("cd {} && {}", shell_quote(dir), command),
            None => command.to_string(),
        };
        // The program must be the FIRST `cmd` value, with each argument a further repeated `cmd`.
        // Do NOT set `path`: when `path` is present the Sprites exec endpoint ignores the `cmd`
        // argv and passes the script as a single arg to the program (so `bash -c` never takes
        // effect and bash treats the whole script as a filename → exit 127). Verified empirically.
        let mut params: Vec<(String, String)> = Vec::with_capacity(5 + env.len());
        params.push(("cmd".into(), "/bin/bash".into()));
        params.push(("cmd".into(), "-c".into()));
        params.push(("cmd".into(), script));
        // Keep the session alive after disconnect for the full timeout so reattach is possible.
        let max_disconnect = timeout_secs.max(30);
        params.push((
            "max_run_after_disconnect".into(),
            format!("{}s", max_disconnect),
        ));
        for (k, v) in env {
            params.push(("env".into(), format!("{}={}", k, v)));
        }

        let url = format!(
            "{}/sprites/{}/exec?{}",
            WSS_API,
            url_encode(name),
            encode_query(&params)
        );
        let deadline = Instant::now() + Duration::from_secs(timeout_secs + 60);

        // First connection.
        let mut ws = self.connect_wss(&url).await?;
        let mut session_id: Option<String> = None;
        let mut stdout = String::new();
        let mut stderr = String::new();
        let mut exit_code: Option<i32> = None;
        // Track bytes consumed on the current connection so we can dedup scrollback on reattach.
        let mut conn_stdout_bytes: usize = 0;
        let mut conn_stderr_bytes: usize = 0;

        loop {
            if Instant::now() > deadline {
                let _ = ws.close(None).await;
                return Err(anyhow!("sprite exec timed out after {}s", timeout_secs));
            }

            let msg = tokio::time::timeout(Duration::from_secs(30), ws.next()).await;
            match msg {
                Ok(Some(Ok(Message::Binary(bin)))) => {
                    if bin.is_empty() {
                        continue;
                    }
                    let stream_id = bin[0];
                    let payload = &bin[1..];
                    match stream_id {
                        STREAM_STDOUT => {
                            let chunk = if conn_stdout_bytes < stdout.len() {
                                let skip = (stdout.len() - conn_stdout_bytes).min(payload.len());
                                String::from_utf8_lossy(&payload[skip..]).into_owned()
                            } else {
                                String::from_utf8_lossy(payload).into_owned()
                            };
                            stdout.push_str(&chunk);
                            conn_stdout_bytes = stdout.len();
                            if let Some(s) = sink {
                                if !chunk.is_empty() {
                                    s.push(&chunk).await;
                                }
                            }
                        }
                        STREAM_STDERR => {
                            let chunk = if conn_stderr_bytes < stderr.len() {
                                let skip = (stderr.len() - conn_stderr_bytes).min(payload.len());
                                String::from_utf8_lossy(&payload[skip..]).into_owned()
                            } else {
                                String::from_utf8_lossy(payload).into_owned()
                            };
                            stderr.push_str(&chunk);
                            conn_stderr_bytes = stderr.len();
                            if let Some(s) = sink {
                                if !chunk.is_empty() {
                                    s.push(&chunk).await;
                                }
                            }
                        }
                        STREAM_EXIT => {
                            exit_code = Some(decode_exit_code(payload));
                        }
                        _ => {} // stdin frames we don't receive.
                    }
                }
                Ok(Some(Ok(Message::Text(text)))) => {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                        match v.get("type").and_then(|t| t.as_str()) {
                            Some("session_info") => {
                                if let Some(id) = v.get("session_id").and_then(|i| i.as_str()) {
                                    session_id = Some(id.to_string());
                                }
                            }
                            Some("exit") => {
                                exit_code =
                                    Some(v.get("exit_code").and_then(|c| c.as_i64()).unwrap_or(0)
                                        as i32);
                            }
                            _ => {}
                        }
                    }
                }
                Ok(Some(Ok(Message::Close(_)))) => {
                    if exit_code.is_some() {
                        break;
                    }
                    if let Some(sid) = session_id.clone() {
                        tracing::warn!(
                            "sprite exec WSS closed before exit; reattaching to session {}",
                            sid
                        );
                        match self
                            .connect_wss(&format!(
                                "{}/sprites/{}/exec/{}",
                                WSS_API,
                                url_encode(name),
                                url_encode(&sid)
                            ))
                            .await
                        {
                            Ok(new_ws) => {
                                ws = new_ws;
                                conn_stdout_bytes = 0;
                                conn_stderr_bytes = 0;
                                continue;
                            }
                            Err(e) => {
                                tracing::error!("sprite exec reattach failed: {}", e);
                                break;
                            }
                        }
                    } else {
                        break;
                    }
                }
                Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => {
                    // tokio-tungstenite handles pings automatically; no action.
                    continue;
                }
                Ok(Some(Ok(Message::Frame(_)))) => {
                    continue;
                }
                Ok(Some(Err(e))) => {
                    if exit_code.is_some() {
                        break;
                    }
                    if let Some(sid) = session_id.clone() {
                        tracing::warn!(
                            "sprite exec WSS error ({:?}); reattaching to session {}",
                            e,
                            sid
                        );
                        match self
                            .connect_wss(&format!(
                                "{}/sprites/{}/exec/{}",
                                WSS_API,
                                url_encode(name),
                                url_encode(&sid)
                            ))
                            .await
                        {
                            Ok(new_ws) => {
                                ws = new_ws;
                                conn_stdout_bytes = 0;
                                conn_stderr_bytes = 0;
                                continue;
                            }
                            Err(re_e) => {
                                return Err(anyhow!(
                                    "sprite exec WSS error: {} (reattach failed: {})",
                                    e,
                                    re_e
                                ));
                            }
                        }
                    } else {
                        return Err(anyhow!("sprite exec WSS error: {}", e));
                    }
                }
                Ok(None) => {
                    if exit_code.is_some() {
                        break;
                    }
                    if let Some(sid) = session_id.clone() {
                        tracing::warn!(
                            "sprite exec WSS stream ended before exit; reattaching to session {}",
                            sid
                        );
                        match self
                            .connect_wss(&format!(
                                "{}/sprites/{}/exec/{}",
                                WSS_API,
                                url_encode(name),
                                url_encode(&sid)
                            ))
                            .await
                        {
                            Ok(new_ws) => {
                                ws = new_ws;
                                conn_stdout_bytes = 0;
                                conn_stderr_bytes = 0;
                                continue;
                            }
                            Err(e) => {
                                return Err(anyhow!(
                                    "sprite exec ended before exit (reattach failed: {})",
                                    e
                                ));
                            }
                        }
                    } else {
                        return Err(anyhow!("sprite exec ended before exit (no session id)"));
                    }
                }
                Err(_) => {
                    if Instant::now() > deadline {
                        let _ = ws.close(None).await;
                        return Err(anyhow!("sprite exec timed out after {}s", timeout_secs));
                    }
                    continue;
                }
            }

            if exit_code.is_some() {
                break;
            }
        }

        let _ = ws.close(None).await;
        Ok(ExecOutput {
            stdout,
            stderr,
            exit_code: exit_code.unwrap_or(0),
        })
    }

    /// Establish a WSS connection with the bearer auth header.
    async fn connect_wss(&self, url: &str) -> Result<WsStream> {
        let mut req = url.into_client_request().context("building WSS request")?;
        req.headers_mut()
            .insert("Authorization", format!("Bearer {}", self.token).parse()?);
        if let Some(org) = &self.org {
            req.headers_mut().insert("X-Sprites-Org", org.parse()?);
        }
        let (ws, _resp) = connect_async(req)
            .await
            .context("connecting to Sprites WSS")?;
        Ok(ws)
    }
}

/// URL-encode a single path/query segment using the `urlencoding` crate.
fn url_encode(s: &str) -> String {
    urlencoding::encode(s).to_string()
}

/// Single-quote a path for safe use in a `cd <path>` shell prefix.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Build a query string from `Vec<(String, String)>` with repeated keys supported.
fn encode_query(params: &[(String, String)]) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{}={}", url_encode(k), url_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Decode the exit code from the binary exit frame's payload. The docs say "payload is exit
/// code as byte" but it may be a single byte (u8) or a 4-byte big-endian i32; handle both.
fn decode_exit_code(payload: &[u8]) -> i32 {
    match payload.len() {
        0 => 0,
        1 => payload[0] as i32,
        4 => i32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]),
        _ => payload[0] as i32,
    }
}
