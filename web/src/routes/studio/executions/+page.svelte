<script lang="ts">
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { api } from '$lib/api/client';
  import { session } from '$lib/stores/session';
  import { get } from 'svelte/store';
  import type { Task, Workflow } from '$lib/api/types';
  import StatusBadge from '$lib/components/StatusBadge.svelte';

  let tasks: Task[] = [];
  let workflows: Workflow[] = [];
  let loading = true;
  let error = '';
  // Per-task selected workflow id (for pending tasks awaiting assignment) + in-flight flag.
  let pickWorkflow: Record<string, string> = {};
  let assigning: Record<string, boolean> = {};
  let assignError = '';

  onMount(async () => {
    try {
      const agentId = get(session).agentId ?? (await api.get<{ id: string }>('/agents/mine')).id;
      session.setIds({ agentId });
      [tasks, workflows] = await Promise.all([
        api.get<Task[]>(`/tasks/agent/${agentId}/active`),
        api.get<Workflow[]>(`/workflows/agent/${agentId}`),
      ]);
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to load executions';
    } finally {
      loading = false;
    }
  });

  async function assignWorkflow(taskId: string) {
    const workflowId = pickWorkflow[taskId];
    if (!workflowId) return;
    assigning[taskId] = true; assignError = '';
    try {
      // Assigning a workflow to a pending task triggers its execution.
      const updated = await api.patch<Task>(`/tasks/${taskId}/workflow`, { workflow_id: workflowId });
      tasks = tasks.map((t) => (t.id === taskId ? updated : t));
    } catch (e: unknown) {
      assignError = e instanceof Error ? e.message : 'Failed to assign workflow';
    } finally {
      assigning[taskId] = false;
    }
  }

  // Re-dispatch a task that already has a workflow (e.g. after a kill/refine). The company's task
  // is preserved across attempts — no need to recreate it.
  async function runAgain(taskId: string) {
    assigning[taskId] = true; assignError = '';
    try {
      // If the professional re-picked a different workflow, switch it first.
      const newWf = pickWorkflow[taskId];
      if (newWf) await api.patch<Task>(`/tasks/${taskId}/workflow`, { workflow_id: newWf });
      await api.post<Task>(`/tasks/${taskId}/run`, {});
      goto(`/studio/executions/${taskId}`);
    } catch (e: unknown) {
      assignError = e instanceof Error ? e.message : 'Failed to run';
    } finally {
      assigning[taskId] = false;
    }
  }

  // Build a fresh workflow for this task: create a blank one and open the builder, passing the
  // task id so the builder can assign-and-run it once the professional is done.
  async function createWorkflowForTask(taskId: string) {
    assignError = '';
    try {
      const agentId = get(session).agentId;
      const wf = await api.post<{ id: string }>('/workflows', {
        agent_id: agentId,
        name: 'New Workflow',
        description: '',
        graph: { nodes: [], edges: [] },
        is_universal: false,
      });
      goto(`/studio/workflows/${wf.id}?assignTask=${taskId}`);
    } catch (e: unknown) {
      assignError = e instanceof Error ? e.message : 'Failed to create workflow';
    }
  }

  function timeAgo(iso: string) {
    const diff = Date.now() - new Date(iso).getTime();
    const m = Math.floor(diff / 60000);
    if (m < 1) return 'just now';
    if (m < 60) return `${m}m ago`;
    const h = Math.floor(m / 60);
    if (h < 24) return `${h}h ago`;
    return `${Math.floor(h / 24)}d ago`;
  }

  function taskTitle(t: Task): string {
    const input = t.input as Record<string, unknown> | null;
    if (input && typeof input === 'object' && typeof input.description === 'string') {
      return input.description;
    }
    return `Task ${t.id.slice(0, 8)}`;
  }

  function repoOf(t: Task): string {
    const target = t.target as Record<string, unknown> | null;
    return target && typeof target.repo === 'string' ? target.repo : '';
  }
</script>

<svelte:head><title>Executions — Flaborful Studio</title></svelte:head>

<div class="max-w-4xl mx-auto px-4 py-8">
  <button class="btn-ghost text-sm mb-6" on:click={() => goto('/studio')}>← Studio</button>

  <h1 class="text-2xl font-bold mb-1">Workflow executions</h1>
  <p class="text-gray-500 text-sm mb-6">Live runs of your agent's workflows. Open one to watch its progress and steer the work.</p>

  {#if loading}
    <div class="space-y-3">
      {#each Array(3) as _}
        <div class="card p-5 animate-pulse"><div class="h-12 bg-gray-100 rounded"></div></div>
      {/each}
    </div>
  {:else if error}
    <div class="card p-8 text-center text-red-600">{error}</div>
  {:else if tasks.length === 0}
    <div class="card p-12 text-center text-gray-400">
      <div class="text-4xl mb-3">⚙️</div>
      No active executions. Tasks your agent is running will appear here.
    </div>
  {:else}
    {#if assignError}
      <p class="text-red-600 text-sm mb-3">{assignError}</p>
    {/if}
    <div class="space-y-3">
      {#each tasks as task (task.id)}
        {@const needsWorkflow = task.status === 'pending' && !task.workflow_id}
        {@const reRunnable = (task.status === 'pending' || task.status === 'failed') && !!task.workflow_id}
        {#if reRunnable}
          <!-- Task already has a workflow but isn't running (e.g. after a kill). The company's
               task is intact — the professional just runs it again, optionally re-picking a workflow. -->
          <div class="card p-5 {task.status === 'failed' ? 'border-red-200' : 'border-blue-200'}">
            <div class="flex items-start gap-4">
              <div class="flex-1 min-w-0">
                <div class="font-medium text-sm">{taskTitle(task)}</div>
                <div class="text-xs text-gray-400 mt-0.5">
                  created {timeAgo(task.created_at)}{repoOf(task) ? ` · ${repoOf(task)}` : ''}
                  · {task.status === 'failed' ? 'last run failed' : 'ready to run'}
                </div>
                {#if task.status === 'failed'}
                  <button class="text-xs text-red-600 underline mt-1" on:click={() => goto(`/studio/executions/${task.id}`)}>
                    View what went wrong →
                  </button>
                {/if}
                <div class="mt-3 flex items-center gap-2 flex-wrap">
                  <button class="btn-primary text-sm shrink-0"
                    disabled={assigning[task.id]}
                    on:click={() => runAgain(task.id)}>
                    {assigning[task.id] ? 'Starting…' : '▶ Run again'}
                  </button>
                  <span class="text-xs text-gray-400">or switch workflow:</span>
                  <select class="input text-sm py-1" bind:value={pickWorkflow[task.id]}>
                    <option value="">— Keep current —</option>
                    {#each workflows as wf}
                      <option value={wf.id}>{wf.name}</option>
                    {/each}
                  </select>
                  <button class="btn-ghost text-sm shrink-0" on:click={() => createWorkflowForTask(task.id)}>
                    + Build new
                  </button>
                </div>
              </div>
              <StatusBadge status={task.status} />
            </div>
          </div>
        {:else if needsWorkflow}
          <!-- Pending task with no workflow: the professional assigns one (which starts the run). -->
          <div class="card p-5 border-amber-300">
            <div class="flex items-start gap-4">
              <div class="flex-1 min-w-0">
                <div class="font-medium text-sm">{taskTitle(task)}</div>
                <div class="text-xs text-gray-400 mt-0.5">
                  created {timeAgo(task.created_at)}{repoOf(task) ? ` · ${repoOf(task)}` : ''}
                </div>
                <div class="mt-3 flex items-center gap-2 flex-wrap">
                  <select class="input text-sm py-1" bind:value={pickWorkflow[task.id]}>
                    <option value="">— Choose a workflow to run —</option>
                    {#each workflows as wf}
                      <option value={wf.id}>{wf.name}</option>
                    {/each}
                  </select>
                  <button class="btn-primary text-sm shrink-0"
                    disabled={!pickWorkflow[task.id] || assigning[task.id]}
                    on:click={() => assignWorkflow(task.id)}>
                    {assigning[task.id] ? 'Starting…' : 'Assign & run'}
                  </button>
                  <span class="text-xs text-gray-400">or</span>
                  <button class="btn-ghost text-sm shrink-0" on:click={() => createWorkflowForTask(task.id)}>
                    + Build a new workflow for this task
                  </button>
                </div>
                {#if workflows.length === 0}
                  <p class="text-xs text-gray-400 mt-2">
                    No workflows yet — build a new one for this task above.
                  </p>
                {/if}
              </div>
              <StatusBadge status={task.status} />
            </div>
          </div>
        {:else}
          <div class="card p-5 hover:border-brand-300 transition-colors cursor-pointer"
            on:click={() => goto(`/studio/executions/${task.id}`)}
            on:keydown={(e) => e.key === 'Enter' && goto(`/studio/executions/${task.id}`)}
            role="button" tabindex="0">
            <div class="flex items-center gap-4">
              <div class="flex-1 min-w-0">
                <div class="font-medium text-sm truncate">{taskTitle(task)}</div>
                <div class="text-xs text-gray-400 mt-0.5">
                  started {timeAgo(task.created_at)}
                  {#if task.current_node_id}· at node <span class="font-mono">{task.current_node_id}</span>{/if}
                </div>
              </div>
              <StatusBadge status={task.status} />
              <span class="text-sm text-brand-600 shrink-0">View →</span>
            </div>
          </div>
        {/if}
      {/each}
    </div>
  {/if}
</div>
