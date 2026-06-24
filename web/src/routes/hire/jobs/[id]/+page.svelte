<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { browser } from '$app/environment';
  import { page } from '$app/stores';
  import { goto } from '$app/navigation';
  import { api } from '$lib/api/client';
  import { getToken } from '$lib/stores/session';
  import type { Job, AuditEvent, Task, JobIntegration, ConnectionSummary, ApplicantView, GithubRepo, VaultGraph } from '$lib/api/types';
  import StatusBadge from '$lib/components/StatusBadge.svelte';
  import ConfidenceBadge from '$lib/components/ConfidenceBadge.svelte';
  import VaultGraphView from '$lib/components/VaultGraph.svelte';

  $: jobId = $page.params.id;

  let job: Job | null = null;
  let tasks: Task[] = [];
  let audit: AuditEvent[] = [];
  let loading = true;
  let error = '';
  let activeTab: 'applicants' | 'tasks' | 'integrations' | 'contract' | 'audit' | 'billing' = 'tasks';

  // Applicants (job board)
  let applicants: ApplicantView[] = [];
  let loadingApplicants = false;
  let expandedApplicant: string | null = null;
  let meetingDrafts: Record<string, { link: string; at: string }> = {};
  // Lazily-loaded applicant vault graphs (IP-safe) + which sample is expanded to full view.
  let vaultGraphs: Record<string, VaultGraph | 'loading' | 'error'> = {};
  let openSample: string | null = null;

  async function loadVaultGraph(appId: string) {
    if (vaultGraphs[appId] && vaultGraphs[appId] !== 'error') return;
    vaultGraphs = { ...vaultGraphs, [appId]: 'loading' };
    try {
      const g = await api.get<VaultGraph>(`/applications/${appId}/vault-graph`);
      vaultGraphs = { ...vaultGraphs, [appId]: g };
    } catch {
      vaultGraphs = { ...vaultGraphs, [appId]: 'error' };
    }
  }

  // Narrow a loaded graph (or null while pending/errored) for the template, avoiding inline casts.
  function graphOf(appId: string): VaultGraph | null {
    const g = vaultGraphs[appId];
    return g && g !== 'loading' && g !== 'error' ? g : null;
  }

  async function loadApplicants() {
    loadingApplicants = true;
    try {
      applicants = await api.get<ApplicantView[]>(`/jobs/${jobId}/applicants`);
    } catch { applicants = []; } finally { loadingApplicants = false; }
  }

  async function setApplicantStatus(appId: string, status: string) {
    await api.put(`/applications/${appId}/status`, { status });
    await loadApplicants();
  }

  async function setMeeting(appId: string) {
    const draft = meetingDrafts[appId];
    if (!draft?.link?.trim()) return;
    await api.put(`/applications/${appId}/meeting`, {
      meeting_link: draft.link.trim(),
      meeting_at: draft.at ? new Date(draft.at).toISOString() : null,
    });
    await loadApplicants();
  }

  async function selectApplicant(appId: string, name: string) {
    if (!confirm(`Hire ${name}'s agent for this job? This sets up the trial and rejects other applicants.`)) return;
    await api.post(`/applications/${appId}/select`, {});
    job = await api.get<Job>(`/jobs/${jobId}`);
    await loadApplicants();
    activeTab = 'tasks';
  }

  // Billing
  interface Invoice { id: string; week_start: string; week_end: string; tasks_delivered: number; hours_billed: number; gross_amount: number; platform_fee: number; net_payout: number; status: string; }
  let invoices: Invoice[] = [];
  let loadingBilling = false;

  // Integrations
  let integrations: JobIntegration[] = [];
  let loadingIntegrations = false;
  let showAddIntegration = false;
  let intTool = 'github';
  let intToken = '';
  let addingInt = false;
  let intError = '';

  // Connection-backed grants: github/google grant access through the agent owner's own
  // connected account rather than a pasted token.
  const CONNECTION_TOOLS: Record<string, 'github' | 'google'> = { github: 'github', google_drive: 'google' };
  let agentConnections: ConnectionSummary[] = [];

  // Company-side GitHub: the company connects its own GitHub to list owned repos (for task
  // targets) and to onboard the professional onto those repos as a collaborator.
  let companyGithub: { external_account: string } | null = null;
  let companyRepos: GithubRepo[] = [];
  let loadingRepos = false;
  let repoError = '';
  // The professional's GitHub login the company must add as a collaborator (from agentConnections).
  $: professionalGithub = agentConnections.find(c => c.provider === 'github')?.external_account ?? '';
  // Google-specific grant config
  let sharedDriveId = '';
  let parentFolderId = '';

  // Test-env strategy (Sprite backend): how the agent's machine is provisioned so its work can
  // be verified. Declared by the company, stored as a `sprite_env` integration.
  let testEnvStrategy = 'none';
  let composeFile = '';
  let egressUrls = '';
  let envVars = '';        // KEY=value per line; secrets ride encrypted_credentials.
  // VPN (network strategy): the corporate profile the company already issues to contractors.
  let vpnProvider = 'wireguard';   // 'openvpn' | 'wireguard'
  let ovpnProfile = '';
  let ovpnUsername = '';
  let ovpnPassword = '';
  let wgConf = '';
  let savingTestEnv = false;
  let testEnvError = '';
  let seededTestEnvFor = '';
  $: testEnvGrant = integrations.find(i => i.tool_type === 'sprite_env');
  // Seed the form from the existing grant once when it loads (scope_config is non-secret;
  // VPN profile blobs live in encrypted credentials and are never returned, so they stay blank).
  $: if (testEnvGrant && seededTestEnvFor !== testEnvGrant.id) {
    seededTestEnvFor = testEnvGrant.id;
    const sc = (testEnvGrant.scope_config ?? {}) as Record<string, unknown>;
    testEnvStrategy = (sc.strategy as string) ?? 'none';
    composeFile = (sc.compose_file as string) ?? '';
    egressUrls = Array.isArray(sc.egress_urls) ? (sc.egress_urls as string[]).join('\n') : '';
    const env = (sc.env as Record<string, string>) ?? {};
    envVars = Object.entries(env).map(([k, v]) => `${k}=${v}`).join('\n');
    const net = (sc.network as Record<string, unknown>) ?? {};
    vpnProvider = (net.provider as string) ?? 'wireguard';
  }

  async function saveTestEnv() {
    savingTestEnv = true;
    testEnvError = '';
    try {
      const scope_config: Record<string, unknown> = { strategy: testEnvStrategy };
      const secrets: Record<string, string> = {};

      if (testEnvStrategy === 'compose' && composeFile.trim()) scope_config.compose_file = composeFile.trim();
      if (testEnvStrategy === 'egress') {
        scope_config.egress_urls = egressUrls.split(/[\n,]/).map(s => s.trim()).filter(Boolean);
      }
      if (testEnvStrategy === 'network') {
        // The VPN profile + creds are secret → encrypted credentials, never scope_config.
        scope_config.network = { provider: vpnProvider };
        if (vpnProvider === 'openvpn') {
          if (!ovpnProfile.trim()) { testEnvError = 'Paste the .ovpn profile your company issues.'; return; }
          secrets.ovpn_profile = ovpnProfile;
          if (ovpnUsername.trim()) secrets.vpn_username = ovpnUsername.trim();
          if (ovpnPassword) secrets.vpn_password = ovpnPassword;
        } else {
          if (!wgConf.trim()) { testEnvError = 'Paste the WireGuard wg0.conf your company issues.'; return; }
          secrets.wg_conf = wgConf;
        }
      }

      // Non-secret env goes in scope_config.env; secret values go in encrypted credentials.
      // (Skipped for `network` — a VPN job uses its profile, not env vars.)
      if (testEnvStrategy !== 'network') {
        const env: Record<string, string> = {};
        for (const line of envVars.split('\n')) {
          const t = line.trim();
          if (!t || !t.includes('=')) continue;
          const idx = t.indexOf('=');
          const key = t.slice(0, idx).trim();
          const val = t.slice(idx + 1).trim();
          // Heuristic: KEY/TOKEN/SECRET/PASSWORD/AUTHKEY values are secret.
          if (/KEY|TOKEN|SECRET|PASSWORD|AUTHKEY|CREDENTIAL/i.test(key)) secrets[key] = val;
          else env[key] = val;
        }
        if (Object.keys(env).length) scope_config.env = env;
      }

      // Replace any existing grant (single test-env per job).
      if (testEnvGrant) await api.delete(`/jobs/${jobId}/integrations/${testEnvGrant.id}`);
      await api.post(`/jobs/${jobId}/integrations`, {
        tool_type: 'sprite_env',
        credentials: Object.keys(secrets).length ? secrets : undefined,
        scope_config,
      });
      ovpnProfile = ''; ovpnPassword = ''; wgConf = '';   // clear secrets from memory after save
      await loadIntegrations();
    } catch (e: unknown) {
      testEnvError = e instanceof Error ? e.message : 'Failed to save test environment';
    } finally {
      savingTestEnv = false;
    }
  }

  $: isConnectionTool = intTool in CONNECTION_TOOLS;
  $: requiredProvider = CONNECTION_TOOLS[intTool];
  $: matchingConnection = agentConnections.find(c => c.provider === requiredProvider);

  // Task creation
  let showNewTask = false;
  let taskDescription = '';
  let taskRepo = '';
  let taskBaseBranch = '';
  let creatingTask = false;
  let taskError = '';

  // Contract
  let contractMd = '';
  let loadingContract = false;

  // SSE task feed
  let taskEventSource: EventSource | null = null;

  onMount(async () => {
    try {
      [job, tasks] = await Promise.all([
        api.get<Job>(`/jobs/${jobId}`),
        api.get<Task[]>(`/tasks/job/${jobId}`),
      ]);
      // Returning from the company GitHub OAuth lands back on the Integrations tab.
      const tabParam = $page.url.searchParams.get('tab');
      if (tabParam === 'integrations') {
        setTab('integrations');
      } else if (job?.status === 'reviewing') {
        // A job still accepting applications opens straight to the applicant pool.
        activeTab = 'applicants';
        loadApplicants();
      }
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to load job';
    } finally {
      loading = false;
    }

    if (browser) {
      const token = getToken();
      const url = `/api/v1/events/tasks/${jobId}${token ? `?token=${token}` : ''}`;
      taskEventSource = new EventSource(url);
      taskEventSource.addEventListener('task_update', (e) => {
        const update = JSON.parse(e.data);
        const idx = tasks.findIndex(t => t.id === update.id);
        if (idx >= 0) {
          tasks[idx] = { ...tasks[idx], ...update };
          tasks = [...tasks];
        } else {
          // New task — refresh list
          api.get<Task[]>(`/tasks/job/${jobId}`).then(t => { tasks = t; });
        }
      });
    }
  });

  onDestroy(() => {
    taskEventSource?.close();
  });

  async function loadAudit() {
    if (audit.length) return;
    audit = await api.get<AuditEvent[]>(`/audit/job/${jobId}`);
  }

  async function loadContract() {
    loadingContract = true;
    try {
      const res = await api.get<{ contracts: Array<{ content: string }> }>(`/jobs/${jobId}/contract`);
      contractMd = res.contracts?.[res.contracts.length - 1]?.content ?? '';
    } catch {
      contractMd = '';
    } finally {
      loadingContract = false;
    }
  }

  async function loadIntegrations() {
    loadingIntegrations = true;
    try {
      [integrations, agentConnections] = await Promise.all([
        api.get<JobIntegration[]>(`/jobs/${jobId}/integrations`),
        api.get<ConnectionSummary[]>(`/jobs/${jobId}/agent-connections`).catch(() => [] as ConnectionSummary[]),
      ]);
      await loadCompanyGithub();
    } catch { integrations = []; } finally { loadingIntegrations = false; }
  }

  // Is the company's own GitHub connected? If so, load its owned repos.
  async function loadCompanyGithub() {
    try {
      const conns = await api.get<{ provider: string; external_account: string }[]>('/auth/connections/mine');
      const gh = conns.find(c => c.provider === 'github');
      companyGithub = gh ? { external_account: gh.external_account } : null;
      if (companyGithub) await loadCompanyRepos();
    } catch { companyGithub = null; }
  }

  async function loadCompanyRepos() {
    loadingRepos = true; repoError = '';
    try {
      companyRepos = await api.get<GithubRepo[]>('/auth/connections/github/repos');
    } catch (e: unknown) {
      repoError = e instanceof Error ? e.message : 'Failed to load repos';
    } finally { loadingRepos = false; }
  }

  // Kick off the company's GitHub OAuth (no agent_id — company connect). On return, the
  // callback page sends us back here via the stored return URL.
  async function connectCompanyGithub() {
    localStorage.setItem('gh_connect_return', `/hire/jobs/${jobId}?tab=integrations`);
    const { url } = await api.get<{ url: string }>('/auth/github/start');
    window.location.href = url;
  }

  // Open the repo's collaborator settings in a NEW tab so the company can add the professional.
  function inviteCollaborator(repo: GithubRepo) {
    window.open(`${repo.html_url}/settings/access`, '_blank', 'noopener');
  }

  // Reset the company's GitHub connection (e.g. connected the wrong account). Job grants that
  // used it will need to be re-granted.
  async function disconnectCompanyGithub() {
    if (!confirm('Disconnect your GitHub account? Any repo grants on this job that used it will need to be set up again.')) return;
    try {
      await api.delete('/auth/connections/github');
      companyGithub = null;
      companyRepos = [];
      taskRepo = '';
    } catch (e: unknown) {
      repoError = e instanceof Error ? e.message : 'Failed to disconnect';
    }
  }

  // When the company picks a repo for a task, default the base branch to that repo's default.
  function onTaskRepoSelect() {
    const repo = companyRepos.find(r => r.full_name === taskRepo);
    if (repo && !taskBaseBranch.trim()) taskBaseBranch = repo.default_branch;
  }

  async function addIntegration() {
    addingInt = true;
    intError = '';
    try {
      if (isConnectionTool) {
        // Grant access through the agent owner's connected account.
        if (!matchingConnection) {
          intError = `The agent's owner hasn't connected a ${requiredProvider} account yet.`;
          return;
        }
        const scope_config: Record<string, string> = {};
        if (intTool === 'google_drive') {
          if (!sharedDriveId.trim()) {
            intError = 'Shared Drive ID is required so the agent writes docs the company owns.';
            return;
          }
          scope_config.shared_drive_id = sharedDriveId.trim();
          if (parentFolderId.trim()) scope_config.parent_folder_id = parentFolderId.trim();
        }
        await api.post(`/jobs/${jobId}/integrations`, {
          tool_type: intTool,
          connection_id: matchingConnection.id,
          scope_config,
        });
      } else {
        // Raw-credential grant (company-provided token, e.g. Slack bot token).
        await api.post(`/jobs/${jobId}/integrations`, {
          tool_type: intTool,
          credentials: { token: intToken },
          scope_config: {},
        });
      }
      showAddIntegration = false;
      intToken = '';
      sharedDriveId = '';
      parentFolderId = '';
      await loadIntegrations();
    } catch (e: unknown) {
      intError = e instanceof Error ? e.message : 'Failed to grant access';
    } finally {
      addingInt = false;
    }
  }

  async function revokeIntegration(integrationId: string) {
    await api.delete(`/jobs/${jobId}/integrations/${integrationId}`);
    integrations = integrations.filter(i => i.id !== integrationId);
  }

  async function createTask() {
    if (!taskDescription.trim()) return;
    creatingTask = true; taskError = '';
    try {
      // The company sets the work-target (what to act on); the professional whose agent is on
      // this job assigns the workflow (how) later, which is what triggers execution.
      const target: Record<string, string> = {};
      if (taskRepo.trim()) target.repo = taskRepo.trim();
      if (taskBaseBranch.trim()) target.base_branch = taskBaseBranch.trim();
      await api.post('/tasks', {
        job_id: jobId,
        workflow_id: null,
        input: { description: taskDescription.trim() },
        target,
      });
      showNewTask = false;
      taskDescription = '';
      taskRepo = '';
      taskBaseBranch = '';
      tasks = await api.get<Task[]>(`/tasks/job/${jobId}`);
    } catch (e: unknown) {
      taskError = e instanceof Error ? e.message : 'Failed to create task';
    } finally {
      creatingTask = false;
    }
  }

  async function killSwitch() {
    if (!confirm('Revoke ALL agent integrations immediately? This cannot be undone.')) return;
    await api.put(`/jobs/${jobId}/kill`);
    goto('/hire');
  }

  async function transition(action: string) {
    job = await api.put<Job>(`/jobs/${jobId}/${action}`);
  }

  function setTab(key: string) {
    activeTab = key as typeof activeTab;
    if (key === 'billing' && invoices.length === 0) loadBilling();
    if (key === 'integrations' && integrations.length === 0) loadIntegrations();
    if (key === 'applicants') loadApplicants();
    // The task form's repo picker needs the company's repos too.
    if (key === 'tasks' && companyGithub === null && companyRepos.length === 0) loadCompanyGithub();
  }

  async function loadBilling() {
    loadingBilling = true;
    try {
      invoices = await api.get<Invoice[]>(`/billing/job/${jobId}`);
    } catch { invoices = []; } finally { loadingBilling = false; }
  }

  function timeAgo(iso: string) {
    const diff = Date.now() - new Date(iso).getTime();
    const m = Math.floor(diff / 60000);
    if (m < 2) return 'just now';
    if (m < 60) return `${m}m ago`;
    const h = Math.floor(m / 60);
    if (h < 24) return `${h}h ago`;
    return `${Math.floor(h / 24)}d ago`;
  }

  $: if (activeTab === 'audit') loadAudit();
  $: if (activeTab === 'contract') loadContract();
</script>

<svelte:head><title>{job?.title ?? 'Job'} — Hire Desk</title></svelte:head>

<div class="max-w-5xl mx-auto px-4 py-8">
  <button class="btn-ghost text-sm mb-6" on:click={() => goto('/hire')}>← Hire Desk</button>

  {#if loading}
    <div class="card p-8 animate-pulse"><div class="h-32 bg-gray-100 rounded"></div></div>
  {:else if error || !job}
    <div class="card p-8 text-center text-red-600">{error || 'Not found'}</div>
  {:else}
    <!-- Job header -->
    <div class="card p-6 mb-6">
      <div class="flex items-start justify-between gap-4">
        <div class="flex-1">
          <h1 class="text-2xl font-bold mb-1">{job.title}</h1>
          <p class="text-gray-500 text-sm mb-3">{job.description}</p>
          <div class="flex items-center gap-3 text-sm">
            <StatusBadge status={job.status} />
            <span class="text-gray-400">${(job.hourly_rate / 100).toFixed(0)}/hr</span>
            {#if job.started_at}
              <span class="text-gray-400">Started {new Date(job.started_at).toLocaleDateString()}</span>
            {/if}
          </div>
        </div>
        <!-- Status actions -->
        <div class="flex gap-2 shrink-0">
          {#if job.status === 'open'}
            <button class="btn-primary text-sm" on:click={() => transition('trial')}>Start trial</button>
          {:else if job.status === 'trial'}
            <button class="btn-primary text-sm" on:click={() => transition('activate')}>Activate agent</button>
          {:else if job.status === 'active'}
            <button class="btn-secondary text-sm" on:click={() => transition('complete')}>Complete job</button>
          {/if}
          <button class="btn-danger text-sm" on:click={killSwitch}>Kill switch</button>
        </div>
      </div>
    </div>

    <!-- Tabs -->
    <div class="flex gap-1 mb-6 bg-gray-100 rounded-lg p-1 w-fit">
      {#each [
        { key: 'applicants', label: 'Applicants' },
        { key: 'tasks', label: 'Tasks' },
        { key: 'integrations', label: 'Integrations' },
        { key: 'contract', label: 'Contract' },
        { key: 'billing', label: 'Billing' },
        { key: 'audit', label: 'Audit log' },
      ] as tab}
        <button
          class="px-4 py-1.5 rounded-md text-sm font-medium transition-colors {activeTab === tab.key ? 'bg-white shadow text-gray-900' : 'text-gray-500 hover:text-gray-700'}"
          on:click={() => setTab(tab.key)}>
          {tab.label}
        </button>
      {/each}
    </div>

    <!-- Applicants tab -->
    {#if activeTab === 'applicants'}
      {#if loadingApplicants}
        <div class="card p-8 animate-pulse"><div class="h-32 bg-gray-100 rounded"></div></div>
      {:else if applicants.length === 0}
        <div class="card p-8 text-center text-gray-400 text-sm">
          No applicants yet. Professionals who apply to this posting will appear here.
        </div>
      {:else}
        <div class="space-y-4">
          {#each applicants as a (a.application.id)}
            {@const app = a.application}
            <div class="card p-5">
              <div class="flex items-start gap-4">
                <div class="w-10 h-10 rounded-full bg-brand-100 text-brand-700 flex items-center justify-center font-semibold shrink-0">
                  {a.professional_name.charAt(0).toUpperCase()}
                </div>
                <div class="flex-1 min-w-0">
                  <div class="flex items-center gap-2 flex-wrap">
                    <span class="font-semibold">{a.professional_name}</span>
                    <span class="text-xs text-gray-400">{a.profession_name}</span>
                    <span class="badge-{app.status === 'selected' ? 'green' : app.status === 'rejected' ? 'red' : app.status === 'interviewing' ? 'blue' : 'gray'} text-xs capitalize">{app.status}</span>
                  </div>
                  <div class="text-xs text-gray-500 mt-1 flex items-center gap-3 flex-wrap">
                    {#if a.hourly_rate}<span>${(a.hourly_rate / 100).toFixed(0)}/hr</span>{/if}
                    {#if a.years_experience}<span>{a.years_experience}y exp</span>{/if}
                    <span>{a.completed_jobs} jobs done</span>
                    <span>{a.approved_outputs} approved outputs</span>
                  </div>
                  {#if a.bio}<p class="text-sm text-gray-600 mt-2">{a.bio}</p>{/if}
                </div>
                <div class="flex flex-col gap-1.5 shrink-0">
                  {#if app.status !== 'selected' && job.status === 'reviewing'}
                    <button class="btn-primary text-xs" on:click={() => selectApplicant(app.id, a.professional_name)}>Hire this agent</button>
                  {/if}
                  <button class="btn-ghost text-xs" on:click={() => expandedApplicant = expandedApplicant === app.id ? null : app.id}>
                    {expandedApplicant === app.id ? 'Hide details' : 'View application'}
                  </button>
                  {#if app.status === 'applied'}
                    <button class="text-xs text-gray-400 hover:text-gray-600" on:click={() => setApplicantStatus(app.id, 'shortlisted')}>Shortlist</button>
                  {/if}
                </div>
              </div>

              {#if expandedApplicant === app.id}
                <div class="mt-4 pt-4 border-t border-gray-100 space-y-4">
                  <!-- Cover letter -->
                  {#if app.cover_letter}
                    <div>
                      <div class="text-xs font-semibold text-gray-500 uppercase tracking-wide mb-1">Cover letter</div>
                      <p class="text-sm text-gray-700 whitespace-pre-wrap">{app.cover_letter}</p>
                    </div>
                  {/if}

                  <!-- Resume -->
                  {#if app.resume_text}
                    <div>
                      <div class="text-xs font-semibold text-gray-500 uppercase tracking-wide mb-1">Resume</div>
                      <p class="text-sm text-gray-700 whitespace-pre-wrap max-h-48 overflow-y-auto bg-gray-50 rounded p-2">{app.resume_text}</p>
                    </div>
                  {/if}

                  <!-- Self-intro video -->
                  {#if app.video_url}
                    <div>
                      <div class="text-xs font-semibold text-gray-500 uppercase tracking-wide mb-1">Self-intro video</div>
                      <a class="text-sm text-brand-600 hover:underline break-all" href={app.video_url} target="_blank" rel="noopener">{app.video_url}</a>
                    </div>
                  {/if}

                  <!-- Knowledge vault graph (IP-safe: shared notes openable, private ones locked) -->
                  <div>
                    <div class="flex items-center justify-between mb-1">
                      <div class="text-xs font-semibold text-gray-500 uppercase tracking-wide">Knowledge vault</div>
                      {#if !vaultGraphs[a.application.id]}
                        <button class="text-xs text-brand-600 hover:underline" on:click={() => loadVaultGraph(a.application.id)}>
                          Show graph →
                        </button>
                      {/if}
                    </div>
                    {#if vaultGraphs[a.application.id] === 'loading'}
                      <p class="text-xs text-gray-400">Loading graph…</p>
                    {:else if vaultGraphs[a.application.id] === 'error'}
                      <p class="text-xs text-red-400">Couldn't load the vault graph.</p>
                    {:else}
                      {@const g = graphOf(a.application.id)}
                      {#if g}
                        <VaultGraphView graph={g} height={360} />
                      {/if}
                    {/if}
                  </div>

                  <!-- Shared knowledge (owner-curated, IP-safe) — full content, not snippets -->
                  {#if a.shared_knowledge.length > 0}
                    <div>
                      <div class="text-xs font-semibold text-gray-500 uppercase tracking-wide mb-1">Shared work samples ({a.shared_knowledge.length})</div>
                      <div class="space-y-2">
                        {#each a.shared_knowledge as chunk}
                          {@const isOpen = openSample === chunk.id}
                          <div class="bg-gray-50 rounded p-2 text-sm text-gray-700">
                            <div class="flex items-center justify-between mb-0.5">
                              <span class="text-[10px] uppercase tracking-wide text-gray-400">{chunk.source_type.replace(/_/g, ' ')}</span>
                              <button class="text-[11px] text-brand-600 hover:underline"
                                on:click={() => openSample = isOpen ? null : chunk.id}>
                                {isOpen ? 'Collapse' : 'Read full'}
                              </button>
                            </div>
                            <pre class="whitespace-pre-wrap break-words font-sans {isOpen ? '' : 'line-clamp-6'} {isOpen ? 'max-h-[28rem] overflow-y-auto' : ''}">{chunk.content}</pre>
                          </div>
                        {/each}
                      </div>
                    </div>
                  {:else}
                    <p class="text-xs text-gray-400">This applicant didn't attach work samples.</p>
                  {/if}

                  <!-- Interview scheduling -->
                  <div>
                    <div class="text-xs font-semibold text-gray-500 uppercase tracking-wide mb-1">Interview (optional)</div>
                    {#if app.meeting_link}
                      <div class="text-sm text-gray-700 mb-2">
                        Scheduled: <a class="text-brand-600 hover:underline break-all" href={app.meeting_link} target="_blank" rel="noopener">{app.meeting_link}</a>
                        {#if app.meeting_at}<span class="text-gray-400"> · {new Date(app.meeting_at).toLocaleString()}</span>{/if}
                      </div>
                    {/if}
                    <div class="flex flex-wrap gap-2 items-end">
                      <div class="flex-1 min-w-[200px]">
                        <input class="input text-sm" placeholder="Paste Zoom/Meet link"
                          value={meetingDrafts[app.id]?.link ?? ''}
                          on:input={(e) => meetingDrafts = { ...meetingDrafts, [app.id]: { ...(meetingDrafts[app.id] ?? { link: '', at: '' }), link: e.currentTarget.value } }} />
                      </div>
                      <div>
                        <input class="input text-sm" type="datetime-local"
                          value={meetingDrafts[app.id]?.at ?? ''}
                          on:input={(e) => meetingDrafts = { ...meetingDrafts, [app.id]: { ...(meetingDrafts[app.id] ?? { link: '', at: '' }), at: e.currentTarget.value } }} />
                      </div>
                      <button class="btn-secondary text-sm" on:click={() => setMeeting(app.id)}>Set interview</button>
                    </div>
                  </div>
                </div>
              {/if}
            </div>
          {/each}
        </div>
      {/if}

    <!-- Tasks tab -->
    {:else if activeTab === 'tasks'}
      <div class="mb-4 flex justify-end">
        <button class="btn-primary text-sm" on:click={() => { showNewTask = !showNewTask; }}>
          + New task
        </button>
      </div>

      {#if showNewTask}
        <div class="card p-5 mb-4 space-y-3">
          <h3 class="font-semibold text-sm">New task</h3>
          <div>
            <label class="label text-xs" for="task-desc">Description</label>
            <textarea class="input min-h-[80px] resize-none text-sm" id="task-desc"
              bind:value={taskDescription}
              placeholder="Describe what the agent should do for this task..." />
          </div>
          <div class="grid grid-cols-2 gap-3">
            <div>
              <label class="label text-xs" for="task-repo">Repo (optional)</label>
              {#if companyRepos.length > 0}
                <select class="input text-sm" id="task-repo" bind:value={taskRepo} on:change={onTaskRepoSelect}>
                  <option value="">— No repo —</option>
                  {#each companyRepos as repo}
                    <option value={repo.full_name}>{repo.full_name}</option>
                  {/each}
                </select>
              {:else}
                <input class="input text-sm" id="task-repo" bind:value={taskRepo}
                  placeholder="owner/name" />
                {#if !companyGithub}
                  <p class="text-xs text-gray-400 mt-1">
                    Connect your GitHub in the <button type="button" class="text-brand-600 underline"
                      on:click={() => setTab('integrations')}>Integrations</button> tab to pick from your repos.
                  </p>
                {/if}
              {/if}
            </div>
            <div>
              <label class="label text-xs" for="task-base">Base branch (optional)</label>
              <input class="input text-sm" id="task-base" bind:value={taskBaseBranch}
                placeholder="main" />
            </div>
          </div>
          <p class="text-xs text-muted">
            The professional whose agent is hired will assign a workflow to run this task.
          </p>
          {#if taskError}
            <p class="text-red-500 text-sm">{taskError}</p>
          {/if}
          <div class="flex gap-2">
            <button class="btn-primary text-sm" disabled={!taskDescription.trim() || creatingTask} on:click={createTask}>
              {creatingTask ? 'Creating...' : 'Create task'}
            </button>
            <button class="btn-ghost text-sm" on:click={() => { showNewTask = false; taskError = ''; }}>Cancel</button>
          </div>
        </div>
      {/if}

      <div class="card">
        {#if tasks.length === 0}
          <div class="p-8 text-center text-gray-400 text-sm">No tasks yet. Create one above.</div>
        {:else}
          <table class="w-full text-sm">
            <thead class="bg-gray-50 text-xs text-gray-500 uppercase tracking-wide">
              <tr>
                <th class="px-5 py-3 text-left">Status</th>
                <th class="px-5 py-3 text-left">Created</th>
                <th class="px-5 py-3 text-left">Confidence</th>
                <th class="px-5 py-3 text-left">Delivered</th>
                <th class="px-5 py-3"></th>
              </tr>
            </thead>
            <tbody class="divide-y divide-gray-100">
              {#each tasks as t}
                <tr class="hover:bg-gray-50 transition-colors">
                  <td class="px-5 py-3"><StatusBadge status={t.status} /></td>
                  <td class="px-5 py-3 text-gray-500">{timeAgo(t.created_at)}</td>
                  <td class="px-5 py-3"><ConfidenceBadge score={t.confidence_score} /></td>
                  <td class="px-5 py-3 text-gray-500">
                    {t.delivered_at ? timeAgo(t.delivered_at) : '—'}
                  </td>
                  <td class="px-5 py-3 text-right">
                    {#if t.status === 'delivered' && t.output_approved}
                      <button class="btn-ghost text-xs">View output</button>
                    {/if}
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      </div>

    <!-- Integrations tab -->
    {:else if activeTab === 'integrations'}
      <div class="space-y-4">
        <div class="card p-5">
          <div class="flex items-center justify-between mb-4">
            <h2 class="font-semibold">Tool integrations</h2>
            <button class="btn-secondary text-sm" on:click={() => showAddIntegration = !showAddIntegration}>
              + Connect tool
            </button>
          </div>

          {#if showAddIntegration}
            <div class="bg-gray-50 rounded-lg p-4 mb-4 space-y-3">
              <div>
                <label class="label text-xs" for="tool">Tool</label>
                <select class="input text-sm" id="tool" bind:value={intTool} on:change={() => intError = ''}>
                  {#each ['github', 'google_drive', 'slack', 'jira', 'linear', 'notion', 'email', 'custom_api'] as t}
                    <option value={t}>{t === 'google_drive' ? 'Google Workspace (Docs/Drive)' : t.replace(/_/g, ' ')}</option>
                  {/each}
                </select>
              </div>

              {#if isConnectionTool}
                <!-- Connection-backed grant: the agent acts as its owner. The company
                     grants access to the owner's account rather than pasting a token. -->
                {#if matchingConnection}
                  <div class="flex items-center gap-2 bg-white border border-gray-200 rounded-lg px-3 py-2.5 text-sm">
                    <span class="w-2 h-2 rounded-full bg-green-400 shrink-0"></span>
                    <span class="text-gray-700">Agent's {requiredProvider} account:</span>
                    <span class="font-medium text-gray-900 truncate">{matchingConnection.external_account}</span>
                  </div>

                  {#if intTool === 'github'}
                    <div class="text-xs text-gray-500 bg-blue-50 border border-blue-100 rounded-lg p-3 leading-relaxed">
                      <p class="font-medium text-blue-800 mb-1">To grant access:</p>
                      Add <span class="font-mono text-blue-700">{matchingConnection.external_account}</span> as a
                      collaborator (or org member) on the repos the agent should work in, on GitHub.
                      The agent commits and opens PRs as that user — you review and merge.
                    </div>
                  {:else if intTool === 'google_drive'}
                    <div class="text-xs text-gray-500 bg-blue-50 border border-blue-100 rounded-lg p-3 leading-relaxed">
                      <p class="font-medium text-blue-800 mb-1">To grant access:</p>
                      In Google Drive, open your <strong>Shared Drive</strong> → Manage members → add
                      <span class="font-mono text-blue-700">{matchingConnection.external_account}</span> as
                      <strong>Content manager</strong>. Docs the agent creates are owned by your organization,
                      not the agent's owner.
                    </div>
                    <div>
                      <label class="label text-xs" for="drive-id">Shared Drive ID</label>
                      <input class="input text-sm font-mono" id="drive-id" bind:value={sharedDriveId}
                        placeholder="0AHk...VA" />
                      <p class="text-xs text-gray-400 mt-1">From the Shared Drive URL: drive.google.com/drive/folders/<strong>&lt;this part&gt;</strong></p>
                    </div>
                    <div>
                      <label class="label text-xs" for="folder-id">Target folder ID <span class="text-gray-400 font-normal">(optional)</span></label>
                      <input class="input text-sm font-mono" id="folder-id" bind:value={parentFolderId}
                        placeholder="defaults to drive root" />
                    </div>
                  {/if}
                {:else}
                  <div class="text-sm text-amber-700 bg-amber-50 border border-amber-200 rounded-lg px-3 py-2.5">
                    The agent's owner hasn't connected a {requiredProvider} account yet.
                    They need to connect it from their Agent Studio before you can grant access.
                  </div>
                {/if}
              {:else}
                <!-- Raw-credential grant: company provides its own token. -->
                <div>
                  <label class="label text-xs" for="token">
                    {#if intTool === 'slack'}Slack Bot Token (starts with xoxb-)
                    {:else}API token / credential{/if}
                  </label>
                  <input class="input text-sm font-mono" id="token" type="password" bind:value={intToken}
                    placeholder={intTool === 'slack' ? 'xoxb-...' : 'token...'} />
                  {#if intTool === 'slack'}
                    <p class="text-xs text-gray-400 mt-1">Create a Slack app at api.slack.com, add chat:write scope, and install to your workspace</p>
                  {/if}
                </div>
              {/if}

              {#if intError}
                <p class="text-red-500 text-sm">{intError}</p>
              {/if}

              <div class="flex gap-2">
                <button class="btn-primary text-sm"
                  disabled={addingInt || (isConnectionTool ? !matchingConnection : !intToken)}
                  on:click={addIntegration}>
                  {addingInt ? 'Granting...' : isConnectionTool ? 'Grant access' : 'Save'}
                </button>
                <button class="btn-ghost text-sm" on:click={() => { showAddIntegration = false; intError = ''; }}>Cancel</button>
              </div>
            </div>
          {/if}

          <!-- Active integrations list -->
          {#if loadingIntegrations}
            <div class="animate-pulse h-10 bg-gray-100 rounded"></div>
          {:else if integrations.length > 0}
            <div class="divide-y divide-gray-100 mb-4">
              {#each integrations as int}
                <div class="flex items-center justify-between py-3">
                  <div class="flex items-center gap-3">
                    <span class="w-2 h-2 rounded-full bg-green-400"></span>
                    <span class="text-sm font-medium capitalize">{int.tool_type.replace(/_/g, ' ')}</span>
                    <span class="text-xs text-gray-400">connected {new Date(int.created_at).toLocaleDateString()}</span>
                  </div>
                  <button class="text-xs text-red-500 hover:text-red-700" on:click={() => revokeIntegration(int.id)}>
                    Revoke
                  </button>
                </div>
              {/each}
            </div>
          {:else}
            <p class="text-sm text-gray-400 mb-4">No tools connected yet.</p>
          {/if}

          <p class="text-xs text-gray-400">
            Credentials are encrypted with AES-256-GCM and scoped exclusively to this job.
          </p>
        </div>

        <!-- Repository access: the company connects its OWN GitHub to list its repos, then adds
             the professional (the agent's owner) as a collaborator on the repos this hire needs.
             The agent commits/opens PRs as the professional — so it must be a collaborator. -->
        <div class="card p-5">
          <h2 class="font-semibold mb-1">Repository access</h2>
          <p class="text-sm text-gray-500 mb-4">
            Connect your GitHub to list your repos, then add the hired professional as a
            collaborator on the ones this agent will work in. The agent commits and opens PRs as
            that user — you review and merge.
          </p>

          {#if !companyGithub}
            <button class="btn-secondary text-sm" on:click={connectCompanyGithub}>
              Connect your GitHub
            </button>
          {:else}
            <div class="flex items-center gap-2 bg-white border border-gray-200 rounded-lg px-3 py-2.5 text-sm mb-3">
              <span class="w-2 h-2 rounded-full bg-green-400 shrink-0"></span>
              <span class="text-gray-700">Your GitHub:</span>
              <span class="font-medium text-gray-900">{companyGithub.external_account}</span>
              <button class="ml-auto text-xs text-red-500 hover:text-red-700 shrink-0"
                on:click={disconnectCompanyGithub}>
                Disconnect
              </button>
            </div>

            {#if professionalGithub}
              <p class="text-xs text-gray-500 mb-3">
                Add <span class="font-mono text-gray-800">{professionalGithub}</span>
                (the hired professional) as a collaborator on each repo below.
              </p>
            {:else}
              <p class="text-xs text-amber-700 bg-amber-50 border border-amber-200 rounded px-3 py-2 mb-3">
                The hired professional hasn't connected their GitHub yet — they need to do that in
                their Agent Studio before you can add them as a collaborator.
              </p>
            {/if}

            {#if loadingRepos}
              <div class="animate-pulse h-10 bg-gray-100 rounded"></div>
            {:else if repoError}
              <p class="text-red-500 text-sm">{repoError}</p>
            {:else if companyRepos.length === 0}
              <p class="text-sm text-gray-400">
                No repos found. If your repos live in an organization, make sure you granted this
                app access to that org when connecting (re-connect to re-show the org grant).
              </p>
            {:else}
              <div class="divide-y divide-gray-100 border border-gray-100 rounded-lg">
                {#each companyRepos as repo}
                  <div class="flex items-center justify-between py-2.5 px-3 gap-3">
                    <div class="min-w-0">
                      <span class="text-sm font-mono truncate">{repo.full_name}</span>
                      {#if repo.private}<span class="text-xs text-gray-400 ml-2">private</span>{/if}
                      {#if !repo.can_admin}<span class="text-xs text-amber-600 ml-2">no admin</span>{/if}
                    </div>
                    <button class="btn-ghost text-xs shrink-0"
                      disabled={!professionalGithub || !repo.can_admin}
                      title={!repo.can_admin ? 'You need admin on this repo to add collaborators' : ''}
                      on:click={() => inviteCollaborator(repo)}>
                      Add collaborator ↗
                    </button>
                  </div>
                {/each}
              </div>
              <p class="text-xs text-gray-400 mt-2">
                Opens the repo's GitHub access settings in a new tab. Add the professional there,
                then come back — the agent can target the repo on a task. Repos marked
                <span class="text-amber-600">no admin</span> can't be managed with your account.
              </p>
              <p class="text-xs text-gray-400 mt-1">
                Missing an org's repos? <button type="button" class="text-brand-600 underline"
                  on:click={connectCompanyGithub}>Re-connect GitHub</button> and grant this app
                access to that organization on the consent screen.
              </p>
            {/if}
          {/if}
        </div>

        <!-- Test environment (Sprite backend): how the agent's machine is provisioned so its
             work can actually be verified. Only consumed when the agent runs on the Sprite backend. -->
        <div class="card p-5">
          <h2 class="font-semibold mb-1">Work environment & access</h2>
          <p class="text-sm text-gray-500 mb-4">
            How the agent's machine is set up so it can do and verify the work — the same kinds of
            access you'd give a contractor. Used when this job's agent runs on a managed sprite.
          </p>

          <div class="space-y-3">
            <div>
              <label class="label text-xs" for="testenv-strategy">Access type</label>
              <select class="input text-sm" id="testenv-strategy" bind:value={testEnvStrategy}>
                <option value="none">None — repo + public dependencies only</option>
                <option value="compose">Docker Compose — bring up the repo's dependencies</option>
                <option value="cloud">Cloud account — scoped AWS/GCP/Azure credentials</option>
                <option value="network">Private network (VPN) — reach internal services</option>
                <option value="egress">Egress allowlist — restrict outbound network</option>
              </select>
            </div>

            {#if testEnvStrategy === 'compose'}
              <div>
                <label class="label text-xs" for="compose-file">Compose file (optional)</label>
                <input class="input text-sm font-mono" id="compose-file" bind:value={composeFile}
                  placeholder="docker-compose.yml" />
                <p class="text-xs text-gray-400 mt-1">Runs <span class="font-mono">docker compose up -d</span> after clone.</p>
              </div>
            {:else if testEnvStrategy === 'egress'}
              <div>
                <label class="label text-xs" for="egress-urls">Allowed hosts</label>
                <textarea class="input text-sm font-mono h-20" id="egress-urls" bind:value={egressUrls}
                  placeholder="api.example.com&#10;registry.npmjs.org"></textarea>
                <p class="text-xs text-gray-400 mt-1">One host per line. The sprite can reach only these; everything else is blocked.</p>
              </div>
            {:else if testEnvStrategy === 'cloud'}
              <p class="text-xs text-gray-500 bg-blue-50 border border-blue-100 rounded-lg p-3">
                Give the agent the scoped cloud credentials you'd issue a contractor (e.g. an IAM
                user/role). Enter them below as <span class="font-mono">KEY=value</span> — they're
                exported as environment on the sprite.
              </p>
            {:else if testEnvStrategy === 'network'}
              <!-- Corporate VPN: the company hands over the profile it already issues. -->
              <div>
                <label class="label text-xs" for="vpn-provider">VPN type</label>
                <select class="input text-sm" id="vpn-provider" bind:value={vpnProvider}>
                  <option value="wireguard">WireGuard (.conf)</option>
                  <option value="openvpn">OpenVPN (.ovpn)</option>
                </select>
              </div>
              {#if vpnProvider === 'wireguard'}
                <div>
                  <label class="label text-xs" for="wg-conf">WireGuard config (wg0.conf)</label>
                  <textarea class="input text-sm font-mono h-28" id="wg-conf" bind:value={wgConf}
                    placeholder="[Interface]&#10;PrivateKey = ...&#10;Address = 10.0.0.2/32&#10;[Peer]&#10;PublicKey = ...&#10;Endpoint = vpn.company.com:51820&#10;AllowedIPs = 10.0.0.0/8"></textarea>
                </div>
              {:else}
                <div>
                  <label class="label text-xs" for="ovpn-profile">OpenVPN profile (.ovpn)</label>
                  <textarea class="input text-sm font-mono h-28" id="ovpn-profile" bind:value={ovpnProfile}
                    placeholder="client&#10;dev tun&#10;remote vpn.company.com 1194 udp&#10;..."></textarea>
                </div>
                <div class="grid grid-cols-2 gap-2">
                  <div>
                    <label class="label text-xs" for="ovpn-user">Username (if required)</label>
                    <input class="input text-sm" id="ovpn-user" bind:value={ovpnUsername} autocomplete="off" />
                  </div>
                  <div>
                    <label class="label text-xs" for="ovpn-pass">Password (if required)</label>
                    <input class="input text-sm" id="ovpn-pass" type="password" bind:value={ovpnPassword} autocomplete="off" />
                  </div>
                </div>
              {/if}
              <p class="text-xs text-amber-700 bg-amber-50 border border-amber-100 rounded-lg p-3">
                The profile is encrypted at rest. A VPN uses UDP, so it can't be combined with an
                egress allowlist (that blocks all UDP and would break the tunnel).
              </p>
            {/if}

            {#if testEnvStrategy === 'compose' || testEnvStrategy === 'cloud'}
              <div>
                <label class="label text-xs" for="env-vars">
                  {testEnvStrategy === 'cloud' ? 'Cloud credentials' : 'Environment variables (optional)'}
                </label>
                <textarea class="input text-sm font-mono h-24" id="env-vars" bind:value={envVars}
                  placeholder="AWS_ACCESS_KEY_ID=AKIA...&#10;AWS_SECRET_ACCESS_KEY=...&#10;AWS_REGION=us-east-1"></textarea>
                <p class="text-xs text-gray-400 mt-1">
                  <span class="font-mono">KEY=value</span> per line. Values whose names contain KEY/TOKEN/SECRET/PASSWORD/AUTHKEY
                  are encrypted; the rest are stored as plain config.
                </p>
              </div>
            {/if}

            {#if testEnvError}<p class="text-xs text-red-500">{testEnvError}</p>{/if}
            <div class="flex items-center gap-3">
              <button class="btn-primary text-sm" disabled={savingTestEnv} on:click={saveTestEnv}>
                {savingTestEnv ? 'Saving...' : testEnvGrant ? 'Update test environment' : 'Save test environment'}
              </button>
              {#if testEnvGrant}<span class="text-xs text-gray-400">Active: {testEnvStrategy}</span>{/if}
            </div>
          </div>
        </div>

        <div class="card p-5 border-red-200 bg-red-50">
          <h3 class="font-semibold text-red-800 mb-1">Kill switch</h3>
          <p class="text-sm text-red-600 mb-3">Revoke all agent access immediately. Use if you need to stop the agent urgently.</p>
          <button class="btn-danger text-sm" on:click={killSwitch}>Revoke all access now</button>
        </div>
      </div>

    <!-- Contract tab -->
    {:else if activeTab === 'contract'}
      {#if loadingContract}
        <div class="card p-8 animate-pulse"><div class="h-64 bg-gray-100 rounded"></div></div>
      {:else if contractMd}
        <div class="card p-6 prose prose-sm max-w-none">
          <!-- Simple markdown rendering: just display as pre for now -->
          <pre class="whitespace-pre-wrap font-sans text-sm text-gray-700 leading-relaxed">{contractMd}</pre>
        </div>
      {:else}
        <div class="card p-8 text-center text-gray-400 text-sm">
          No contract generated yet.
          <button class="btn-secondary text-sm ml-3" on:click={() => api.post(`/jobs/${jobId}/contract`, {}).then(loadContract)}>
            Generate contract
          </button>
        </div>
      {/if}

    <!-- Audit log tab -->
    {:else if activeTab === 'audit'}
      <div class="card divide-y divide-gray-100">
        {#if audit.length === 0}
          <div class="p-8 text-center text-gray-400 text-sm">No audit events yet.</div>
        {:else}
          {#each audit as ev}
            <div class="px-5 py-3 flex items-start gap-3">
              <div class="w-6 h-6 rounded-full flex items-center justify-center text-xs shrink-0 mt-0.5
                {ev.actor_type === 'agent' ? 'bg-purple-100 text-purple-700' :
                 ev.actor_type === 'professional' ? 'bg-blue-100 text-blue-700' :
                 ev.actor_type === 'company' ? 'bg-green-100 text-green-700' :
                 'bg-gray-100 text-gray-600'}">
                {ev.actor_type[0].toUpperCase()}
              </div>
              <div class="flex-1 min-w-0">
                <div class="text-sm text-gray-700">{ev.summary}</div>
                <div class="text-xs text-gray-400 mt-0.5">{timeAgo(ev.created_at)}</div>
              </div>
              <span class="text-xs text-gray-400 bg-gray-100 rounded px-2 py-0.5 font-mono shrink-0">{ev.event_type}</span>
            </div>
          {/each}
        {/if}
      </div>

    <!-- Billing tab -->
    {:else if activeTab === 'billing'}
      <div class="card">
        <div class="px-5 py-4 border-b border-gray-100 flex items-center justify-between">
          <h2 class="font-semibold">Weekly invoices</h2>
          <button class="btn-secondary text-sm" on:click={() => api.post('/billing/generate', {}).then(loadBilling)}>
            Generate this week
          </button>
        </div>
        {#if loadingBilling}
          <div class="p-8 animate-pulse"><div class="h-24 bg-gray-100 rounded"></div></div>
        {:else if invoices.length === 0}
          <div class="p-8 text-center text-gray-400 text-sm">No invoices yet.</div>
        {:else}
          <table class="w-full text-sm">
            <thead class="bg-gray-50 text-xs text-gray-500 uppercase tracking-wide">
              <tr>
                <th class="px-5 py-3 text-left">Week</th>
                <th class="px-5 py-3 text-left">Tasks</th>
                <th class="px-5 py-3 text-right">Billed</th>
                <th class="px-5 py-3 text-right">Platform fee</th>
                <th class="px-5 py-3 text-right">Pro payout</th>
                <th class="px-5 py-3 text-left">Status</th>
              </tr>
            </thead>
            <tbody class="divide-y divide-gray-100">
              {#each invoices as inv}
                <tr class="hover:bg-gray-50">
                  <td class="px-5 py-3 text-gray-700">{inv.week_start} – {inv.week_end}</td>
                  <td class="px-5 py-3 text-gray-500">{inv.tasks_delivered}</td>
                  <td class="px-5 py-3 text-right font-medium">${(inv.gross_amount / 100).toFixed(2)}</td>
                  <td class="px-5 py-3 text-right text-gray-400">${(inv.platform_fee / 100).toFixed(2)}</td>
                  <td class="px-5 py-3 text-right text-green-700 font-medium">${(inv.net_payout / 100).toFixed(2)}</td>
                  <td class="px-5 py-3">
                    <span class="badge-{inv.status === 'paid' ? 'green' : inv.status === 'failed' ? 'red' : 'yellow'}">
                      {inv.status}
                    </span>
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      </div>
    {/if}
  {/if}
</div>
