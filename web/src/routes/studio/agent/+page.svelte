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
  // Bring-your-own Anthropic key. We never receive the stored key back — only a presence flag.
  let anthropicKey = '';          // the new key the user is typing (blank = leave unchanged)
  let anthropicKeySet = false;    // true if a key is already on file
  let replacingKey = false;       // user chose to replace an on-file key

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
      // Only send the key when the user actually typed one — omitting it leaves the stored key
      // untouched (an empty string would clear it server-side).
      const payload: Record<string, unknown> = {
        execution_backend: backend,
        agent_runtime: runtime,
        runtime_spec,
      };
      if (anthropicKey.trim()) payload.anthropic_api_key = anthropicKey.trim();
      await api.patch(`/agents/${agentId}/execution`, payload);
      if (anthropicKey.trim()) {
        anthropicKeySet = true;
        anthropicKey = '';
        replacingKey = false;
      }
      execSaved = true;
      setTimeout(() => execSaved = false, 2000);
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to save execution settings';
    } finally {
      savingExec = false;
    }
  }

  async function removeAnthropicKey() {
    if (!agentId) return;
    savingExec = true;
    try {
      // Explicit empty string clears the stored key server-side.
      await api.patch(`/agents/${agentId}/execution`, {
        execution_backend: backend,
        agent_runtime: runtime,
        anthropic_api_key: '',
      });
      anthropicKeySet = false;
      anthropicKey = '';
      replacingKey = false;
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to remove key';
    } finally {
      savingExec = false;
    }
  }

  // Persona / instructions
  let personaText = '';
  let savingPersona = false;
  let personaSaved = false;

  async function savePersona() {
    if (!agentId) return;
    savingPersona = true;
    try {
      await api.patch(`/agents/${agentId}/persona`, { persona_system_prompt: personaText });
      personaSaved = true;
      setTimeout(() => personaSaved = false, 2000);
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to save instructions';
    } finally {
      savingPersona = false;
    }
  }

  // Evaluation criteria — reusable pass/fail rules graded on each delivered task.
  interface Criterion { id: string; name: string; prompt: string; priority: string }
  let criteria: Criterion[] = [];
  let newCriterion = { name: '', prompt: '', priority: 'warning' };
  let addingCriterion = false;

  async function loadCriteria() {
    try {
      criteria = await api.get<Criterion[]>(`/evaluations/agent/${agentId}/criteria`);
    } catch { criteria = []; }
  }
  async function addCriterion() {
    if (!newCriterion.name.trim() || !newCriterion.prompt.trim()) return;
    addingCriterion = true;
    try {
      await api.post(`/evaluations/agent/${agentId}/criteria`, newCriterion);
      newCriterion = { name: '', prompt: '', priority: 'warning' };
      await loadCriteria();
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to add criterion';
    } finally {
      addingCriterion = false;
    }
  }
  async function deleteCriterion(id: string) {
    try {
      await api.delete(`/evaluations/agent/${agentId}/criteria/${id}`);
      await loadCriteria();
    } catch { /* ignore */ }
  }

  // Reflections — the agent's self-improvement proposals awaiting review.
  interface Proposal { id: string; kind: string; title: string; rationale: string; payload: unknown; status: string; created_at: string }
  let proposals: Proposal[] = [];
  let reflecting = false;
  let applyingId = '';

  const KIND_LABEL: Record<string, string> = {
    new_skill: 'New skill',
    instruction_update: 'Instruction',
    skill_fix: 'Skill fix',
    tool_access: 'Tool access',
  };

  async function loadProposals() {
    try {
      proposals = await api.get<Proposal[]>(`/reflections/agent/${agentId}`);
    } catch { proposals = []; }
  }
  async function reflectNow() {
    reflecting = true;
    try {
      await api.post(`/reflections/agent/${agentId}`, {});
      await loadProposals();
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Reflection failed';
    } finally {
      reflecting = false;
    }
  }
  async function applyProposal(id: string) {
    applyingId = id;
    try {
      await api.post(`/reflections/agent/${agentId}/${id}/apply`, {});
      await Promise.all([loadProposals(), reloadPersona()]);
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to apply';
    } finally {
      applyingId = '';
    }
  }
  async function dismissProposal(id: string) {
    try {
      await api.post(`/reflections/agent/${agentId}/${id}/dismiss`, {});
      await loadProposals();
    } catch { /* ignore */ }
  }
  // After applying an instruction_update, refresh the persona box so the change shows.
  async function reloadPersona() {
    try {
      const a = await api.get<Agent>('/agents/mine');
      personaText = a?.persona_system_prompt ?? personaText;
    } catch { /* ignore */ }
  }

  // Tool-approval policy — pause before the agent takes an external write action.
  let policyMode = 'always_allow';
  let savingPolicy = false;
  let policySaved = false;
  const POLICY_MODES = [
    { key: 'always_allow', label: 'Auto — no approval', hint: 'The agent acts without pausing (default).' },
    { key: 'ask_on_writes', label: 'Approve every action', hint: 'Pause for your OK before any external write (PR, comment, email, doc).' },
  ];
  async function loadPolicy() {
    try {
      const p = await api.get<{ mode: string }>(`/agents/${agentId}/tool-policy`);
      policyMode = p.mode ?? 'always_allow';
    } catch { /* default */ }
  }
  async function savePolicy() {
    savingPolicy = true;
    try {
      await api.put(`/agents/${agentId}/tool-policy`, { mode: policyMode, rules: {} });
      policySaved = true;
      setTimeout(() => policySaved = false, 2000);
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to save policy';
    } finally {
      savingPolicy = false;
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
      anthropicKeySet = agent?.anthropic_key_set ?? false;
      personaText = agent?.persona_system_prompt ?? '';
      loadCriteria();
      loadProposals();
      loadPolicy();
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
  <p class="text-sand-500 text-sm mb-6">Retrain your agent from selected knowledge, or rebuild it from scratch.</p>

  {#if loading}
    <div class="card p-8 animate-pulse"><div class="h-32 bg-sand-100 rounded"></div></div>
  {:else if error && !agent}
    <div class="card p-8 text-center text-red-600">{error}</div>
  {:else if agent}
    <!-- Status -->
    <div class="card p-5 mb-6 flex items-center justify-between">
      <div>
        <div class="text-xs text-sand-500 uppercase tracking-wide">Agent status</div>
        <div class="text-lg font-semibold capitalize">{agent.status}</div>
      </div>
      {#if agent.status === 'training'}
        <span class="badge-yellow">Training in progress</span>
      {:else if agent.status === 'ready' || agent.status === 'deployed'}
        <span class="badge-green capitalize">{agent.status}</span>
      {/if}
    </div>

    <!-- Instructions (persona system prompt) -->
    <div class="card p-5 mb-6">
      <h2 class="font-semibold mb-1">Agent instructions</h2>
      <p class="text-xs text-sand-500 mb-3">
        Your agent's standing system prompt — how it should write and behave. Auto-generated from
        your work at onboarding; refine it here. Your agent can also propose edits (see suggestions below).
      </p>
      <textarea class="input text-sm min-h-[140px] font-mono text-xs" bind:value={personaText}
        placeholder="Describe how your agent should approach work, in your voice…"></textarea>
      <div class="flex justify-end mt-2">
        <button class="btn-secondary text-sm" disabled={savingPersona} on:click={savePersona}>
          {savingPersona ? 'Saving…' : personaSaved ? '✓ Saved' : 'Save instructions'}
        </button>
      </div>
    </div>

    <!-- Reflections review queue -->
    <div class="card p-5 mb-6">
      <div class="flex items-start justify-between mb-1">
        <h2 class="font-semibold">Suggestions from your agent</h2>
        <button class="btn-ghost text-xs shrink-0" disabled={reflecting} on:click={reflectNow}>
          {reflecting ? 'Reflecting…' : 'Reflect now'}
        </button>
      </div>
      <p class="text-xs text-sand-500 mb-3">
        Your agent periodically reviews its work and proposes improvements. Nothing changes until you approve it.
      </p>

      {#if proposals.length === 0}
        <div class="text-sm text-sand-400 text-center py-4">
          No suggestions right now. They appear here after your agent completes some work.
        </div>
      {:else}
        <ul class="space-y-2">
          {#each proposals as p (p.id)}
            <li class="border border-sand-200 rounded-lg p-3">
              <div class="flex items-start gap-2 mb-1">
                <span class="badge-purple text-xs mt-0.5 shrink-0">{KIND_LABEL[p.kind] ?? p.kind}</span>
                <span class="text-sm font-medium flex-1">{p.title}</span>
              </div>
              <p class="text-xs text-sand-500 mb-2">{p.rationale}</p>
              <div class="flex gap-2">
                <button class="btn-primary text-xs" disabled={applyingId === p.id} on:click={() => applyProposal(p.id)}>
                  {applyingId === p.id ? 'Applying…' : 'Approve'}
                </button>
                <button class="btn-ghost text-xs" on:click={() => dismissProposal(p.id)}>Dismiss</button>
              </div>
            </li>
          {/each}
        </ul>
      {/if}
    </div>

    <!-- Evaluation criteria -->
    <div class="card p-5 mb-6">
      <h2 class="font-semibold mb-1">Evaluation criteria</h2>
      <p class="text-xs text-sand-500 mb-3">
        Pass/fail rules graded on every delivered output. A failed <strong>critical</strong> rule
        flags the task and notifies you. Leave empty to skip evaluation.
      </p>

      {#if criteria.length}
        <ul class="divide-y divide-sand-100 mb-3 border border-sand-100 rounded-lg">
          {#each criteria as c (c.id)}
            <li class="px-3 py-2.5 flex items-start gap-3">
              <span class="badge {c.priority === 'critical' ? 'badge-red' : 'badge-yellow'} text-xs mt-0.5 shrink-0">{c.priority}</span>
              <div class="flex-1 min-w-0">
                <div class="text-sm font-medium">{c.name}</div>
                <div class="text-xs text-sand-500">{c.prompt}</div>
              </div>
              <button class="text-red-400 hover:text-red-600 text-sm shrink-0" title="Delete" on:click={() => deleteCriterion(c.id)}>×</button>
            </li>
          {/each}
        </ul>
      {/if}

      <div class="grid sm:grid-cols-[1fr_2fr_auto] gap-2 items-start">
        <input class="input text-sm" placeholder="Name (e.g. Accuracy)" bind:value={newCriterion.name} />
        <input class="input text-sm" placeholder="Rule (e.g. The output addresses every part of the task)" bind:value={newCriterion.prompt} />
        <div class="flex gap-2">
          <select class="input text-sm" bind:value={newCriterion.priority}>
            <option value="warning">Warning</option>
            <option value="critical">Critical</option>
          </select>
          <button class="btn-secondary text-sm shrink-0" disabled={addingCriterion} on:click={addCriterion}>Add</button>
        </div>
      </div>
    </div>

    <!-- Tool-approval policy -->
    <div class="card p-5 mb-6">
      <h2 class="font-semibold mb-1">Approval before actions</h2>
      <p class="text-xs text-sand-500 mb-3">
        Decide whether your agent pauses for your OK before it takes an external action on a job —
        opening a PR, posting a comment, sending an email, creating a doc.
      </p>
      <div class="space-y-2">
        {#each POLICY_MODES as m}
          <label class="flex items-start gap-2.5 border rounded-lg px-3 py-2.5 cursor-pointer transition-colors {policyMode === m.key ? 'border-brand-400 bg-brand-50' : 'border-sand-200 hover:border-sand-300'}">
            <input type="radio" class="mt-0.5 accent-brand-600" bind:group={policyMode} value={m.key} />
            <span>
              <span class="block text-sm font-medium text-sand-800">{m.label}</span>
              <span class="block text-xs text-sand-500">{m.hint}</span>
            </span>
          </label>
        {/each}
      </div>
      <div class="flex justify-end mt-3">
        <button class="btn-secondary text-sm" disabled={savingPolicy} on:click={savePolicy}>
          {savingPolicy ? 'Saving…' : policySaved ? '✓ Saved' : 'Save policy'}
        </button>
      </div>
    </div>

    <!-- Execution backend / runtime -->
    <div class="card p-5 mb-6">
      <h2 class="font-semibold mb-1">Where your agent does work</h2>
      <p class="text-xs text-sand-400 mb-4">For coding jobs, your agent runs on a real machine. Pick where it runs and which coding tool drives it. (A workflow can override this per task.)</p>

      <div class="space-y-3">
        <div>
          <label class="label text-xs" for="backend">Execution backend</label>
          <select class="input text-sm" id="backend" bind:value={backend}>
            <option value="managed">Claude Code Managed Cloud</option>
            <option value="sprite">flaborful sandboxes (powered by sprites.io)</option>
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
            <p class="text-xs text-sand-400 mt-1">We install your chosen tool on the sprite and point it at your style model.</p>
          </div>

          {#if runtime === 'custom'}
            <div>
              <label class="label text-xs" for="runtime-spec">Custom runtime spec (JSON)</label>
              <textarea class="input text-sm font-mono h-40" id="runtime-spec"
                bind:value={runtimeSpecText} placeholder={CUSTOM_SPEC_PLACEHOLDER}></textarea>
              <p class="text-xs text-sand-400 mt-1">
                Templates: <span class="font-mono">{'{taste_url}'}</span> in the MCP config;
                <span class="font-mono">{'{repo_dir}'}</span>, <span class="font-mono">{'{task}'}</span>,
                <span class="font-mono">{'{persona}'}</span> in the invoke command.
              </p>
            </div>
          {/if}
        {:else}
          <p class="text-xs text-sand-400">Claude Code Managed Cloud runs Claude Code on Anthropic's hosted container; your repo token stays off the machine. Runs bill to <strong>your own</strong> Anthropic API key.</p>
        {/if}

        <!-- Bring-your-own Anthropic key. Required for Managed Cloud; optional for sandboxes. -->
        <div>
          <label class="label text-xs" for="anthropic-key">Anthropic API key</label>
          {#if anthropicKeySet && !replacingKey}
            <div class="flex items-center gap-2 text-sm">
              <span class="text-sand-400 font-mono">•••• on file</span>
              <button type="button" class="text-xs underline text-sand-400" on:click={() => { replacingKey = true; anthropicKey = ''; }}>Replace</button>
              <button type="button" class="text-xs underline text-red-500" on:click={removeAnthropicKey} disabled={savingExec}>Remove</button>
            </div>
          {:else}
            <input class="input text-sm font-mono" id="anthropic-key" type="password" autocomplete="off"
              placeholder="sk-ant-…" bind:value={anthropicKey} />
            {#if anthropicKeySet}
              <button type="button" class="text-xs underline text-sand-400 mt-1" on:click={() => { replacingKey = false; anthropicKey = ''; }}>Cancel</button>
            {/if}
          {/if}
          <p class="text-xs text-sand-400 mt-1">
            {#if backend === 'managed'}
              <strong>Required</strong> for Claude Code Managed Cloud — runs use this key.
            {:else}
              Optional. Leave blank to run on flaborful's shared capacity.
            {/if}
            Stored encrypted; never shown again.
          </p>
        </div>
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
      <p class="text-xs text-sand-400 mb-4">Choose which parts of your knowledge base to train on. Your full KB is preserved either way.</p>

      <div class="space-y-2 mb-4">
        {#each typeCounts as t}
          <label class="flex items-center gap-2.5 text-sm cursor-pointer">
            <input type="checkbox" checked={selectedTypes.has(t.key)} on:change={() => toggleType(t.key)} />
            <span class="flex-1">{t.label}</span>
            <span class="text-xs text-sand-400">{t.count} item{t.count === 1 ? '' : 's'}</span>
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
        <div class="mt-4 border-t border-sand-100 pt-4">
          <div class="text-xs font-semibold text-sand-500 mb-2">{preview.length} training example{preview.length === 1 ? '' : 's'} selected</div>
          <div class="space-y-1.5 max-h-72 overflow-y-auto">
            {#each preview as p}
              <div class="bg-sand-50 rounded p-2 text-xs">
                <span class="text-[10px] uppercase tracking-wide text-sand-400">{p.source_type.replace(/_/g, ' ')}</span>
                <div class="text-sand-700 line-clamp-2">{p.content_preview}</div>
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
