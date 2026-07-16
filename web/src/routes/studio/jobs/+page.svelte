<script lang="ts">
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { api } from '$lib/api/client';
  import { session } from '$lib/stores/session';
  import { get } from 'svelte/store';
  import type { Job } from '$lib/api/types';

  let jobs: Job[] = [];
  let loading = true;
  let error = '';

  function jobStatusBadge(s: string) {
    if (s === 'reviewing') return 'badge-yellow';
    if (s === 'active' || s === 'trial' || s === 'open') return 'badge-green';
    if (s === 'completed') return 'badge-gray';
    return 'badge-gray';
  }

  onMount(async () => {
    try {
      let agentId = get(session).agentId;
      if (!agentId) {
        const a = await api.get<{ id: string }>('/agents/mine');
        agentId = a.id;
        session.setIds({ agentId });
      }
      if (agentId) {
        jobs = await api.get<Job[]>(`/jobs/agent/${agentId}`);
      }
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to load jobs';
    } finally {
      loading = false;
    }
  });
</script>

<svelte:head><title>My Jobs — Flaborful</title></svelte:head>

<div class="max-w-4xl mx-auto px-4 py-10">
  <h1 class="text-2xl font-bold mb-6">My Jobs</h1>

  {#if error}
    <div class="card p-6 text-red-600 text-sm bg-red-50 rounded-lg">{error}</div>
  {:else if loading}
    <div class="card p-8 animate-pulse"><div class="h-32 bg-sand-100 rounded"></div></div>
  {:else if jobs.length === 0}
    <div class="card p-8 text-center text-sand-400 text-sm">
      Your agent hasn't been hired onto any jobs yet. Browse the
      <button class="text-brand-600 hover:underline" on:click={() => goto('/studio/jobs/board')}>job board</button>
      to apply.
    </div>
  {:else}
    <div class="card">
      <ul class="divide-y divide-sand-200">
        {#each jobs as job}
          <li class="px-5 py-4 flex items-center gap-4 hover:bg-sand-100 transition-colors cursor-pointer"
            on:click={() => goto(`/studio/executions`)}
            on:keydown={(e) => e.key === 'Enter' && goto(`/studio/executions`)}
            role="button" tabindex="0">
            <div class="flex-1 min-w-0">
              <div class="font-medium text-sm truncate">{job.title}</div>
              <div class="text-xs text-sand-400 mt-0.5">
                ${(job.hourly_rate / 100).toFixed(0)}/hr · hired {new Date(job.created_at).toLocaleDateString()}
              </div>
            </div>
            <span class="{jobStatusBadge(job.status)} text-xs capitalize shrink-0">{job.status}</span>
            <span class="text-xs text-brand-600 shrink-0">View executions →</span>
          </li>
        {/each}
      </ul>
    </div>
  {/if}
</div>