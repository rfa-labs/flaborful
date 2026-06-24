<script lang="ts">
  import { onMount } from 'svelte';
  import { page } from '$app/stores';
  import { goto } from '$app/navigation';
  import { api } from '$lib/api/client';
  import { session } from '$lib/stores/session';
  import type { Task, Workflow } from '$lib/api/types';

  let task: Task | null = null;
  let loading = true;
  let submitting = false;
  let error = '';
  let editedOutput = '';
  let amended = false;

  // Workflow assignment (for tasks with no workflow yet)
  let workflows: Workflow[] = [];
  let selectedWorkflowId = '';
  let assigning = false;

  $: taskId = $page.params.id;

  onMount(async () => {
    try {
      task = await api.get<Task>(`/tasks/${taskId}`);
      editedOutput = task.output_draft ?? '';
      if (!task.workflow_id && $session.agentId) {
        workflows = await api.get<Workflow[]>(`/workflows/agent/${$session.agentId}`);
      }
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to load task';
    } finally {
      loading = false;
    }
  });

  async function assignAndRun() {
    if (!selectedWorkflowId || !task) return;
    assigning = true;
    try {
      task = await api.patch<Task>(`/tasks/${task.id}/workflow`, { workflow_id: selectedWorkflowId });
      error = '';
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to assign workflow';
    } finally {
      assigning = false;
    }
  }

  // Build a fresh workflow for this task: create a blank one, open the builder, and pass the task
  // id so the builder can assign-and-run it once it's built.
  async function createWorkflowForTask() {
    if (!task) return;
    assigning = true;
    try {
      let agentId = $session.agentId;
      if (!agentId) {
        agentId = (await api.get<{ id: string }>('/agents/mine')).id;
        session.setIds({ agentId });
      }
      const wf = await api.post<{ id: string }>('/workflows', {
        agent_id: agentId,
        name: 'New Workflow',
        description: '',
        graph: { nodes: [], edges: [] },
        is_universal: false,
      });
      goto(`/studio/workflows/${wf.id}?assignTask=${task.id}`);
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to create workflow';
      assigning = false;
    }
  }

  $: amended = task ? editedOutput !== (task.output_draft ?? '') : false;

  async function approve() {
    if (!task) return;
    submitting = true;
    try {
      await api.post(`/tasks/${task.id}/approve`, {
        approved_output: editedOutput,
        amended,
      });
      goto('/studio');
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to approve';
    } finally {
      submitting = false;
    }
  }

  async function reject() {
    if (!task) return;
    submitting = true;
    try {
      // Send back to in-progress; the professional fixes via the execution view (Chat → Rerun).
      await api.post(`/tasks/${task.id}/reject`, {});
      goto(`/studio/executions/${task.id}`);
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to reject';
    } finally {
      submitting = false;
    }
  }

  function confidenceBg(score: number | null) {
    if (score == null) return 'bg-gray-100 text-gray-600';
    if (score >= 0.9) return 'bg-green-100 text-green-800';
    if (score >= 0.7) return 'bg-yellow-100 text-yellow-800';
    return 'bg-red-100 text-red-800';
  }
</script>

<svelte:head><title>Review task — Flaborful</title></svelte:head>

<div class="max-w-5xl mx-auto px-4 py-8">
  <div class="flex items-center gap-3 mb-6">
    <button class="btn-ghost text-sm" on:click={() => goto('/studio')}>← Back</button>
    <h1 class="text-xl font-bold">Review agent output</h1>
  </div>

  {#if loading}
    <div class="card p-8 animate-pulse"><div class="h-48 bg-gray-100 rounded"></div></div>
  {:else if error}
    <div class="card p-8 text-center text-red-600">{error}</div>
  {:else if task}
    <div class="grid lg:grid-cols-3 gap-6">
      <!-- Main editor -->
      <div class="lg:col-span-2 space-y-4">
        <!-- Task input -->
        <div class="card p-5">
          <h2 class="text-sm font-semibold text-gray-500 uppercase tracking-wide mb-3">Task input</h2>
          <pre class="text-sm text-gray-700 whitespace-pre-wrap bg-gray-50 rounded-lg p-4 font-mono">{JSON.stringify(task.input, null, 2)}</pre>
        </div>

        <!-- Agent's draft -->
        <div class="card p-5">
          <div class="flex items-center justify-between mb-3">
            <h2 class="text-sm font-semibold text-gray-500 uppercase tracking-wide">
              Agent output {#if amended}<span class="badge-yellow ml-2">edited</span>{/if}
            </h2>
            {#if task.confidence_score != null}
              <span class="px-2 py-1 rounded-full text-xs font-medium {confidenceBg(task.confidence_score)}">
                {Math.round(task.confidence_score * 100)}% confidence
              </span>
            {/if}
          </div>
          <textarea
            class="w-full min-h-[300px] font-mono text-sm border border-gray-200 rounded-lg p-4 focus:outline-none focus:ring-2 focus:ring-brand-500 resize-y"
            bind:value={editedOutput}
            placeholder="Agent output will appear here..."
          ></textarea>
          {#if amended}
            <p class="text-xs text-amber-600 mt-2">
              ⚠ You've edited the output — this will be recorded as an amendment and fed back into training.
            </p>
          {/if}
        </div>

        <!-- Workflow assignment (if no workflow assigned yet) -->
        {#if task && !task.workflow_id && task.status === 'pending'}
          <div class="card p-5 border-amber-200 bg-amber-50">
            <h3 class="text-sm font-semibold text-amber-800 mb-2">No workflow assigned</h3>
            <p class="text-xs text-amber-700 mb-3">This task has no workflow. Select one to start execution.</p>
            <select class="input text-sm mb-3" bind:value={selectedWorkflowId}>
              <option value="">— Select a workflow —</option>
              {#each workflows as wf}
                <option value={wf.id}>{wf.name}</option>
              {/each}
            </select>
            <button class="btn-primary text-sm w-full" disabled={!selectedWorkflowId || assigning} on:click={assignAndRun}>
              {assigning ? 'Assigning...' : 'Assign & execute'}
            </button>
            <div class="flex items-center gap-2 my-3">
              <div class="flex-1 h-px bg-amber-200"></div>
              <span class="text-xs text-amber-700">or</span>
              <div class="flex-1 h-px bg-amber-200"></div>
            </div>
            <button class="btn-secondary text-sm w-full" disabled={assigning} on:click={createWorkflowForTask}>
              + Build a new workflow for this task
            </button>
          </div>
        {/if}

        <!-- Actions -->
        <div class="flex gap-3">
          <button
            class="btn-primary flex-1 py-3"
            disabled={submitting || !task?.output_draft}
            on:click={approve}>
            {#if submitting}Approving...{:else if amended}Approve (with amendments){:else}Approve & deliver{/if}
          </button>
          <button class="btn-secondary px-6" disabled={submitting} on:click={reject}>
            Reject
          </button>
        </div>
      </div>

      <!-- Sidebar -->
      <div class="space-y-4">
        <div class="card p-4">
          <h3 class="text-sm font-semibold text-gray-700 mb-3">Task details</h3>
          <dl class="space-y-2 text-sm">
            <div class="flex justify-between">
              <dt class="text-gray-500">Status</dt>
              <dd class="font-medium capitalize">{task.status.replace('_', ' ')}</dd>
            </div>
            <div class="flex justify-between">
              <dt class="text-gray-500">Created</dt>
              <dd class="font-medium">{new Date(task.created_at).toLocaleDateString()}</dd>
            </div>
            {#if task.confidence_score != null}
              <div class="flex justify-between">
                <dt class="text-gray-500">Confidence</dt>
                <dd class="font-medium">{Math.round(task.confidence_score * 100)}%</dd>
              </div>
            {/if}
          </dl>
        </div>

        <div class="card p-4">
          <h3 class="text-sm font-semibold text-gray-700 mb-2">About approvals</h3>
          <p class="text-xs text-gray-500 leading-relaxed">
            Every approval teaches your agent. Edits you make are recorded as preference pairs
            and fed back into the LoRA training cycle — the agent gets better with each correction.
          </p>
        </div>
      </div>
    </div>
  {/if}
</div>
