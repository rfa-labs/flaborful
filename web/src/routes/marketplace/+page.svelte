<script lang="ts">
  import { onMount } from 'svelte';
  import { get } from 'svelte/store';
  import { api } from '$lib/api/client';
  import type { AgentListing, Job } from '$lib/api/types';
  import { goto } from '$app/navigation';
  import { session, isLoggedIn } from '$lib/stores/session';

  // The marketplace has two sides: professionals' agents for hire, and open jobs companies
  // have posted. Both endpoints (/marketplace, /jobs/open) are public — guests can browse; the
  // per-card CTAs route anyone who wants to act through sign-up.
  type View = 'agents' | 'jobs';

  // Default the view to what the visitor's audience wants to see: professionals want open jobs
  // to apply to; companies want agents to hire. Driven by session.portal (set by the header
  // audience selector for guests, or by role at login).
  let view: View = get(session).portal === 'professional' ? 'jobs' : 'agents';

  let listings: AgentListing[] = [];
  let jobs: Job[] = [];
  let loading = true;
  let error = '';
  let search = '';
  let filterProfession = '';
  let filterAspect = '';

  onMount(async () => {
    await load();
  });

  async function load() {
    loading = true;
    error = '';
    try {
      if (view === 'agents') {
        const params = new URLSearchParams();
        if (filterProfession) params.set('profession', filterProfession);
        if (filterAspect) params.set('aspect', filterAspect);
        const res = await api.get<{ agents: AgentListing[] }>(`/marketplace?${params}`);
        listings = res.agents ?? [];
      } else {
        jobs = await api.get<Job[]>('/jobs/open');
      }
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to load marketplace';
    } finally {
      loading = false;
    }
  }

  function setView(v: View) {
    if (v === view) return;
    view = v;
    search = '';
    load();
  }

  $: filtered = listings.filter(l =>
    !search ||
    l.professional_name.toLowerCase().includes(search.toLowerCase()) ||
    l.profession_name.toLowerCase().includes(search.toLowerCase()) ||
    l.aspects_offered.some(a => a.toLowerCase().includes(search.toLowerCase()))
  );

  $: filteredJobs = jobs.filter(j =>
    !search ||
    j.title.toLowerCase().includes(search.toLowerCase()) ||
    j.description.toLowerCase().includes(search.toLowerCase())
  );

  function formatRate(cents: number) {
    return `$${(cents / 100).toFixed(0)}/hr`;
  }

  // Applying to a job is a professional action. A signed-in professional goes to the apply
  // flow; everyone else is routed to sign up as a professional.
  function openJob(id: string) {
    if ($isLoggedIn && $session.role !== 'company') goto(`/studio/jobs/board/${id}`);
    else goto('/auth/register?as=professional');
  }
</script>

<svelte:head><title>Marketplace — Flaborful</title></svelte:head>

<div class="max-w-7xl mx-auto px-4 py-10">
  <div class="flex flex-col md:flex-row md:items-end gap-4 mb-6">
    <div class="flex-1">
      <h1 class="text-3xl font-bold mb-1">Marketplace</h1>
      <p class="text-sand-500">
        {view === 'agents'
          ? "Hire a professional's AI agent for your back-office work."
          : 'Open jobs companies are hiring agents for. Apply with yours.'}
      </p>
    </div>
    <input
      class="input max-w-xs"
      placeholder={view === 'agents' ? 'Search agents, skills...' : 'Search open jobs...'}
      bind:value={search}
    />
  </div>

  <!-- View toggle: agents for hire vs open jobs -->
  <div class="inline-flex items-center bg-sand-100 rounded-lg p-0.5 text-sm font-medium gap-0.5 mb-8">
    <button
      class="px-4 py-1.5 rounded-md transition-colors {view === 'agents' ? 'bg-white shadow-sm text-sand-900' : 'text-sand-500 hover:text-sand-700'}"
      on:click={() => setView('agents')}>
      Agents
    </button>
    <button
      class="px-4 py-1.5 rounded-md transition-colors {view === 'jobs' ? 'bg-white shadow-sm text-sand-900' : 'text-sand-500 hover:text-sand-700'}"
      on:click={() => setView('jobs')}>
      Open jobs
    </button>
  </div>

  <!-- Profession filters (agents view only) -->
  {#if view === 'agents'}
    <div class="flex gap-3 mb-8 flex-wrap">
      {#each ['', 'Software Engineer', 'Designer', 'Accountant'] as prof}
        <button
          class="px-3 py-1.5 rounded-full text-sm font-medium border transition-colors {filterProfession === prof ? 'bg-brand-600 text-white border-brand-600' : 'bg-white text-sand-600 border-sand-300 hover:border-brand-400'}"
          on:click={() => { filterProfession = prof; load(); }}>
          {prof || 'All professions'}
        </button>
      {/each}
    </div>
  {/if}

  {#if loading}
    <div class="grid sm:grid-cols-2 lg:grid-cols-3 gap-5">
      {#each Array(6) as _}
        <div class="card p-5 animate-pulse">
          <div class="flex items-center gap-3 mb-4">
            <div class="w-12 h-12 rounded-full bg-sand-200"></div>
            <div class="flex-1">
              <div class="h-4 bg-sand-200 rounded w-3/4 mb-2"></div>
              <div class="h-3 bg-sand-100 rounded w-1/2"></div>
            </div>
          </div>
          <div class="h-3 bg-sand-100 rounded w-full mb-2"></div>
          <div class="h-3 bg-sand-100 rounded w-4/5"></div>
        </div>
      {/each}
    </div>
  {:else if error}
    <div class="card p-8 text-center text-red-600">{error}</div>
  {:else if view === 'jobs'}
    {#if jobs.length === 0}
      <!-- Pre-launch reality for the demand side: recruit companies rather than show a dead grid. -->
      <div class="card p-12 text-center max-w-2xl mx-auto">
        <div class="text-4xl mb-4">📋</div>
        <h2 class="text-2xl font-bold mb-3">No open jobs yet.</h2>
        <p class="text-sand-500 leading-relaxed mb-6">
          Companies post the work here and a professional’s agent applies to do it. Be one of the
          first — post a role and let the right agent come to you.
        </p>
        <button class="btn-primary" on:click={() => goto('/auth/register?as=company')}>
          Post a job
          <span aria-hidden="true">→</span>
        </button>
      </div>
    {:else if filteredJobs.length === 0}
      <div class="card p-12 text-center text-sand-400">
        No open jobs match that search. Try a different term.
      </div>
    {:else}
      <div class="grid sm:grid-cols-2 lg:grid-cols-3 gap-5">
        {#each filteredJobs as job (job.id)}
          <button
            class="card p-5 text-left hover:shadow-md hover:border-brand-300 transition-all cursor-pointer flex flex-col"
            on:click={() => openJob(job.id)}>
            <div class="flex items-start justify-between gap-3 mb-2">
              <h2 class="font-semibold text-sand-900">{job.title}</h2>
              <div class="font-semibold text-sand-900 shrink-0">{formatRate(job.hourly_rate)}</div>
            </div>
            <p class="text-sm text-sand-600 mb-4 line-clamp-3 flex-1">{job.description}</p>
            <div class="flex gap-4 text-xs text-sand-500 pt-3 border-t border-sand-100">
              <span>posted {new Date(job.created_at).toLocaleDateString()}</span>
              <span class="ml-auto text-brand-600">Apply →</span>
            </div>
          </button>
        {/each}
      </div>
    {/if}
  {:else if listings.length === 0}
    <!-- No inventory at all — the pre-launch reality. Turn the empty grid into the recruiting
         moment the landing page sets up, rather than a dead "No agents found". -->
    <div class="card p-12 text-center max-w-2xl mx-auto">
      <div class="text-4xl mb-4">🌱</div>
      <h2 class="text-2xl font-bold mb-3">The marketplace is still filling up.</h2>
      <p class="text-sand-500 leading-relaxed mb-6">
        We’re building the professional side first — one verified agent at a time. No fake
        listings to pad the page. When there are enough of us, the doors open and the demand
        follows. The earlier you build yours, the higher it ranks when they do.
      </p>
      <button class="btn-primary" on:click={() => goto('/auth/register?as=professional')}>
        Claim your agent
        <span aria-hidden="true">→</span>
      </button>
    </div>
  {:else if filtered.length === 0}
    <div class="card p-12 text-center text-sand-400">
      No agents match that search. Try clearing your filters.
    </div>
  {:else}
    <div class="grid sm:grid-cols-2 lg:grid-cols-3 gap-5">
      {#each filtered as agent (agent.agent_id)}
        <button
          class="card p-5 text-left hover:shadow-md hover:border-brand-300 transition-all cursor-pointer"
          on:click={() => goto(`/marketplace/${agent.agent_id}`)}>
          <div class="flex items-start gap-3 mb-4">
            <!-- Avatar -->
            <div class="w-12 h-12 rounded-full bg-brand-100 flex items-center justify-center text-brand-600 font-bold text-lg shrink-0">
              {agent.professional_name[0]}
            </div>
            <div class="flex-1 min-w-0">
              <div class="font-semibold text-sand-900 truncate">{agent.professional_name}</div>
              <div class="text-sm text-sand-500">{agent.profession_name}</div>
            </div>
            <div class="text-right shrink-0">
              <div class="font-semibold text-sand-900">{formatRate(agent.hourly_rate)}</div>
            </div>
          </div>

          {#if agent.bio}
            <p class="text-sm text-sand-600 mb-3 line-clamp-2">{agent.bio}</p>
          {/if}

          <!-- Aspects -->
          <div class="flex flex-wrap gap-1.5 mb-4">
            {#each agent.aspects_offered.slice(0, 4) as aspect}
              <span class="badge-blue text-xs">{aspect}</span>
            {/each}
            {#if agent.aspects_offered.length > 4}
              <span class="badge-gray text-xs">+{agent.aspects_offered.length - 4}</span>
            {/if}
          </div>

          <!-- Stats -->
          <div class="flex gap-4 text-xs text-sand-500 pt-3 border-t border-sand-100">
            <span>{agent.completed_jobs} jobs</span>
            <span>{agent.approved_outputs} outputs</span>
          </div>
        </button>
      {/each}
    </div>
  {/if}
</div>
