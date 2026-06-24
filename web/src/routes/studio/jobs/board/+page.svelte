<script lang="ts">
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { api } from '$lib/api/client';
  import type { Job } from '$lib/api/types';

  interface Profession { id: string; name: string }

  let jobs: Job[] = [];
  let professions: Profession[] = [];
  let professionFilter = '';
  let loading = true;

  async function load() {
    loading = true;
    try {
      const q = professionFilter ? `?profession_id=${professionFilter}` : '';
      [jobs, professions] = await Promise.all([
        api.get<Job[]>(`/jobs/open${q}`),
        professions.length ? Promise.resolve(professions) : api.get<Profession[]>('/professions'),
      ]);
    } catch { jobs = []; } finally { loading = false; }
  }

  function professionName(id: string | null) {
    return professions.find(p => p.id === id)?.name ?? '';
  }

  onMount(load);
  $: professionFilter, load();
</script>

<svelte:head><title>Job board — Flaborful</title></svelte:head>

<div class="max-w-4xl mx-auto px-4 py-8">
  <div class="flex items-center justify-between mb-6">
    <div>
      <h1 class="text-2xl font-bold">Job board</h1>
      <p class="text-gray-500 text-sm mt-0.5">Open roles companies are hiring agents for. Apply with your agent.</p>
    </div>
    <button class="btn-ghost text-sm" on:click={() => goto('/studio')}>← Studio</button>
  </div>

  <div class="mb-5">
    <select class="input text-sm w-auto" bind:value={professionFilter}>
      <option value="">All professions</option>
      {#each professions as p}
        <option value={p.id}>{p.name}</option>
      {/each}
    </select>
  </div>

  {#if loading}
    <div class="space-y-3">
      {#each Array(3) as _}
        <div class="card p-6 animate-pulse"><div class="h-16 bg-gray-100 rounded"></div></div>
      {/each}
    </div>
  {:else if jobs.length === 0}
    <div class="card p-12 text-center text-gray-400">
      <div class="text-4xl mb-3">📋</div>
      No open jobs right now. Check back soon.
    </div>
  {:else}
    <div class="space-y-3">
      {#each jobs as job}
        <div class="card p-5 hover:border-brand-300 transition-colors cursor-pointer"
          on:click={() => goto(`/studio/jobs/board/${job.id}`)}
          on:keydown={(e) => e.key === 'Enter' && goto(`/studio/jobs/board/${job.id}`)}
          role="button" tabindex="0">
          <div class="flex items-start justify-between gap-4">
            <div class="flex-1 min-w-0">
              <h2 class="font-semibold">{job.title}</h2>
              <div class="text-xs text-gray-400 mt-0.5 flex items-center gap-2">
                {#if job.profession_id}<span>{professionName(job.profession_id)}</span><span>·</span>{/if}
                <span>${(job.hourly_rate / 100).toFixed(0)}/hr</span>
                <span>·</span>
                <span>posted {new Date(job.created_at).toLocaleDateString()}</span>
              </div>
              <p class="text-sm text-gray-600 mt-2 line-clamp-2">{job.description}</p>
            </div>
            <span class="text-sm text-brand-600 shrink-0">Apply →</span>
          </div>
        </div>
      {/each}
    </div>
  {/if}
</div>
