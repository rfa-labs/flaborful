<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from '$lib/api/client';
  import type { AgentListing } from '$lib/api/types';
  import { goto } from '$app/navigation';

  let listings: AgentListing[] = [];
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
      const params = new URLSearchParams();
      if (filterProfession) params.set('profession', filterProfession);
      if (filterAspect) params.set('aspect', filterAspect);
      const res = await api.get<{ agents: AgentListing[] }>(`/marketplace?${params}`);
      listings = res.agents ?? [];
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to load marketplace';
    } finally {
      loading = false;
    }
  }

  $: filtered = listings.filter(l =>
    !search ||
    l.professional_name.toLowerCase().includes(search.toLowerCase()) ||
    l.profession_name.toLowerCase().includes(search.toLowerCase()) ||
    l.aspects_offered.some(a => a.toLowerCase().includes(search.toLowerCase()))
  );

  function formatRate(cents: number) {
    return `$${(cents / 100).toFixed(0)}/hr`;
  }
</script>

<svelte:head><title>Marketplace — Flaborful</title></svelte:head>

<div class="max-w-7xl mx-auto px-4 py-10">
  <div class="flex flex-col md:flex-row md:items-end gap-4 mb-8">
    <div class="flex-1">
      <h1 class="text-3xl font-bold mb-1">Agent Marketplace</h1>
      <p class="text-gray-500">Hire a professional's AI agent for your back-office work.</p>
    </div>
    <input
      class="input max-w-xs"
      placeholder="Search agents, skills..."
      bind:value={search}
    />
  </div>

  <!-- Filters -->
  <div class="flex gap-3 mb-8 flex-wrap">
    {#each ['', 'Software Engineer', 'Designer', 'Accountant'] as prof}
      <button
        class="px-3 py-1.5 rounded-full text-sm font-medium border transition-colors {filterProfession === prof ? 'bg-brand-600 text-white border-brand-600' : 'bg-white text-gray-600 border-gray-300 hover:border-brand-400'}"
        on:click={() => { filterProfession = prof; load(); }}>
        {prof || 'All professions'}
      </button>
    {/each}
  </div>

  {#if loading}
    <div class="grid sm:grid-cols-2 lg:grid-cols-3 gap-5">
      {#each Array(6) as _}
        <div class="card p-5 animate-pulse">
          <div class="flex items-center gap-3 mb-4">
            <div class="w-12 h-12 rounded-full bg-gray-200"></div>
            <div class="flex-1">
              <div class="h-4 bg-gray-200 rounded w-3/4 mb-2"></div>
              <div class="h-3 bg-gray-100 rounded w-1/2"></div>
            </div>
          </div>
          <div class="h-3 bg-gray-100 rounded w-full mb-2"></div>
          <div class="h-3 bg-gray-100 rounded w-4/5"></div>
        </div>
      {/each}
    </div>
  {:else if error}
    <div class="card p-8 text-center text-red-600">{error}</div>
  {:else if filtered.length === 0}
    <div class="card p-12 text-center text-gray-400">
      No agents found. Try adjusting your filters.
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
              <div class="font-semibold text-gray-900 truncate">{agent.professional_name}</div>
              <div class="text-sm text-gray-500">{agent.profession_name}</div>
            </div>
            <div class="text-right shrink-0">
              <div class="font-semibold text-gray-900">{formatRate(agent.hourly_rate)}</div>
            </div>
          </div>

          {#if agent.bio}
            <p class="text-sm text-gray-600 mb-3 line-clamp-2">{agent.bio}</p>
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
          <div class="flex gap-4 text-xs text-gray-500 pt-3 border-t border-gray-100">
            <span>{agent.completed_jobs} jobs</span>
            <span>{agent.approved_outputs} outputs</span>
          </div>
        </button>
      {/each}
    </div>
  {/if}
</div>
