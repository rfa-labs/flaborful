<script lang="ts">
  // Run-log browser: a cross-run history of every execution for this agent, newest first. The
  // per-node data lives in node_executions; this is the top-level index that drills into each run.
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { api } from '$lib/api/client';
  import { session } from '$lib/stores/session';
  import { get } from 'svelte/store';

  interface Run {
    task_id: string;
    job_title: string;
    status: string;
    node_count: number;
    created_at: string;
    delivered_at: string | null;
    grade: string | null;
  }

  let runs: Run[] = [];
  let loading = true;
  let error = '';

  const STATUS_BADGE: Record<string, string> = {
    delivered: 'badge-green',
    failed: 'badge-red',
    awaiting_review: 'badge-yellow',
    awaiting_effect: 'badge-yellow',
    running: 'badge-blue',
    pending: 'badge-gray',
  };
  const GRADE_BADGE: Record<string, string> = { pass: 'badge-green', warning: 'badge-yellow', critical: 'badge-red' };

  onMount(async () => {
    try {
      let id = get(session).agentId;
      if (!id) { id = (await api.get<{ id: string }>('/agents/mine')).id; session.setIds({ agentId: id }); }
      runs = await api.get<Run[]>(`/tasks/agent/${id}/runs`);
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to load runs';
    } finally {
      loading = false;
    }
  });

  function duration(r: Run): string {
    if (!r.delivered_at) return '—';
    const ms = new Date(r.delivered_at).getTime() - new Date(r.created_at).getTime();
    const s = Math.round(ms / 1000);
    if (s < 60) return `${s}s`;
    const m = Math.floor(s / 60);
    return `${m}m ${s % 60}s`;
  }
</script>

<svelte:head><title>Run log — Flaborful</title></svelte:head>

<div class="max-w-5xl mx-auto px-4 py-8">
  <div class="mb-6">
    <h1 class="text-2xl font-bold">Run log</h1>
    <p class="text-sand-500 text-sm mt-0.5">Every execution your agent has run. Click one to inspect its nodes.</p>
  </div>

  {#if loading}
    <div class="card p-8 animate-pulse"><div class="h-40 bg-sand-100 rounded"></div></div>
  {:else if error}
    <div class="card p-6 text-red-600 text-sm">{error}</div>
  {:else if runs.length === 0}
    <div class="card p-12 text-center text-sand-400">
      No runs yet. When your agent is hired and executes a workflow, its runs appear here.
    </div>
  {:else}
    <div class="card overflow-hidden">
      <table class="w-full text-sm">
        <thead class="bg-sand-50 text-sand-500 text-xs uppercase tracking-wide">
          <tr>
            <th class="text-left font-medium px-4 py-2.5">Job</th>
            <th class="text-left font-medium px-4 py-2.5">Status</th>
            <th class="text-left font-medium px-4 py-2.5">Eval</th>
            <th class="text-right font-medium px-4 py-2.5">Nodes</th>
            <th class="text-right font-medium px-4 py-2.5">Duration</th>
            <th class="text-right font-medium px-4 py-2.5">When</th>
          </tr>
        </thead>
        <tbody class="divide-y divide-sand-100">
          {#each runs as r (r.task_id)}
            <tr class="hover:bg-sand-50 cursor-pointer" on:click={() => goto(`/studio/executions/${r.task_id}`)}>
              <td class="px-4 py-2.5 font-medium truncate max-w-[220px]">{r.job_title}</td>
              <td class="px-4 py-2.5"><span class="{STATUS_BADGE[r.status] ?? 'badge-gray'} text-xs capitalize">{r.status.replace(/_/g, ' ')}</span></td>
              <td class="px-4 py-2.5">
                {#if r.grade}<span class="{GRADE_BADGE[r.grade] ?? 'badge-gray'} text-xs capitalize">{r.grade}</span>{:else}<span class="text-sand-300">—</span>{/if}
              </td>
              <td class="px-4 py-2.5 text-right text-sand-500">{r.node_count}</td>
              <td class="px-4 py-2.5 text-right text-sand-500">{duration(r)}</td>
              <td class="px-4 py-2.5 text-right text-sand-400 text-xs">{new Date(r.created_at).toLocaleString()}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</div>
