<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { browser } from '$app/environment';
  import { api } from '$lib/api/client';
  import { session, getToken, isLoggedIn } from '$lib/stores/session';
  import { get } from 'svelte/store';
  import type { ProfessionalDashboard, Task, Job } from '$lib/api/types';
  import { goto } from '$app/navigation';

  $: professionalId = $session.professionalId;
  $: userId = $session.userId;

  let dashboard: ProfessionalDashboard | null = null;
  let agent: { id: string; status: string } | null = null;
  let loading = true;
  let error = '';
  let advancingStatus = false;
  let retraining = false;
  let training = false;
  let vaultNoteCount = 0;
  let notifEventSource: EventSource | null = null;
  let activeTasks: Array<Task & { job_title: string }> = [];

  // Connected accounts (owner-side OAuth connections the agent acts through)
  interface Conn { provider: string; external_account: string }
  let connections: Conn[] = [];
  let connectingGoogle = false;

  $: hasGithub = connections.some(c => c.provider === 'github');
  $: googleConn = connections.find(c => c.provider === 'google');

  async function loadConnections() {
    try {
      connections = await api.get<Conn[]>('/auth/connections/mine');
    } catch { connections = []; }
  }

  async function connectGoogle() {
    if (!agent) return;
    connectingGoogle = true;
    try {
      const res = await api.get<{ url: string }>(`/auth/google/start?agent_id=${agent.id}`);
      window.location.href = res.url;
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to start Google OAuth';
      connectingGoogle = false;
    }
  }

  async function markAgentReady() {
    if (!agent) return;
    advancingStatus = true;
    try {
      // AgentStatus serializes lowercase ("ready"), not "Ready" — sending the wrong case 422s.
      const updated = await api.patch<{ id: string; status: string }>(`/agents/${agent.id}/status`, { status: 'ready' });
      agent = updated; // only overwrite on success, so a failure can't blank the agent
      if (professionalId) {
        dashboard = await api.get<ProfessionalDashboard>(`/dashboard/professional/${professionalId}`);
        error = '';
      }
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to update status';
    } finally {
      advancingStatus = false;
    }
  }

  async function retrainAgent() {
    if (!agent) return;
    retraining = true;
    error = '';
    try {
      await api.post(`/agents/${agent.id}/retrain`, {});
      agent = { ...agent, status: 'training' };
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to trigger retraining';
    } finally {
      retraining = false;
    }
  }

  // First-time training — manual, deliberate. Same endpoint as retrain (POST /agents/:id/retrain
  // → trigger_training_with_filter, full KB). Training is no longer auto-started by onboarding.
  async function trainAgent() {
    if (!agent) return;
    training = true;
    error = '';
    try {
      await api.post(`/agents/${agent.id}/retrain`, {});
      agent = { ...agent, status: 'training' };
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to start training';
    } finally {
      training = false;
    }
  }

  async function loadVaultNoteCount(agentId: string) {
    try {
      const notes = await api.get<unknown[]>(`/knowledge/agent/${agentId}/notes`);
      vaultNoteCount = Array.isArray(notes) ? notes.length : 0;
    } catch { vaultNoteCount = 0; }
  }

  onMount(async () => {
    // Read synchronously from the store (reactive $ vars may not be updated yet inside onMount)
    let resolvedProfessionalId = get(session).professionalId;

    // If missing, recover from the API (handles sign-in before session rehydration fix)
    if (!resolvedProfessionalId) {
      try {
        const prof = await api.get<{ id: string }>('/professionals/mine');
        const agentRes = await api.get<{ id: string }>('/agents/mine');
        session.setIds({ professionalId: prof.id, agentId: agentRes.id });
        resolvedProfessionalId = prof.id;
      } catch { /* genuinely new user */ }
    }

    // Load the owner's connected accounts (GitHub/Google) regardless of dashboard state.
    loadConnections();

    if (resolvedProfessionalId) {
      try {
        const resolvedAgentId = get(session).agentId;
        [dashboard] = await Promise.all([
          api.get<ProfessionalDashboard>(`/dashboard/professional/${resolvedProfessionalId}`),
          api.get<{ id: string; status: string }>('/agents/mine')
            .then(a => { agent = a; session.setIds({ agentId: a.id }); loadVaultNoteCount(a.id); })
            .catch(() => { /* no agent yet */ }),
        ]);

        // Load active tasks across all jobs
        if (resolvedAgentId || get(session).agentId) {
          const agentId = resolvedAgentId || get(session).agentId!;
          const jobs = await api.get<Job[]>(`/jobs/agent/${agentId}`).catch(() => [] as Job[]);
          const activeJobs = jobs.filter(j => j.status === 'open' || j.status === 'trial' || j.status === 'active');
          const taskLists = await Promise.all(
            activeJobs.map(j =>
              api.get<Task[]>(`/tasks/job/${j.id}`)
                .then(ts => ts.map(t => ({ ...t, job_title: j.title })))
                .catch(() => [] as Array<Task & { job_title: string }>)
            )
          );
          activeTasks = taskLists.flat().filter(t => t.status === 'pending' || t.status === 'running');
        }
      } catch (e: unknown) {
        error = e instanceof Error ? e.message : 'Could not load dashboard';
      } finally {
        loading = false;
      }
    } else {
      error = 'No professional profile yet';
      loading = false;
    }

    const resolvedUserId = get(session).userId;
    if (browser && resolvedUserId) {
      const token = getToken();
      notifEventSource = new EventSource(`/api/v1/events/notifications/${resolvedUserId}${token ? `?token=${token}` : ''}`);
      notifEventSource.addEventListener('notification', () => {
        const pid = get(session).professionalId;
        if (pid) {
          Promise.all([
            api.get<ProfessionalDashboard>(`/dashboard/professional/${pid}`).then(d => { dashboard = d; }),
            api.get<{ id: string; status: string }>('/agents/mine').then(a => { agent = a; }).catch(() => {}),
          ]).catch(() => {});
        }
      });
    }
  });

  onDestroy(() => {
    notifEventSource?.close();
  });

  function confidenceColor(score: number | null) {
    if (score == null) return 'badge-gray';
    if (score >= 0.9) return 'badge-green';
    if (score >= 0.7) return 'badge-yellow';
    return 'badge-red';
  }

  function confidenceLabel(score: number | null) {
    if (score == null) return 'unscored';
    return `${Math.round(score * 100)}%`;
  }

  function timeAgo(iso: string) {
    const diff = Date.now() - new Date(iso).getTime();
    const m = Math.floor(diff / 60000);
    if (m < 60) return `${m}m ago`;
    const h = Math.floor(m / 60);
    if (h < 24) return `${h}h ago`;
    return `${Math.floor(h / 24)}d ago`;
  }
</script>

<svelte:head><title>Agent Studio — Flaborful</title></svelte:head>

<div class="max-w-7xl mx-auto px-4 py-8">
  <div class="mb-8">
    <h1 class="text-2xl font-bold">Agent Studio</h1>
    <p class="text-gray-500 text-sm mt-0.5">Your professional AI workspace</p>
  </div>

  <!-- Agent status banner -->
  {#if agent}
    {#if agent.status.toLowerCase() === 'training'}
      <div class="mb-6 rounded-xl border bg-amber-50 border-amber-200 px-5 py-4 flex items-center gap-4">
        <div class="flex-1">
          <div class="font-semibold text-sm">Agent status: <span class="text-amber-700">Training</span></div>
          <div class="text-xs text-gray-500 mt-0.5">LoRA training is running. You'll be notified when your agent is ready.</div>
        </div>
        <div class="flex gap-2 shrink-0">
          <button class="btn-primary text-sm" disabled={advancingStatus} on:click={markAgentReady}>
            {advancingStatus ? 'Updating...' : 'Mark as ready'}
          </button>
        </div>
      </div>
    {:else if agent.status.toLowerCase() === 'onboarding'}
      <div class="mb-6 rounded-xl border bg-blue-50 border-blue-200 px-5 py-4 flex items-center gap-4">
        <div class="flex-1">
          <div class="font-semibold text-sm">Agent status: <span class="text-blue-700">Onboarding</span></div>
          <div class="text-xs text-gray-500 mt-0.5">
            {#if vaultNoteCount > 0}
              Your knowledge vault has {vaultNoteCount} note{vaultNoteCount === 1 ? '' : 's'}. Train your agent when you're ready — or keep enriching it first.
            {:else}
              Build your knowledge base, then train your agent when you're ready.
            {/if}
          </div>
        </div>
        <div class="flex gap-2 shrink-0">
          <button class="btn-ghost text-sm" on:click={() => goto('/studio/onboarding')}>
            Continue setup
          </button>
          <button class="btn-primary text-sm" disabled={training || vaultNoteCount === 0} on:click={trainAgent}>
            {training ? 'Queuing…' : 'Train agent ▶'}
          </button>
        </div>
      </div>
    {:else if agent.status.toLowerCase() === 'ready' || agent.status.toLowerCase() === 'deployed'}
      <div class="mb-6 rounded-xl border bg-green-50 border-green-200 px-5 py-4 flex items-center gap-4">
        <div class="flex-1">
          <div class="font-semibold text-sm">Agent status: <span class="text-green-700 capitalize">{agent.status}</span></div>
          <div class="text-xs text-gray-500 mt-0.5">Retrain from your existing knowledge base to improve your agent without redoing onboarding.</div>
        </div>
        <div class="flex gap-2 shrink-0">
          <button class="btn-ghost text-sm" on:click={() => goto('/studio/onboarding')}>
            Add knowledge
          </button>
          <button class="btn-primary text-sm" disabled={retraining} on:click={retrainAgent}>
            {retraining ? 'Queuing...' : 'Retrain agent'}
          </button>
        </div>
      </div>
    {/if}
  {/if}

  {#if loading}
    <div class="grid sm:grid-cols-4 gap-4 mb-8">
      {#each Array(4) as _}
        <div class="card p-4 animate-pulse"><div class="h-12 bg-gray-100 rounded"></div></div>
      {/each}
    </div>
  {:else if error}
    <!-- New professional — show onboarding prompt -->
    <div class="card p-12 text-center">
      <div class="text-5xl mb-4">🤖</div>
      <h2 class="text-xl font-semibold mb-2">Your agent doesn't exist yet</h2>
      <p class="text-gray-500 mb-6">Build your AI agent in three steps: Identity → Knowledge → Workflows.</p>
      <button class="btn-primary px-8" on:click={() => goto('/studio/onboarding')}>
        Start building
      </button>
    </div>
  {:else if dashboard}
    <!-- Stat cards -->
    <div class="grid sm:grid-cols-2 lg:grid-cols-4 gap-4 mb-8">
      {#each [
        { label: 'Agent status', value: dashboard.agent_status, sub: '' },
        { label: 'Active jobs', value: dashboard.active_jobs, sub: '' },
        { label: 'Pending review', value: dashboard.tasks_pending_review, sub: 'tasks waiting for you' },
        { label: 'Delivered today', value: dashboard.tasks_delivered_today, sub: 'outputs sent' },
      ] as stat}
        <div class="card p-4">
          <div class="text-xs text-gray-500 font-medium uppercase tracking-wide mb-1">{stat.label}</div>
          <div class="text-2xl font-bold text-gray-900">{stat.value}</div>
          {#if stat.sub}<div class="text-xs text-gray-400 mt-0.5">{stat.sub}</div>{/if}
        </div>
      {/each}
    </div>

    {#if dashboard.open_jobs > 0}
      <div class="card p-5 mb-8 border-l-4 border-brand-500 bg-brand-50">
        <div class="flex items-center gap-3">
          <div class="text-2xl">🎉</div>
          <div class="flex-1">
            <div class="font-semibold text-brand-800">
              Hired! Awaiting trial start
            </div>
            <div class="text-sm text-brand-700 mt-0.5">
              You've been hired onto {dashboard.open_jobs} job{dashboard.open_jobs > 1 ? 's' : ''}. The company will start the trial — head to
              <a href="/studio/jobs" class="underline font-medium">My jobs</a> to see the status.
            </div>
          </div>
        </div>
      </div>
    {/if}

    <div class="grid lg:grid-cols-3 gap-6">
      <!-- Review queue -->
      <div class="lg:col-span-2">
        <div class="card">
          <div class="px-5 py-4 border-b border-gray-100 flex items-center justify-between">
            <h2 class="font-semibold">Review queue</h2>
            {#if dashboard.tasks_pending_review > 0}
              <span class="badge-red">{dashboard.tasks_pending_review} pending</span>
            {/if}
          </div>
          {#if dashboard.pending_review_tasks.length === 0}
            <div class="p-8 text-center text-gray-400 text-sm">
              No tasks waiting — your agent is on it 🎉
            </div>
          {:else}
            <ul class="divide-y divide-gray-100">
              {#each dashboard.pending_review_tasks as task}
                <li class="px-5 py-4 flex items-center gap-4 hover:bg-gray-50 transition-colors">
                  <div class="flex-1 min-w-0">
                    <div class="font-medium text-sm truncate">{task.job_title}</div>
                    <div class="text-xs text-gray-400 mt-0.5">{timeAgo(task.created_at)}</div>
                  </div>
                  <span class="{confidenceColor(task.confidence_score)} shrink-0">
                    {confidenceLabel(task.confidence_score)}
                  </span>
                  <button
                    class="btn-primary text-xs shrink-0"
                    on:click={() => goto(`/studio/review/${task.task_id}`)}>
                    Review
                  </button>
                </li>
              {/each}
            </ul>
          {/if}
        </div>
      </div>

      <!-- Active tasks (pending/running, no review needed yet) -->
      {#if activeTasks.length > 0}
        <div class="lg:col-span-2">
          <div class="card">
            <div class="px-5 py-4 border-b border-gray-100 flex items-center justify-between">
              <h2 class="font-semibold">Active tasks</h2>
              <span class="badge-blue">{activeTasks.length} in progress</span>
            </div>
            <ul class="divide-y divide-gray-100">
              {#each activeTasks as task}
                <li class="px-5 py-4 flex items-center gap-4">
                  <div class="flex-1 min-w-0">
                    <div class="font-medium text-sm truncate">{task.job_title}</div>
                    <div class="text-xs text-gray-400 mt-0.5">{timeAgo(task.created_at)}</div>
                  </div>
                  <span class="text-xs px-2 py-1 rounded-full capitalize
                    {task.status === 'running' ? 'bg-blue-100 text-blue-700' : 'bg-gray-100 text-gray-600'}">
                    {task.status}
                  </span>
                  {#if !task.workflow_id}
                    <button class="btn-primary text-xs shrink-0" on:click={() => goto(`/studio/review/${task.id}`)}>
                      Assign workflow
                    </button>
                  {/if}
                </li>
              {/each}
            </ul>
          </div>
        </div>
      {/if}

      <!-- Side stats -->
      <div class="space-y-4">
        <div class="card p-5">
          <h3 class="text-sm font-semibold text-gray-700 mb-3">Agent performance</h3>
          <div class="space-y-3">
            <div class="flex justify-between text-sm">
              <span class="text-gray-500">Total approvals</span>
              <span class="font-medium">{dashboard.total_approved_outputs}</span>
            </div>
            <div class="flex justify-between text-sm">
              <span class="text-gray-500">Amendment rate</span>
              <span class="font-medium">{dashboard.amendment_rate.toFixed(1)}%</span>
            </div>
            <div class="flex justify-between text-sm">
              <span class="text-gray-500">Avg confidence</span>
              <span class="font-medium">{Math.round(dashboard.avg_confidence_score * 100)}%</span>
            </div>
          </div>
        </div>

        <div class="card p-5">
          <h3 class="text-sm font-semibold text-gray-700 mb-3">Quick links</h3>
          <nav class="space-y-1">
            {#each [
              { label: 'Job board', href: '/studio/jobs/board' },
              { label: 'Workflow Studio', href: '/studio/workflows' },
              { label: 'Executions', href: '/studio/executions' },
              { label: 'Knowledge vault', href: '/studio/knowledge' },
              { label: 'My jobs', href: '/studio/jobs' },
              { label: 'Payouts', href: '/studio/payouts' },
              { label: 'Agent settings', href: '/studio/agent' },
            ] as link}
              <a href={link.href} class="block text-sm text-brand-600 hover:text-brand-700 py-1">{link.label}</a>
            {/each}
          </nav>
        </div>

        <!-- Connected accounts: the providers the agent acts through on jobs.
             Companies grant the agent access to their resources for whichever of
             these the owner has connected. -->
        <div class="card p-5">
          <h3 class="text-sm font-semibold text-gray-700 mb-1">Connected accounts</h3>
          <p class="text-xs text-gray-400 mb-3">Your agent works through these. Companies grant it access to their repos and drives.</p>

          <div class="space-y-2">
            <!-- GitHub -->
            <div class="flex items-center gap-2.5 text-sm">
              <svg class="w-4 h-4 text-gray-700 shrink-0" viewBox="0 0 24 24" fill="currentColor"><path d="M12 0C5.37 0 0 5.37 0 12c0 5.31 3.435 9.795 8.205 11.385.6.105.825-.255.825-.57 0-.285-.015-1.23-.015-2.235-3.015.555-3.795-.735-4.035-1.41-.135-.345-.72-1.41-1.23-1.695-.42-.225-1.02-.78-.015-.795.945-.015 1.62.87 1.845 1.23 1.08 1.815 2.805 1.305 3.495.99.105-.78.42-1.305.765-1.605-2.67-.3-5.46-1.335-5.46-5.925 0-1.305.465-2.385 1.23-3.225-.12-.3-.54-1.53.12-3.18 0 0 1.005-.315 3.3 1.23.96-.27 1.98-.405 3-.405s2.04.135 3 .405c2.295-1.56 3.3-1.23 3.3-1.23.66 1.65.24 2.88.12 3.18.765.84 1.23 1.905 1.23 3.225 0 4.605-2.805 5.625-5.475 5.925.435.375.81 1.095.81 2.22 0 1.605-.015 2.895-.015 3.3 0 .315.225.69.825.57A12.02 12.02 0 0 0 24 12c0-6.63-5.37-12-12-12z"/></svg>
              <span class="flex-1 text-gray-700">GitHub</span>
              {#if hasGithub}
                <span class="badge-green text-xs">Connected</span>
              {:else}
                <a href="/studio/onboarding" class="text-xs text-brand-600 hover:text-brand-700">Connect</a>
              {/if}
            </div>

            <!-- Google Workspace -->
            <div class="flex items-center gap-2.5 text-sm">
              <svg class="w-4 h-4 shrink-0" viewBox="0 0 24 24"><path fill="#4285F4" d="M22.56 12.25c0-.78-.07-1.53-.2-2.25H12v4.26h5.92a5.06 5.06 0 0 1-2.2 3.32v2.77h3.57c2.08-1.92 3.27-4.74 3.27-8.1z"/><path fill="#34A853" d="M12 23c2.97 0 5.46-.98 7.28-2.66l-3.57-2.77c-.98.66-2.23 1.06-3.71 1.06-2.86 0-5.29-1.93-6.16-4.53H2.18v2.84A11 11 0 0 0 12 23z"/><path fill="#FBBC05" d="M5.84 14.1a6.6 6.6 0 0 1 0-4.2V7.06H2.18a11 11 0 0 0 0 9.88l3.66-2.84z"/><path fill="#EA4335" d="M12 5.38c1.62 0 3.06.56 4.21 1.64l3.15-3.15C17.45 2.09 14.97 1 12 1 7.7 1 3.99 3.47 2.18 7.06l3.66 2.84C6.71 7.31 9.14 5.38 12 5.38z"/></svg>
              <span class="flex-1 text-gray-700">Google Workspace</span>
              {#if googleConn}
                <span class="badge-green text-xs" title={googleConn.external_account}>Connected</span>
              {:else if agent}
                <button class="text-xs text-brand-600 hover:text-brand-700" disabled={connectingGoogle} on:click={connectGoogle}>
                  {connectingGoogle ? 'Redirecting…' : 'Connect'}
                </button>
              {:else}
                <span class="text-xs text-gray-300">—</span>
              {/if}
            </div>
          </div>

          {#if googleConn}
            <p class="text-[11px] text-gray-400 mt-3">{googleConn.external_account}</p>
          {/if}
        </div>
      </div>
    </div>
  {/if}
</div>
