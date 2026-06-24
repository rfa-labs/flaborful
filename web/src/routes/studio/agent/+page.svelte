<script lang="ts">
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { api } from '$lib/api/client';
  import { session } from '$lib/stores/session';
  import { get } from 'svelte/store';
  import type { Agent, KnowledgeChunkView } from '$lib/api/types';

  let agent: Agent | null = null;
  let chunks: KnowledgeChunkView[] = [];
  let agentId = '';
  let loading = true;
  let error = '';

  // Retraining selection
  const SOURCE_TYPES = [
    { key: 'resume', label: 'Resume' },
    { key: 'portfolio', label: 'Portfolio' },
    { key: 'approved_output', label: 'Approved outputs' },
    { key: 'chat_correction', label: 'Chat corrections' },
    { key: 'job_context', label: 'Job context' },
    { key: 'application_material', label: 'Application materials' },
    { key: 'interview_note', label: 'Interview notes' },
  ];
  let selectedTypes = new Set<string>(SOURCE_TYPES.map(t => t.key));
  let preview: { id: string; source_type: string; visibility: string; content_preview: string }[] = [];
  let previewing = false;
  let retraining = false;
  let retrainStarted = false;

  // Execution backend / runtime
  let backend: 'managed' | 'sprite' = 'managed';
  let runtime: 'claude_code' | 'codex' | 'custom' = 'claude_code';
  // Custom-runtime template (install/MCP/invoke), edited as JSON. Only sent when runtime=custom.
  let runtimeSpecText = '';
  let savingExec = false;
  let execSaved = false;

  const CUSTOM_SPEC_PLACEHOLDER = `{
  "install_cmds": ["pip install my-agent"],
  "mcp_config_path": "/root/.config/my-agent/mcp.json",
  "mcp_config_template": "{\\"servers\\":{\\"principal\\":{\\"url\\":\\"{taste_url}\\"}}}",
  "invoke_template": "cd {repo_dir} && my-agent run --prompt {task} --system {persona} 2>&1"
}`;

  async function saveExecution() {
    if (!agentId) return;
    let runtime_spec: unknown = null;
    if (runtime === 'custom' && runtimeSpecText.trim()) {
      try {
        runtime_spec = JSON.parse(runtimeSpecText);
      } catch {
        error = 'Custom runtime spec is not valid JSON.';
        return;
      }
    }
    savingExec = true;
    try {
      await api.patch(`/agents/${agentId}/execution`, {
        execution_backend: backend,
        agent_runtime: runtime,
        runtime_spec,
      });
      execSaved = true;
      setTimeout(() => execSaved = false, 2000);
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to save execution settings';
    } finally {
      savingExec = false;
    }
  }

  // Danger zone
  let resetting = false;
  let showResetConfirm = false;

  $: typeCounts = SOURCE_TYPES.map(t => ({
    ...t,
    count: chunks.filter(c => c.source_type === t.key).length,
  }));

  onMount(async () => {
    try {
      agentId = get(session).agentId ?? (await api.get<{ id: string }>('/agents/mine')).id;
      session.setIds({ agentId });
      [agent, chunks] = await Promise.all([
        api.get<Agent>('/agents/mine'),
        api.get<{ chunks: KnowledgeChunkView[] }>(`/knowledge/agent/${agentId}`).then(r => r.chunks),
      ]);
      if (agent?.execution_backend) backend = agent.execution_backend;
      if (agent?.agent_runtime) runtime = agent.agent_runtime;
      if (agent?.runtime_spec) runtimeSpecText = JSON.stringify(agent.runtime_spec, null, 2);
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to load agent';
    } finally {
      loading = false;
    }
  });

  function filterBody() {
    return { source_types: Array.from(selectedTypes) };
  }

  async function doPreview() {
    previewing = true;
    try {
      preview = await api.post(`/agents/${agentId}/retrain/preview`, filterBody());
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Preview failed';
    } finally {
      previewing = false;
    }
  }

  async function startRetrain() {
    if (selectedTypes.size === 0) { error = 'Select at least one knowledge source.'; return; }
    retraining = true; error = '';
    try {
      await api.post(`/agents/${agentId}/retrain`, filterBody());
      retrainStarted = true;
      if (agent) agent = { ...agent, status: 'training' };
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Retrain failed';
    } finally {
      retraining = false;
    }
  }

  async function resetAgent() {
    resetting = true; error = '';
    try {
      await api.post(`/agents/${agentId}/reset`, {});
      goto('/studio/onboarding');
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Reset failed';
      resetting = false;
    }
  }

  function toggleType(key: string) {
    if (selectedTypes.has(key)) selectedTypes.delete(key);
    else selectedTypes.add(key);
    selectedTypes = new Set(selectedTypes);
    preview = [];
  }
</script>

<svelte:head><title>Agent management — Flaborful</title></svelte:head>

<div class="max-w-3xl mx-auto px-4 py-8">
  <button class="btn-ghost text-sm mb-6" on:click={() => goto('/studio')}>← Studio</button>

  <h1 class="text-2xl font-bold mb-1">Agent management</h1>
  <p class="text-gray-500 text-sm mb-6">Retrain your agent from selected knowledge, or rebuild it from scratch.</p>

  {#if loading}
    <div class="card p-8 animate-pulse"><div class="h-32 bg-gray-100 rounded"></div></div>
  {:else if error && !agent}
    <div class="card p-8 text-center text-red-600">{error}</div>
  {:else if agent}
    <!-- Status -->
    <div class="card p-5 mb-6 flex items-center justify-between">
      <div>
        <div class="text-xs text-gray-500 uppercase tracking-wide">Agent status</div>
        <div class="text-lg font-semibold capitalize">{agent.status}</div>
      </div>
      {#if agent.status === 'training'}
        <span class="badge-yellow">Training in progress</span>
      {:else if agent.status === 'ready' || agent.status === 'deployed'}
        <span class="badge-green capitalize">{agent.status}</span>
      {/if}
    </div>

    <!-- Execution backend / runtime -->
    <div class="card p-5 mb-6">
      <h2 class="font-semibold mb-1">Where your agent does work</h2>
      <p class="text-xs text-gray-400 mb-4">For coding jobs, your agent runs on a real machine. Pick where it runs and which coding tool drives it. (A workflow can override this per task.)</p>

      <div class="space-y-3">
        <div>
          <label class="label text-xs" for="backend">Execution backend</label>
          <select class="input text-sm" id="backend" bind:value={backend}>
            <option value="managed">Managed (hosted — fastest, no setup)</option>
            <option value="sprite">Sprite (your own VM — full control of the test environment)</option>
          </select>
        </div>

        {#if backend === 'sprite'}
          <div>
            <label class="label text-xs" for="runtime">Coding agent runtime</label>
            <select class="input text-sm" id="runtime" bind:value={runtime}>
              <option value="claude_code">Claude Code</option>
              <option value="codex">Codex</option>
              <option value="custom">Custom</option>
            </select>
            <p class="text-xs text-gray-400 mt-1">We install your chosen tool on the sprite and point it at your style model.</p>
          </div>

          {#if runtime === 'custom'}
            <div>
              <label class="label text-xs" for="runtime-spec">Custom runtime spec (JSON)</label>
              <textarea class="input text-sm font-mono h-40" id="runtime-spec"
                bind:value={runtimeSpecText} placeholder={CUSTOM_SPEC_PLACEHOLDER}></textarea>
              <p class="text-xs text-gray-400 mt-1">
                Templates: <span class="font-mono">{'{taste_url}'}</span> in the MCP config;
                <span class="font-mono">{'{repo_dir}'}</span>, <span class="font-mono">{'{task}'}</span>,
                <span class="font-mono">{'{persona}'}</span> in the invoke command.
              </p>
            </div>
          {/if}
        {:else}
          <p class="text-xs text-gray-400">Managed runs Claude Code on Anthropic's hosted container; your repo token stays off the machine.</p>
        {/if}
      </div>

      <div class="flex justify-end mt-3">
        <button class="btn-secondary text-sm" disabled={savingExec} on:click={saveExecution}>
          {savingExec ? 'Saving…' : execSaved ? '✓ Saved' : 'Save'}
        </button>
      </div>
    </div>

    <!-- Retraining -->
    <div class="card p-5 mb-6">
      <h2 class="font-semibold mb-1">Retrain from knowledge base</h2>
      <p class="text-xs text-gray-400 mb-4">Choose which parts of your knowledge base to train on. Your full KB is preserved either way.</p>

      <div class="space-y-2 mb-4">
        {#each typeCounts as t}
          <label class="flex items-center gap-2.5 text-sm cursor-pointer">
            <input type="checkbox" checked={selectedTypes.has(t.key)} on:change={() => toggleType(t.key)} />
            <span class="flex-1">{t.label}</span>
            <span class="text-xs text-gray-400">{t.count} item{t.count === 1 ? '' : 's'}</span>
          </label>
        {/each}
      </div>

      {#if retrainStarted}
        <div class="bg-green-50 text-green-800 text-sm rounded-lg px-3 py-2 mb-3">
          Retraining started. Your agent's status will update to Ready when the new adapter is live.
        </div>
      {/if}
      {#if error}<div class="text-red-600 text-sm bg-red-50 rounded-lg px-3 py-2 mb-3">{error}</div>{/if}

      <div class="flex gap-2">
        <button class="btn-secondary text-sm" disabled={previewing || selectedTypes.size === 0} on:click={doPreview}>
          {previewing ? 'Loading…' : 'Preview dataset'}
        </button>
        <button class="btn-primary text-sm" disabled={retraining || agent.status === 'training' || selectedTypes.size === 0} on:click={startRetrain}>
          {retraining ? 'Starting…' : 'Start retraining'}
        </button>
      </div>

      {#if preview.length > 0}
        <div class="mt-4 border-t border-gray-100 pt-4">
          <div class="text-xs font-semibold text-gray-500 mb-2">{preview.length} training example{preview.length === 1 ? '' : 's'} selected</div>
          <div class="space-y-1.5 max-h-72 overflow-y-auto">
            {#each preview as p}
              <div class="bg-gray-50 rounded p-2 text-xs">
                <span class="text-[10px] uppercase tracking-wide text-gray-400">{p.source_type.replace(/_/g, ' ')}</span>
                <div class="text-gray-700 line-clamp-2">{p.content_preview}</div>
              </div>
            {/each}
          </div>
        </div>
      {/if}
    </div>

    <!-- Danger zone -->
    <div class="card p-5 border-red-200 bg-red-50">
      <h2 class="font-semibold text-red-800 mb-1">Delete & rebuild agent</h2>
      <p class="text-sm text-red-600 mb-3">
        Deletes the trained agent (its LoRA adapter) and returns you to onboarding to rebuild from scratch.
        <strong>Your knowledge base is preserved</strong> — only the trained model is removed.
      </p>
      {#if !showResetConfirm}
        <button class="btn-danger text-sm" on:click={() => showResetConfirm = true}>Delete & rebuild</button>
      {:else}
        <div class="flex items-center gap-2">
          <span class="text-sm text-red-700">Are you sure?</span>
          <button class="btn-danger text-sm" disabled={resetting} on:click={resetAgent}>
            {resetting ? 'Resetting…' : 'Yes, delete & rebuild'}
          </button>
          <button class="btn-ghost text-sm" on:click={() => showResetConfirm = false}>Cancel</button>
        </div>
      {/if}
    </div>
  {/if}
</div>
