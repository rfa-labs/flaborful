# Flaborful — Full End-to-End Testing Walkthrough (current)

A step-by-step manual test of the platform as a real user, covering everything added since
the original walkthrough:

- **Two-sided job board** — companies post jobs; professionals *apply* with their agent
  (the old "company picks an agent off the marketplace" flow is gone)
- **Owner-controlled IP sharing** — pick which knowledge-base items a company sees
- **Resume capture** in onboarding
- **Connections** — owner connects GitHub/Google once; company grants per job
- **Google Workspace docs** — agent creates formatted Docs in a company Shared Drive
- **Agent management** — selective retraining + delete-and-rebuild
- **Knowledge base** — timeline + per-item delete
- **Workflow execution management** — live DAG visualization, per-node inspector
- **Per-node chat + staged reruns** — talk to your agent about a stage, correct it, re-run
- **Side-effect gate** — reruns of write nodes (post comment, send email, create doc) pause
  for a reuse / update-in-place / post-fresh decision
- **Work-execution backends (NEW)** — the agent does *real* software-engineering work: a
  frontier coding agent clones the repo, edits, runs tests, commits, pushes, and opens a PR.
  Two pluggable backends — **Managed Agents** (Anthropic-hosted) and **Sprite** (your own
  Fly.io VM) — and a "taste tool" that lets the coding agent consult the professional's LoRA.

---

## SETUP

### Ports & topology (important — these changed)
- **Backend (Axum):** `http://localhost:8080`
- **Frontend (SvelteKit dev):** `http://localhost:5173`
- Vite proxies `/api/*` → `:8080` (see `web/vite.config.ts`), so the browser only ever
  talks to `:5173`.

### 1. Services

You need **Postgres** and **Qdrant** reachable via the URLs in `.env`.

```bash
# Postgres (any local instance or the docker setup); confirm DATABASE_URL in .env points at it.
# Qdrant local (optional — you can also use the cloud Qdrant already in .env):
docker run -p 6333:6333 -p 6334:6334 qdrant/qdrant
```

### 2. Migrations

Apply all migrations (through `0018_node_checkpoints` — `0015`–`0018` add the work-execution
backends, task sprites, the `sprite_env` test-env grant, and per-node checkpoints):

```bash
bash scripts/migrate.sh          # = sqlx migrate run --source migrations
# or:  cargo sqlx migrate run --source migrations
```

### 3. Run it

```bash
# terminal 1 — backend
cargo run                        # listens on 0.0.0.0:8080

# terminal 2 — frontend
cd web && npm install && npm run dev   # serves http://localhost:5173
```

### 4. `.env` — what you actually need for local testing

The platform is built to **degrade gracefully** so you can test the whole flow with almost
no external infra. Behavior when a key is missing:

| Missing env | Behavior in dev |
|---|---|
| `RUNPOD_API_KEY` | **LoRA training is skipped and the agent is auto-marked `ready`.** You do NOT need a GPU to test the full flow. |
| `SMTP_URL` | Email nodes log "would send …" and continue (no real email). |
| `SANDBOX_URL` | CodeExecution / TestRunner nodes skip and continue. |
| `BRAVE_API_KEY` / `TAVILY_API_KEY` | Web search returns empty and continues. |
| `CF_ACCOUNT_ID` / `CF_API_TOKEN` | LLM falls back to base Ollama model (no LoRA personalization). |
| `ANTHROPIC_API_KEY` | **Managed-Agents backend skips** (`AgentImplement` returns `status:"skipped"`, empty diff). It's the frontier coding model's key — required for any *real* AgentImplement run on either backend. |
| `SPRITES_TOKEN` (+ `SPRITES_ORG`) | **Sprite backend disabled** — any agent set to `sprite` transparently falls back to Managed, and the sprite-only nodes (`SpriteCommand`/`SpriteTest`/`GitCommit`/`GitPush`) report "no live sprite" and fail that node. Set it to actually exercise the self-hosted VM path. |

**Minimum to exercise the core flow (agent + jobs + tasks + review + reruns):**
`DATABASE_URL`, `QDRANT_URL`, `QDRANT_API_KEY`, `JWT_SECRET`, `CREDENTIAL_KEY`,
`OLLAMA_BASE_URL`, `OLLAMA_API_KEY`, `ORCHESTRATOR_MODEL`, `WORKER_MODEL`. (Inference needs
a working Ollama/Cloud endpoint — that's the one external dependency the agent genuinely
needs to produce output. Embeddings run **locally** via fastembed/AllMiniLM, so no
embedding API key is required.)

**Add per integration you want to test for real:**
- GitHub knowledge ingest + GitHub tool node → `GITHUB_CLIENT_ID/SECRET`,
  `GITHUB_OAUTH_REDIRECT_URI`
- Google Docs → `GOOGLE_CLIENT_ID/SECRET`, `GOOGLE_OAUTH_REDIRECT_URI`
- **Real coding work (AgentImplement / SWE nodes)** → `ANTHROPIC_API_KEY` (the frontier
  coding model). This alone gets you the **Managed Agents** backend end-to-end. The owner's
  GitHub connection (above) supplies the repo clone/push token, authored as the professional.
- **Self-hosted Sprite backend** → `SPRITES_TOKEN` (+ `SPRITES_ORG`) for the Fly.io Sprites
  API. The sprite also needs `ANTHROPIC_API_KEY` (passed through to drive the coding agent).

> **Taste tool note:** when the coding agent calls `consult_principal` (its "how would my
> principal do this?" tool), the backend answers from the professional's LoRA via
> `CF_ACCOUNT_ID`/`CF_API_TOKEN` (or the Ollama base model if those are unset). The Cloudflare
> token **never leaves the backend** — the agent only hits `POST /api/v1/mcp/taste/:token`
> with a short-lived per-task token. For Sprite runs, `API_BASE_URL` must be reachable *from
> the sprite* for that callback (see the ngrok note — same public-URL requirement as RunPod).

### 5. OAuth callbacks — NO ngrok needed

The GitHub/Google OAuth redirect URIs point at the **frontend** (`localhost:5173/auth/<p>/
callback`); that page then POSTs the code to the backend. Because the browser does the
redirect to `localhost`, OAuth works entirely locally. Just make sure the redirect URIs
registered in the GitHub OAuth App / Google Cloud Console **exactly** match:

```
GITHUB_OAUTH_REDIRECT_URI=http://localhost:5173/auth/github/callback
GOOGLE_OAUTH_REDIRECT_URI=http://localhost:5173/auth/google/callback
```

For Google: enable **Google Docs API** + **Google Drive API**, and add the scopes
`documents` + `drive.file`. For the Shared Drive test you also need a Google Workspace
(not a personal gmail) and a Shared Drive you can add a member to.

### 6. ngrok — ONLY for the RunPod LoRA webhook

The single thing that needs a public URL is the **RunPod training webhook**: RunPod's cloud
worker calls back to `${API_BASE_URL}/api/v1/onboarding/lora/complete` when training
finishes. Your local `:8080` isn't reachable from the internet, so:

```bash
ngrok http 8080
# then in .env:
API_BASE_URL=https://<your-id>.ngrok-free.app
RUNPOD_API_KEY=...        # and RUNPOD_ENDPOINT_ID, R2_* for adapter storage
```
Restart `cargo run` after editing `.env`. **Skip this entirely** unless you specifically
want to test real GPU training — without `RUNPOD_API_KEY` the agent goes `ready` instantly.

> Tip: ngrok is also handy if you want to test inbound webhooks from GitHub/Slack later,
> but none of the flows below require it except RunPod.

> **Sprite backend also needs a public `API_BASE_URL`.** The coding agent on the sprite calls
> back to `${API_BASE_URL}/api/v1/mcp/taste/:token` for the taste tool, so when testing the
> **Sprite** backend (not Managed) `API_BASE_URL` must resolve from the VM — reuse the same
> ngrok tunnel. The **Managed Agents** backend does *not* need this: it handles the taste
> custom-tool over Anthropic's own session event stream, so its callback stays server-side.

### 7. Two browser profiles

Use two profiles (or one normal + one incognito): **Professional** and **Company**. Session
JWTs live in `localStorage` per profile.

---

## PART 1 — PROFESSIONAL: Build Your Agent

### Step 1 — Register
`http://localhost:5173/auth/register?as=professional` → name, email, password → **Create
account**. JWT is stored; you land on `/studio/onboarding`.

### Step 2 — Onboarding: Identity (+ Resume — NEW)
Fill name, bio, hourly rate, years. **New:** paste a **resume** into the Resume field — it's
stored as a *shareable* `resume` knowledge chunk and becomes the default resume on job
applications. Click **Continue →**. (`POST /professionals`, `POST /agents`, then
`POST /knowledge/agent/:id/resume`.)

### Step 3 — Onboarding: GitHub Knowledge
Click **Connect GitHub** → authorize on GitHub → redirected to
`/auth/github/callback` → back to onboarding, which runs `POST /onboarding/github/ingest`.
You'll see "✓ Style guide extracted". (Skippable if you don't have the GitHub OAuth app —
you can proceed with just the interview.)
> NEW (2026-06-22): the style guide + per-aspect insights are now written as **vault notes**
> (`style_rule` / `architecture`), and the request also **spawns background agentic ingestion**
> that documents your top repos into the vault (needs `ANTHROPIC_API_KEY`). See **Step VG-2** for
> how to verify the deep repo→vault notes.

### Step 4 — Onboarding: AI Interview
Work through the scenarios (pick an option + write a critique each). **Finish interview** →
`POST /onboarding/interview/submit` → training is triggered. The captured preferences are now
written as `style_rule` vault notes (still feeding LoRA preference pairs).
> NEW (2026-06-22): there's also a **slim, open-form** interview — fewer, open-ended questions about
> communication & reasoning style (`…/interview/questions/:profession` →
> `…/interview/open/submit`). See **Step VG-4**.

### Step 5 — Agent goes Ready
On `/studio`: with **no RUNPOD_API_KEY**, the agent is **already `ready`** (green banner).
With RunPod, it shows `training` until the webhook fires; you can also click **Mark as
ready** to force it.

### Step 6 — Connected accounts (NEW)
On `/studio`, find the **Connected accounts** card in the sidebar.
- **GitHub** shows Connected (from Step 3).
- Click **Connect** on **Google Workspace** → Google OAuth → back to studio showing your
  Google email. (Needed later for the Google Docs tool.) `GET /auth/connections/mine`
  backs this card.

### Step 7 — Build a workflow with a review gate
`/studio/workflows` → create one (use a template or add nodes). For a good end-to-end test,
build: **Trigger → LLM Reasoning → Human Gate → Formatter → Output**.
- On the LLM node, set a prompt (e.g. "Review this and give structured feedback:
  {{input}}").
- **To test Google Docs:** add an **LLM Reasoning** node with **"Author a document"**
  toggled on (it emits structured doc JSON), then a **Google Doc** node after it.
- **Save** (`PUT /workflows/:id`).

> NEW vs old: the engine now **continues past the Human Gate** after approval (it used to
> just deliver the draft). Downstream nodes (Formatter, Output, Google Doc…) actually run.

### Step 8 — Knowledge vault: IP sharing + timeline + delete (NEW)
`/studio/knowledge`:
- Each chunk has a **🔒 Private / 🔓 Shareable** toggle — only *shareable* chunks can be
  attached to applications. Mark a couple shareable.
- **Edit your resume** in the resume editor (re-saves the resume — now a `resume`-slug vault note).
- Click **Timeline** to see the add/delete history (`/knowledge/agent/:id/timeline`).
- Click **Delete** on a chunk to remove it (records a `deleted` timeline event).
> NEW (2026-06-22): chunks are now **derived from the `vault_notes` source of truth** (note id ==
> chunk id), so all of the above still works unchanged. See **PART 6.5** for the note-level
> endpoints (`/notes`, `/note/:slug`, `/graph`, `/reindex`) and the graph view.

---

## PART 2 — COMPANY: Post a Job, Review Applicants, Hire

Switch to the **Company** profile.

### Step 9 — Register + company profile
`/auth/register?as=company` → land on `/hire` → fill **Set up your company** → **Create
company profile** (`POST /companies`).

### Step 10 — Post a job (NEW model)
On `/hire` click **Post a job** → `/hire/jobs/new`:
- Title, profession, description, **requirements/tasks**, hourly rate → **Post job →**.
- The job is created **with no agent**, status **`reviewing`** (accepting applications).
  `POST /jobs` (the server ignores any client `company_id` and uses *your* company).

> The marketplace (`/marketplace`) still exists as a **read-only talent directory** — its
> "Post a job" button just routes here. There is no direct "hire this agent" anymore.

---

## PART 3 — PROFESSIONAL: Apply to the Job

Switch back to the **Professional** profile.

### Step 11 — Browse the job board (NEW)
`/studio` sidebar → **Job board** (`/studio/jobs/board`). You'll see the company's open
posting. Click it → `/studio/jobs/board/[id]`.

### Step 12 — Apply (NEW)
On the apply page:
- Write a **cover letter** (required).
- The **resume** is pre-filled from your saved resume; edit if you like.
- Optionally paste a **self-intro video URL**.
- **Work samples to share:** only your *shareable* KB chunks appear; tick the ones to
  expose. The selection is frozen as a snapshot for this application.
- **Submit application** (`POST /jobs/:id/apply`). Your cover letter is also ingested into
  your KB as an `application_material` chunk.

---

## PART 4 — COMPANY: Review Applicants, Interview, Select

Switch to the **Company** profile → open the job (`/hire/jobs/[id]`). It opens on the
**Applicants** tab (NEW).

### Step 13 — Review the applicant pool
Each applicant card shows name, profession, stats, and **View application** expands to the
cover letter, resume, video link, and the **shared work samples** (only what the owner
chose). Optionally **Shortlist**.
> NEW (2026-06-22): the expanded application now has a **Knowledge vault** section — click
> **Show graph →** to render the applicant's vault as an interactive graph (`GET
> /applications/:id/vault-graph`). It's **IP-safe**: shared notes are colored by layer and
> **openable** (click a node to read the full note); every other note is an anonymized **🔒 Private
> note #n** placeholder so you see how rich/connected their knowledge base is *without* leaking
> private titles or bodies. Shared work samples are now shown **in full** (click **Read full** to
> expand) — no more truncated snippets.

### Step 14 — Optional interview (NEW)
In an expanded applicant, paste a **Zoom/Meet link** + time → **Set interview**
(`PUT /applications/:id/meeting`). Status → `interviewing`.

### Step 15 — Hire (NEW selection model)
Click **Hire this agent** on the chosen applicant (`POST /applications/:id/select`). This
**sets the job's agent**, rejects the others, ingests the winning resume into the agent's
KB, and moves the job into the normal trial flow. The page flips to the **Tasks** tab.

### Step 16 — Grant tool access (Connections model — NEW)
**Integrations** tab → **+ Connect tool**:
- **GitHub:** the UI shows the **agent owner's connected GitHub login** — you don't paste a
  token. It tells you to add that login as a collaborator on your repos. Click **Grant
  access** (links the owner's connection to the job).
- **Google Workspace (Docs/Drive):** shows the owner's Google account. Enter your **Shared
  Drive ID** (from the Drive URL) and optional folder id, after adding the owner's Google
  account as a Content Manager on that Shared Drive. **Grant access.**
- **Slack/etc.:** still the paste-a-token path.

`GET /jobs/:id/agent-connections` powers the owner-account display;
`POST /jobs/:id/integrations` records the grant (with `connection_id` for github/google).

### Step 17 — Trial → Activate
Header buttons: **Start trial** (`/trial`) → **Activate agent** (`/activate`). All job
mutation endpoints now enforce that you own the job (403 otherwise).

### Step 18 — Create a task
**Tasks** tab → **+ New task** → description + pick your **workflow** → **Create**
(`POST /tasks`). The TaskRunner dispatches it immediately.

---

## PART 5 — Watch Execution + Steer the Agent (NEW: the big one)

### Step 19 — Company watches task status
`/hire/jobs/[id]` Tasks tab streams updates via SSE (`/events/tasks/:job_id`):
`pending → running → awaiting_review` (at the Human Gate). Confidence badge appears.

### Step 20 — Professional: execution DAG + per-node inspector (NEW)
Switch to **Professional** → `/studio` sidebar → **Executions**
(`/studio/executions`). Open the running task → `/studio/executions/[id]`:
- A **read-only DAG** of the workflow, nodes colored by status: **green** done, **amber**
  current/awaiting, **gray** pending, **red** failed. Live-updates via
  `/events/node-executions/:task_id`.
- **Click any node** → inspector shows its **input**, **output**, and **logs** (these are
  now persisted per node — they weren't before).
> NEW (2026-06-22): node **input/output** now render with **typed views** instead of a raw JSON
> blob — an **Agent: Implement** / **Test Gate** output shows a **colorized, collapsible diff**
> (per-file, +/− counts), a markdown **summary**, a **files-changed** list, and a toggle for the
> agent **transcript**; a **Test Gate** shows a **✓/✗ pass-fail** badge + attempt count; a write
> node shows its status + a link to the real artifact (PR/doc). Anything unrecognized falls back to
> pretty JSON. (Assert the diff is readable and the summary renders — that was the unusable
> one-blob view before.)

### Step 21 — Per-node chat + staged correction (NEW)
In the inspector for a completed node, click **💬 Chat about this work**:
- Ask the agent to revise (e.g. "make the tone more concise"). It replies grounded in that
  node's input/output + persona/LoRA + RAG. (`POST /node-chat/:task/:node`)
- Edit the **Corrected output** box → **Apply correction (stage)**
  (`POST /node-chat/:task/:node/apply`). The node + everything downstream are marked
  **dirty** (● on the DAG). A `ChatCorrection` chunk is added to your KB.

### Step 22 — Rerun from a node (NEW)
On a dirty node, click **Rerun from here ↓** (`POST /tasks/:id/rerun/:node_id`). The engine
rebuilds context from persisted outputs and re-runs that node + downstream with your
correction. Watch the DAG re-light via SSE.

### Step 23 — Review the Human Gate (the classic HITL)
`/studio` review queue → **Review** → `/studio/review/[taskId]`:
- Approve unchanged → `POST /tasks/:id/approve {amended:false}` → **workflow continues past
  the gate** and runs downstream nodes to delivery.
- Edit then approve → `{amended:true}` → an `rlhf_event` is logged; the amended value flows
  downstream.

### Step 24 — Side-effect gate on rerun (NEW — the idempotency safety)
This is the headline new safety feature. To see it:
1. Use a workflow whose downstream includes a **write** node — GitHub `post_comment`,
   Slack `post`, **Google Doc create**, Jira/Linear comment, Notion write, or Email send.
2. Let the task run through it once (the write happens; the node's output records a
   `resource_id`). For Google Docs, a real doc appears in the Shared Drive.
3. Go back upstream, **chat-correct** a node and **Rerun from here** so the rerun reaches
   the already-done write node.
4. The task **pauses** at that node (`awaiting_effect`; node rings **orange**). The
   inspector shows: **Reuse** (keep existing, don't redo), **Update existing** (edit the
   real artifact in place — e.g. overwrite the Google Doc / edit the PR comment), **Post
   fresh** (new one). Email shows no Update (can't unsend).
5. Pick **Update existing** → confirm the external artifact was edited, not duplicated.
   (`POST /tasks/:id/node/:node_id/effect-decision {decision}`)

> Without this gate, a rerun would post duplicate comments / send duplicate emails / create
> duplicate docs. Edit-in-place works for GitHub, Slack, Notion, Jira, Linear, and Google
> Docs; Email is reuse-or-refire only.

---

## PART 6 — Work Execution: Real SWE Work (NEW — the second big one)

This is the flow where the agent actually **does and verifies software-engineering work**
rather than just generating text. The DAG does coarse orchestration + HITL; a real frontier
coding agent owns the inner read→edit→test→fix loop, consulting the professional's LoRA via
the **taste tool**. Pick a backend per agent, build an SWE workflow, run it against a repo.

> **Prereqs for a *real* run:** `ANTHROPIC_API_KEY` set (coding model). The professional must
> have **connected GitHub** (Part 1, Step 6) and the company must have **granted the GitHub
> tool** on the job (Part 4, Step 16) — that supplies the clone/push token. Without
> `ANTHROPIC_API_KEY`, `AgentImplement` returns `status:"skipped"` (empty diff) so you can
> still walk the wiring, just with no real edits.

### Step 25 — Choose the agent's execution backend (Professional)
`/studio/agent` → the **Execution** card:
- **Managed (hosted)** — default, least setup. Runs Claude Code on Anthropic's container; the
  repo token rides Anthropic's git-proxy and never touches a VM. Needs only `ANTHROPIC_API_KEY`.
- **Sprite (your own VM)** — needs `SPRITES_TOKEN`. Reveals a **Coding agent runtime** picker:
  **Claude Code** / **Codex** / **Custom**. For **Custom**, a JSON **runtime spec** editor
  appears (`install_cmds`, `mcp_config_path`, `mcp_config_template`, `invoke_template` with
  `{taste_url}`/`{repo_dir}`/`{task}`/`{persona}` placeholders).
- **Save** → `PATCH /agents/:id/execution`. (If you pick Sprite but `SPRITES_TOKEN` is unset,
  runs transparently fall back to Managed.)

### Step 26 — (Sprite only) Company declares the work environment & access
Company profile → `/hire/jobs/[id]` → **Integrations** tab → **Work environment & access**
card. This is the *same kind of access you'd give a human contractor* — it controls how the
sprite is provisioned so the agent can do and verify the work. Pick an **Access type**:
- **None** — repo + public dependencies only, no extra provisioning.
- **Compose** — `docker compose up -d` the repo's deps (optional non-default compose file).
- **Cloud account** — scoped AWS/GCP/Azure credentials (an IAM user/role you'd issue a
  contractor), entered as `KEY=value`. Exported as env on the sprite.
- **Private network (VPN)** — reach internal/non-public services by joining the company's
  network with the **VPN profile the company already issues** (see VPN sub-steps below).
- **Egress allowlist** — restrict the sprite's outbound network to an allowlist of hosts.

For **Cloud account** / **Compose**, env vars are `KEY=value` per line; values whose names
contain KEY/TOKEN/SECRET/PASSWORD/AUTHKEY are **encrypted** (stored as the `sprite_env` job
integration's credentials), the rest are plain `scope_config`. **Save** → stored as
`tool_type='sprite_env'`. Only the Sprite backend consumes it; Managed Agents ignores it.

**VPN sub-steps (Private network):** pick the **VPN type**:
- **WireGuard** — paste the `wg0.conf` (stored encrypted as `secrets.wg_conf`).
- **OpenVPN** — paste the `.ovpn` profile (+ optional username/password). Stored encrypted as
  `secrets.ovpn_profile` / `vpn_username` / `vpn_password`.

The profile is held in the grant's **encrypted credentials**, never in `scope_config`, and is
cleared from the form after save. On provisioning, the sprite installs the client, writes the
profile, brings the tunnel up, and runs a **reachability check** before the agent starts.

> **Hard rule — VPN and Egress are mutually exclusive.** A VPN runs over UDP; applying *any*
> network/egress policy makes the sprite's egress proxy block **all** UDP, which would kill the
> tunnel. So a `network` (VPN) job **never** gets an egress allowlist applied — the backend skips
> it (and logs a warning) even if both are somehow set. Pick one.

### Step 27 — Build an SWE workflow
`/studio/workflows` → new workflow. The palette now has a **Software Engineering** group:
- **Agent: Implement** — the coarse node. Config: the **task** (or `task_var`), **repo**
  (`owner/name`, else inherited from `ctx.vars.repo`), base/work branch, an optional
  **Backend override**, and `output_var` (default `implementation`). Outputs `{diff, branch,
  files_changed, transcript}`; the full agent transcript streams into the node logs.
- **Run Command** / **Run Tests** — execute on the sprite (`SpriteCommand`/`SpriteTest`).
- **Git Commit** / **Git Push** — `GitCommit` (local, idempotent) / `GitPush` (side-effecting).
- **Create PR** — `PrCreate`, opens a GitHub PR from the pushed branch via the GitHub API
  (**no VM needed** — works even after the sprite is destroyed).

A good end-to-end graph:
**Trigger → Agent: Implement → Run Tests → Human Gate → Git Commit → Git Push → Create PR**.
Add a Human Gate after Implement so you review the diff before anything is pushed. **Save.**

> The sprite-only nodes (Run Command/Tests, Git Commit/Push) require a **live sprite for the
> task** — that sprite is created lazily by the first **Agent: Implement** node on the Sprite
> backend. Put an Agent: Implement node upstream of them. On the **Managed** backend the sprite
> nodes have no VM and will fail; use Managed for an `AgentImplement → … → PrCreate` graph that
> reads the branch from the implement node instead.

### Step 28 — Create a task against a real repo
Company → **Tasks** tab → **+ New task**: describe the change (e.g. "add a `/healthz` endpoint
returning 200 OK"), set the **repo** in the workflow's Agent: Implement node (or pass it as a
task var), pick the SWE workflow → **Create**. The TaskRunner dispatches it.

### Step 29 — Watch the coding agent work
Professional → `/studio/executions/[id]`:
- **Agent: Implement** node goes amber, then its **logs** stream the coding agent's transcript
  live (the read→edit→test→fix loop). If the agent calls **`consult_principal`**, that round-
  trips to your LoRA — you'll see the question + the LoRA's answer in the transcript. (Assert
  this appears to confirm the taste tool works.)
- On the **Sprite** backend, the early logs show `[sprite] creating …`, `[sprite] repo cloned`,
  any `[sprite] env setup: docker compose …`, then `[sprite] running coding agent…`.
- The node output carries the **diff** + changed files + work branch.

> **Testing VPN access (Private network).** Declare a `network` grant (Step 26) with a real
> **WireGuard** `wg0.conf` (or **OpenVPN** `.ovpn`) for a peer/host you control. On the run, the
> `[sprite] env setup:` logs show the client install + `wg-quick up` (or `openvpn`) + the
> **reachability check**. Verify with a downstream **Run Command** node hitting an internal-only
> host (`curl http://10.x.x.x/…`): it **succeeds** with the grant and **fails** without it. A
> bad/expired profile makes the node **fail loudly** at the reachability check — it does not
> silently continue. (Requires `SPRITES_TOKEN`; no extra env var — the profile comes from the
> company form.)

> **Testing Egress allowlist.** Declare an `egress` grant with an allowlist. The backend calls
> the real Sprites policy API `POST /v1/sprites/{name}/policy/network` with DNS allow-rules plus a
> trailing `deny *`. Verify with a **Run Command** `curl`: an **allowlisted** host succeeds, a
> **non-allowlisted** host is **REFUSED** (DNS blocked). Don't combine this with a VPN job (see
> the hard rule in Step 26).

### Step 30 — Review the diff at the Human Gate, then push + PR
- Open the **Human Gate** in the review queue → inspect the diff → **Approve** (or edit).
- The workflow continues: **Git Commit** (commits authored as the professional —
  `Flaborful Agent <agent@flaborful.com>`), **Git Push** (pushes the branch; side-effecting),
  **Create PR** → a **real PR appears** on GitHub. Its output records `pr_url`/`pr_number` and
  `resource_kind:"github_pr"`.

### Step 31 — Sprite teardown
On task terminal (delivered/failed) the task's sprite is destroyed automatically; **Complete
job** and the **Kill switch** (Integrations tab) also tear down every live sprite for the job
immediately (the kill switch is the safety net — sprites may hold cloud creds / a live VPN
tunnel). With `SPRITES_TOKEN` unset there's nothing to tear down.

### Step 32 — Rerun safety: side-effect gate + filesystem reconciliation
Two distinct rerun safeties for SWE work:
- **Side-effect gate on `GitPush`/`PrCreate`:** chat-correct an upstream node and **Rerun from
  here** so the rerun reaches an already-done **Git Push** or **Create PR**. The task pauses
  (`awaiting_effect`). For a **PR**, you get **Reuse / Update existing** (edits the PR title/
  body in place) **/ Post fresh**. For a **Git Push**, there's **no Update** — a re-push is
  reuse-or-refire (a branch push isn't an editable artifact). See Part 5, Step 24 for the UI.
- **Filesystem reconciliation (Sprite only):** rerunning a sprite-mutating node
  (`AgentImplement`/`SpriteCommand`/`GitCommit`/`GitPush`) **resets the working tree first** so
  edits aren't double-applied. It restores that node's pre-run Fly **checkpoint** if available
  (precise), else does `git reset --hard <clone HEAD> && git clean -fdx` (coarse). To verify:
  rerun an upstream-corrected Agent: Implement node, then confirm `git status` is clean and the
  final diff isn't a doubled version of the prior attempt's.

---

## PART 6.5 — Knowledge Vault + Hybrid Agent (NEW — 2026-06-22)

The professional's KB is now a **markdown-source-of-truth vault** (`vault_notes` in Postgres;
Qdrant is a derived index), the coding agent gets a **browsable vault tool** + an injected
**Execution Blueprint**, and a **Test Gate** bounces failing PRs back to the builder before a human
sees them. These flows exercise all of it. (Migrations `0022`, `0023` — run `cargo sqlx migrate run`.)

### Step VG-1 — Vault is populated as notes (not raw chunks)
After onboarding (Steps 3–4), open the Knowledge vault UI (or `GET /api/v1/knowledge/agent/:id/notes`).
You should see **typed notes** — `style_rule` (interview/style guide), `architecture` (per-aspect),
plus whatever the ingestion run wrote. Each note maps 1:1 to a Qdrant chunk (same id), so the
existing timeline + IP-sharing snapshot still resolve. Verify a `knowledge_events` "added" row
exists per note (`GET …/timeline`).

### Step VG-2 — Agentic code ingestion (GitHub → vault)
On **GitHub Knowledge** (Step 3), after the style-guide seed the backend **spawns a background
ingestion run per repo** (top ~5) on the **Managed Agents** backend. Requires `ANTHROPIC_API_KEY`
+ the GitHub connection. To verify:
- Watch the server logs for `vault ingestion …`; the agent clones each repo and calls
  `upsert_note` repeatedly.
- After it finishes, `GET …/notes` shows **feature / pattern / tech / golden_snippet** notes plus a
  per-repo **MOC**, with `related` links wiring them together and `domain`/`repo` set.
- Open `GET …/agent/:id/graph` → a `{nodes, edges}` payload (the Obsidian-style graph feed: nodes
  from notes, edges from `related[]`). The studio graph view renders it.
- Without `ANTHROPIC_API_KEY` the run is a no-op (skipped) — the style-guide seed notes still exist.

### Step VG-3 — Human edits a note → index re-derives
`PUT /api/v1/knowledge/agent/:id/note/:slug` (or the vault UI) editing a note's body. Confirm:
the row updates, the derived Qdrant chunk re-embeds automatically (search picks up the new text),
and a timeline event is recorded. This is the gold-standard-link path. `POST …/agent/:id/reindex`
rebuilds the whole index from the vault (and **backfills** legacy Qdrant-only agents into notes
first — verify chunk-count parity in the `{backfilled, reindexed}` response).

### Step VG-4 — Slim, open-form interview
`GET /api/v1/onboarding/interview/questions/:profession?aspect=<slug>` returns a **small** set of
**open-ended** questions about communication & reasoning style (3 aspect-scoped / 5 overall) — not
the A/B/C battery. Answer them and `POST …/interview/open/submit` → each answer becomes a
`style_rule` vault note (verify in `…/notes`). The legacy scenario battery still works and still
feeds LoRA preference pairs.

### Step VG-5 — Execution Blueprint injected + escalation-only consult
Create an SWE task (Part 6) for an agent whose vault has style/architecture/golden notes. In the
**Agent: Implement** node logs, confirm:
- The builder's prompt carries a **`# Execution Blueprint`** section (style rules + architecture map
  + golden snippets) distilled from the vault — assert it appears.
- `consult_principal` is now called **only for genuine ambiguity** (not every decision) — far fewer
  round-trips than before. The taste tool itself is unchanged.

### Step VG-6 — Vault browse tools mid-task
During the same run, the builder can call the `vault` MCP tools (`search_vault`, `read_note`,
`follow_links`, `list_vault`, `upsert_note`). On **Managed Agents** these are native custom tools;
on **Sprite** they're an MCP server registered next to `principal` (the `vault` server in
`.mcp.json` / Codex `config.toml`). Verify the transcript shows a `[search_vault]` / `[read_note]`
call grounding the work, and any `upsert_note` writes appear in `…/notes`.

### Step VG-7 — Test Gate (the gatekeeper)
Build `Trigger → Agent: Implement → **Test Gate** → Human Gate → … → Create PR`. Configure the
Test Gate node: `test_command` (e.g. `npm test` / `cargo test`), optional `lint_command`,
`max_retries` (default 2). Create a task whose change is likely to break a test, then watch:
- The gate runs the tests on the backend (same sprite the builder used).
- **On failure** it **loops back to the builder** with the failing output (each fix attempt is a new
  `node_executions` attempt row, visible in the DAG) — up to `max_retries`.
- **On pass** it proceeds to the Human Gate. If retries are exhausted, the **task fails** with the
  last failing output — a human **never** sees a red PR.
- (Sprite only) Rerunning the gate reconciles the working tree first (it's now treated as
  sprite-mutating).

### Step VG-8 — Rolling MEMORY.md harvested into the vault
On the **Sprite** backend, the builder is told to keep a `MEMORY.md` at the repo root. After
**Agent: Implement** completes, `harvest_memory_note` reads it and writes a `note`-layer vault note
(`source_type=approved_output`, slug `memory-<task>`). Verify a `MEMORY.md` note appears in
`…/notes` after a sprite run that produced one. (Managed Agents has no readable FS here, so this is
a Sprite-path check.)

> **Smoke check (vault, no external writes):** register pro → onboard (interview only, skip GitHub)
> → `…/notes` shows `style_rule` notes → build `Trigger → LLM → Human Gate → Output` → confirm the
> LLM node's RAG now pulls from the vault via hybrid retrieval (`vault::retrieve`). No
> GitHub/Anthropic needed for the storage + retrieval + interview paths; ingestion + blueprint +
> test gate need `ANTHROPIC_API_KEY` (Managed) or `SPRITES_TOKEN` (Sprite).

---

## PART 7 — Agent Management (NEW)

`/studio` sidebar → **Agent settings** (`/studio/agent`):

### Step 33 — Selective retraining
- Tick which **knowledge sources** to train on (resume, portfolio, approved outputs, **chat
  corrections**, job context, application materials, interview notes).
- **Preview dataset** (`/agents/:id/retrain/preview`) lists exactly which examples would be
  used — no training kicked off.
- **Start retraining** (`/agents/:id/retrain` with the filter). Without RunPod it completes
  instantly back to `ready`.

### Step 34 — Delete & rebuild (soft-reset)
**Danger zone → Delete & rebuild** → confirm. This wipes the LoRA (R2 files + training
fields), sets status back to `onboarding`, and sends you to onboarding — **the knowledge
base is preserved** (same agent id). Verify in the Knowledge vault that your chunks survived,
then re-run onboarding to rebuild a fresh adapter.

---

## PART 8 — Billing & Payout

### Step 35 — Generate invoice (Company)
`/hire/jobs/[id]` → **Billing** tab → **Generate this week** (`POST /billing/generate`).
Invoice row appears (gross, 15% fee, pro payout).

### Step 36 — Payout (Professional)
`/studio/payouts` → this week's payout entry (pending until an admin marks the invoice paid
via `POST /billing/:invoice_id/pay`).

---

## Status progressions (current)

- **Agent:** `onboarding → training → ready → deployed` (`suspended` available;
  `deleted_at` set on soft-reset)
- **Job:** `draft → reviewing → open → trial → active → completed` (`reviewing` = accepting
  applications; `open` = applicant selected)
- **Application:** `applied → shortlisted → interviewing → selected | rejected | withdrawn`
- **Task:** `pending → running → awaiting_review → awaiting_effect → approved/amended →
  delivered` (`awaiting_effect` = paused at a side-effect gate on rerun)
- **Node execution:** `running → completed | failed | awaiting_review | awaiting_effect |
  stale`
- **Agent execution config:** `execution_backend` ∈ `managed | sprite`; `agent_runtime` ∈
  `claude_code | codex | custom` (Sprite only); optional `runtime_spec` JSON for `custom`
- **Task sprite:** `creating → running → suspended → destroyed | gone` (`gone` = reaped by Fly;
  recreated + re-cloned on next use)
- **Access type / test-env strategy** (company `sprite_env` grant): `none | compose | cloud |
  network | egress`. `network` = a real corporate VPN (`scope_config.network.provider` ∈
  `openvpn | wireguard`; profile in encrypted credentials). `network` and `egress` are **mutually
  exclusive** (a network policy blocks all UDP and would break the VPN tunnel).

## Authorization note
Every owner/company-scoped endpoint now enforces resource ownership (`src/services/authz.rs`).
If you cross profiles (e.g. company A opening company B's job, or a pro touching another
agent's KB) you'll get **403 Forbidden** — that's expected.

## Smoke-test shortcut (no integrations)
For the fastest confidence check, you can exercise everything except the real external
writes with just DB + Qdrant + Ollama:
register pro → onboard (skip GitHub, just interview) → ready → build a
`Trigger→LLM→HumanGate→Formatter→Output` workflow → register company → post job → apply →
hire → create task → watch the DAG → chat-correct a node → rerun → approve the gate →
deliver. No GitHub/Google/RunPod/ngrok required.

**Real-coding smoke test (Managed backend):** the lightest way to see actual SWE work is the
**Managed Agents** backend — it needs only `ANTHROPIC_API_KEY` (plus the GitHub connection +
job grant for a real repo). Leave the agent's backend on **Managed**, build
`Trigger → Agent: Implement → **Test Gate** → Human Gate → Create PR`, create a task naming a repo
you can push to, and confirm: the implement node's logs stream a transcript carrying an injected
**`# Execution Blueprint`**, a `consult_principal` call round-trips to the LoRA **only on real
ambiguity** (escalation-only), the builder may call the **`vault` tools**, the **Test Gate** runs
the project's tests (looping failures back to the builder before the human gate), the diff shows at
the gate, and approving opens a real PR. No `SPRITES_TOKEN`/ngrok needed (Managed keeps the taste +
vault callbacks server-side). Add `SPRITES_TOKEN` + a public `API_BASE_URL` only to exercise the
self-hosted Sprite path, the sprite-only nodes, and the **MEMORY.md harvest** (Sprite-only).

---

## New-feature → key-files map

| Area | Key files |
|---|---|
| Connections (GitHub/Google OAuth, per-owner) | `src/routes/auth.rs`, `src/services/connections.rs`, migration `0008` |
| Job board + applications | `src/routes/jobs.rs`, `src/routes/applications.rs`, `src/services/applications.rs`, migration `0009` |
| KB visibility / resume / timeline / delete | `src/services/knowledge.rs`, `src/routes/knowledge.rs`, migration `0012` |
| Google Docs tool | `src/services/google_docs.rs`, `run_google_docs_create` in `src/services/workflow_engine.rs` |
| Resumable engine + node executions | `src/services/workflow_engine.rs` (`execute_graph`, `rebuild_context`, `resume_from`), migration `0010` |
| HITL continuation | `src/services/task_runner.rs` (`resume_after_approval`) |
| Execution DAG UI | `web/src/routes/studio/executions/`, SSE `src/routes/sse.rs` |
| Per-node chat + reruns | `src/services/node_chat.rs`, `src/routes/node_chat.rs`, migration `0013` |
| Side-effect gate + edit-in-place | `WorkflowNode::side_effect_kind`/`effect_supports_update` in `src/models/workflow.rs`, `apply_effect_update` in `src/services/workflow_engine.rs`, migration `0014` |
| Agent management (retrain filter, reset) | `src/services/lora_training.rs`, `src/routes/agents.rs`, `web/src/routes/studio/agent/`, migration `0011` |
| Authorization guards | `src/services/authz.rs` |
| **Work-execution backends** (trait, registry, TaskBrief) | `src/services/execution/mod.rs`, threaded via `src/state.rs` → `task_runner.rs` → `workflow_engine.rs::with_backends` |
| **Managed Agents backend** | `src/services/execution/managed_agents.rs` (needs `ANTHROPIC_API_KEY`) |
| **Sprite backend + runtimes** | `src/services/execution/sprite.rs`, `sprite_client.rs` (REST seam: exec/checkpoint/restore + `set_egress` → real `POST /v1/sprites/{name}/policy/network` DNS rules), `runtime/{claude_code,codex,custom}.rs` (needs `SPRITES_TOKEN`) |
| **Taste tool** (shared `consult_principal` → LoRA) | `src/services/execution/taste.rs`, route `src/routes/mcp_taste.rs` (`POST /api/v1/mcp/taste/:token`), `taste_tokens` table (migration `0015`) |
| **Knowledge vault (markdown SoT)** | `src/services/vault.rs` (CRUD, `index_note`/`reindex_agent`, `retrieve`, `backfill_from_qdrant`, R2 mirror, `handle_tool`), `vault_notes` table (migration `0022`), note/graph routes in `src/routes/knowledge.rs`; RAG switched in `workflow_engine.rs::retrieve_rag_context` |
| **Vault browse tools** (`list/read/search/follow_links/upsert_note`) | MCP server `src/routes/mcp_vault.rs` (`POST /api/v1/mcp/vault/:token`), shared `vault::handle_tool`; registered in `runtime/{claude_code,codex,custom}.rs` `mcp_config(taste_url, vault_url)` + native custom tools in `managed_agents.rs`; `TaskBrief::vault_url` |
| **Agentic code ingestion** (repos → vault) | `src/services/onboarding/ingest.rs` (`spawn_ingestion`/`ingest_repo`), wired in `src/routes/onboarding.rs::ingest_github`; agent-bound token `taste::mint_agent_token` (migration `0023` makes `taste_tokens.task_id` nullable) |
| **Execution Blueprint + escalation-only consult** | `src/services/blueprint.rs` (`build_blueprint`), injected in `workflow_engine.rs::run_agent_implement`; prompt changes in `runtime/claude_code.rs::build_prompt` + `managed_agents.rs` system prompt |
| **Test Gate** (tests/lint gate + builder-fix retries) | `NodeType::TestGate` in `src/models/workflow.rs`, `run_test_gate`/`run_gate_checks`/`run_builder_fix` in `src/services/workflow_engine.rs`, palette + config in `web/src/routes/studio/workflows/[id]/+page.svelte` |
| **Rolling MEMORY.md harvest** | `harvest_memory_note` in `src/services/workflow_engine.rs`; builder instruction in `runtime/claude_code.rs::build_prompt` |
| **Slim open-form interview** | `generate_open_questions`/`OpenQuestion`/`OpenAnswer` in `src/services/onboarding/interview.rs`, routes `get_open_questions`/`submit_open_interview` in `src/routes/onboarding.rs` |
| **SWE nodes** (`AgentImplement`/`PrCreate`/`SpriteCommand`/`SpriteTest`/`GitCommit`/`GitPush`) | `src/models/workflow.rs` (NodeType, `side_effect_kind`, `effect_supports_update`), bodies in `src/services/workflow_engine.rs`, palette in `web/src/routes/studio/workflows/[id]/+page.svelte` |
| **Access / test-env strategies** (company `sprite_env` grant: none/compose/cloud/network/egress) | `src/services/execution/sprite_env.rs` — VPN via `apply_openvpn`/`apply_wireguard`, profile blobs in encrypted creds (`VPN_SECRET_KEYS`: `ovpn_profile`/`wg_conf`/`vpn_username`/`vpn_password`); VPN×egress exclusion enforced in `sprite.rs::ensure_sprite`; "Work environment & access" form on `web/src/routes/hire/jobs/[id]/+page.svelte` Integrations tab; migration `0017` |
| **Rerun FS reconciliation** (checkpoint / git-reset) | `reconcile_sprite_filesystem` + `capture_node_checkpoint` in `src/services/workflow_engine.rs`, `task_sprites.head_sha` (migration `0016`), `node_executions.node_checkpoint_id` (migration `0018`) |
| Agent backend/runtime config | `agents.execution_backend`/`agent_runtime`/`runtime_spec` (migration `0015`), `PATCH /agents/:id/execution`, `web/src/routes/studio/agent/` |
