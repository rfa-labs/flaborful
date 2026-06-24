//! Test-environment strategies for the Sprite backend.
//!
//! The *company* declares, per job, how the agent's sprite should be provisioned so the work can
//! actually be verified — what deps to bring up, what the box may reach on the network, what cloud
//! creds it gets. This lives on a `job_integrations` row of `tool_type='sprite_env'`, reusing the
//! existing encrypted-creds + `scope_config` plumbing. Only the Sprite backend consumes it; Managed
//! Agents maps to CMA limited-networking + vault secrets instead.
//!
//! `scope_config` shape (all optional except `strategy`):
//! ```json
//! { "strategy": "none|compose|egress|network|cloud",
//!   "compose_file": "docker-compose.yml",        // compose
//!   "egress_urls": ["api.example.com", "..."],   // egress
//!   "env": { "AWS_REGION": "us-east-1", ... } }   // cloud / any: extra env exported in the sprite
//! ```
//! Secret values (cloud creds, a Tailscale auth key) live in the row's encrypted_credentials and
//! are merged into `env` at resolve time so they never touch `scope_config`/the graph.
//!
//! v1 implements `none`/`compose`/`egress` fully; `network`/`cloud` are thin (env-inject + one
//! command) but present in the schema/switch so the company form and provisioning don't need
//! re-architecture when they're fleshed out.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TestEnvStrategy {
    /// Unit tests only — no extra provisioning. The default.
    #[default]
    None,
    /// Bring up the repo's docker-compose dependencies before tests.
    Compose,
    /// Restrict the sprite's outbound network to an allowlist (Fly Sprites L3 egress).
    Egress,
    /// Join the company's private network (Tailscale ephemeral auth key).
    Network,
    /// Inject scoped cloud credentials as environment variables.
    Cloud,
}

impl TestEnvStrategy {
    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "none" => Some(Self::None),
            "compose" => Some(Self::Compose),
            "egress" => Some(Self::Egress),
            "network" => Some(Self::Network),
            "cloud" => Some(Self::Cloud),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Compose => "compose",
            Self::Egress => "egress",
            Self::Network => "network",
            Self::Cloud => "cloud",
        }
    }
}

/// A resolved, ready-to-apply provisioning plan for a sprite, derived from the company's
/// `sprite_env` grant. The Sprite backend runs `setup_cmds` (in the repo dir) after cloning,
/// exports `env` into every subsequent command, and applies `egress_allowlist` to the sprite's
/// network policy.
#[derive(Debug, Clone, Default)]
pub struct SpriteProvisioning {
    pub strategy: TestEnvStrategy,
    /// Commands to run in the repo dir after clone (e.g. `docker compose up -d`, `tailscale up`).
    pub setup_cmds: Vec<String>,
    /// Commands to run on teardown (e.g. `docker compose down -v`) — best-effort.
    pub teardown_cmds: Vec<String>,
    /// Env exported into the agent + test commands (cloud creds, region, etc.).
    pub env: Vec<(String, String)>,
    /// Outbound host allowlist for the `egress` strategy (empty = unrestricted).
    pub egress_allowlist: Vec<String>,
}

/// Build a provisioning plan from a job's `sprite_env` scope_config + decrypted secrets.
/// `secrets` is the decrypted credentials blob (may be empty); its keys are merged into `env`
/// so cloud creds / the Tailscale key never live in `scope_config`.
pub fn resolve(scope_config: &Value, secrets: &Value) -> SpriteProvisioning {
    let strategy = scope_config
        .get("strategy")
        .and_then(|v| v.as_str())
        .and_then(TestEnvStrategy::from_str_opt)
        .unwrap_or_default();

    // Env: scope_config.env (non-secret) then secrets (override / add sensitive values).
    // VPN profile blobs are NOT env vars — they're consumed by the Network arm, so skip them here.
    let mut env: Vec<(String, String)> = Vec::new();
    for source in [scope_config.get("env"), Some(secrets)] {
        if let Some(obj) = source.and_then(|v| v.as_object()) {
            for (k, v) in obj {
                if VPN_SECRET_KEYS.contains(&k.as_str()) {
                    continue;
                }
                if let Some(s) = v.as_str() {
                    env.push((k.clone(), s.to_string()));
                }
            }
        }
    }

    let mut p = SpriteProvisioning {
        strategy,
        env,
        ..Default::default()
    };

    match strategy {
        TestEnvStrategy::None => {}
        TestEnvStrategy::Compose => {
            let file = scope_config
                .get("compose_file")
                .and_then(|v| v.as_str())
                .unwrap_or("docker-compose.yml");
            // -f only if a non-default file was named; `docker compose` finds the default itself.
            let f = if file == "docker-compose.yml" {
                String::new()
            } else {
                format!("-f {} ", file)
            };
            p.setup_cmds.push(format!(
                "docker compose {f}up -d --wait || docker compose {f}up -d",
                f = f
            ));
            p.teardown_cmds
                .push(format!("docker compose {f}down -v", f = f));
        }
        TestEnvStrategy::Egress => {
            p.egress_allowlist = scope_config
                .get("egress_urls")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
        }
        TestEnvStrategy::Network => {
            // Join the company's network using the corporate VPN profile they already issue
            // (OpenVPN or WireGuard). The profile/creds are secret (in `secrets`), never env.
            //
            // IMPORTANT: a `network` job must run with NO network policy — Fly's egress proxy
            // blocks all UDP when a policy is active, which would kill the VPN tunnel. We leave
            // egress_allowlist empty here and the caller (sprite::ensure_sprite) additionally
            // refuses to apply egress for the Network strategy (defense in depth).
            let provider = scope_config
                .get("network")
                .and_then(|n| n.get("provider"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            match provider {
                "openvpn" => apply_openvpn(&mut p, secrets),
                "wireguard" => apply_wireguard(&mut p, secrets),
                other => tracing::warn!(
                    "network strategy with unsupported/empty provider {:?}",
                    other
                ),
            }
        }
        TestEnvStrategy::Cloud => {
            // Creds are already in `env` (merged from secrets); nothing else to run.
        }
    }

    p
}

/// Secret keys that carry VPN profile blobs (or auth) rather than env vars; excluded from the
/// env merge and consumed by the Network arm.
const VPN_SECRET_KEYS: &[&str] = &["ovpn_profile", "vpn_username", "vpn_password", "wg_conf"];

const VPN_DIR: &str = "/etc/flaborful";

/// Emit a setup command that writes `contents` to `path` on the sprite, base64-decoded so
/// multi-line PEM/conf blobs survive the single-command `exec` shape without quoting hazards.
fn write_file_cmd(path: &str, contents: &str) -> String {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(contents);
    format!(
        "sudo mkdir -p {dir} && echo {b64} | base64 -d | sudo tee {path} >/dev/null && sudo chmod 600 {path}",
        dir = VPN_DIR, b64 = b64, path = path,
    )
}

/// OpenVPN: write the `.ovpn` profile (+ optional auth file), install, connect as a daemon, then
/// verify the tunnel device appears (fail loudly otherwise).
fn apply_openvpn(p: &mut SpriteProvisioning, secrets: &Value) {
    let Some(profile) = secrets.get("ovpn_profile").and_then(|v| v.as_str()) else {
        tracing::warn!("openvpn network strategy missing secrets.ovpn_profile");
        return;
    };
    let cfg = format!("{}/vpn.ovpn", VPN_DIR);
    p.setup_cmds.push(write_file_cmd(&cfg, profile));

    let mut connect = format!(
        "sudo openvpn --config {cfg} --daemon --log {dir}/openvpn.log",
        cfg = cfg,
        dir = VPN_DIR
    );
    if let (Some(u), Some(pw)) = (
        secrets.get("vpn_username").and_then(|v| v.as_str()),
        secrets.get("vpn_password").and_then(|v| v.as_str()),
    ) {
        let auth = format!("{}/vpn.auth", VPN_DIR);
        p.setup_cmds
            .push(write_file_cmd(&auth, &format!("{}\n{}\n", u, pw)));
        connect.push_str(&format!(" --auth-user-pass {}", auth));
    }

    p.setup_cmds.push(
        "sudo apt-get update -qq && sudo apt-get install -y -qq openvpn iproute2".to_string(),
    );
    p.setup_cmds.push(connect);
    // Reachability: wait for a tun device to come up; fail the node if it never does.
    p.setup_cmds.push(
        "timeout 30 sh -c 'until ip link show | grep -q tun; do sleep 1; done' || { echo 'openvpn tunnel did not come up'; exit 1; }".to_string(),
    );
    p.teardown_cmds
        .push("sudo pkill openvpn || true".to_string());
}

/// WireGuard: write the `wg0.conf`, install, bring the interface up, then verify the peer is
/// configured (fail loudly otherwise).
fn apply_wireguard(p: &mut SpriteProvisioning, secrets: &Value) {
    let Some(conf) = secrets.get("wg_conf").and_then(|v| v.as_str()) else {
        tracing::warn!("wireguard network strategy missing secrets.wg_conf");
        return;
    };
    let cfg = format!("{}/wg0.conf", VPN_DIR);
    p.setup_cmds.push(write_file_cmd(&cfg, conf));
    p.setup_cmds.push(
        "sudo apt-get update -qq && sudo apt-get install -y -qq wireguard-tools iproute2"
            .to_string(),
    );
    p.setup_cmds.push(format!("sudo wg-quick up {}", cfg));
    p.setup_cmds.push(
        "timeout 30 sh -c 'until sudo wg show | grep -q peer; do sleep 1; done' || { echo 'wireguard tunnel did not come up'; exit 1; }".to_string(),
    );
    p.teardown_cmds
        .push(format!("sudo wg-quick down {} || true", cfg));
}

/// Load the job's active `sprite_env` grant and resolve it into a provisioning plan. Returns the
/// default (`none`) plan when the company declared no test-env strategy. Decryption failures
/// degrade to a plan without the secret env rather than failing the whole task.
pub async fn for_job(db: &PgPool, job_id: Uuid) -> SpriteProvisioning {
    let row = sqlx::query!(
        r#"SELECT scope_config, encrypted_credentials
           FROM job_integrations
           WHERE job_id = $1 AND tool_type = 'sprite_env' AND is_active = true
           ORDER BY created_at DESC LIMIT 1"#,
        job_id,
    )
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    let Some(row) = row else {
        return SpriteProvisioning::default();
    };

    let secrets = row
        .encrypted_credentials
        .as_deref()
        .and_then(|enc| crate::services::crypto::decrypt(enc).ok())
        .and_then(|pt| serde_json::from_str::<Value>(&pt).ok())
        .unwrap_or_else(|| Value::Object(Default::default()));

    resolve(&row.scope_config, &secrets)
}
