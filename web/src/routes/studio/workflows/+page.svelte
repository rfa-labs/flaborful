<script lang="ts">
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { api } from '$lib/api/client';
  import { session } from '$lib/stores/session';
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  type Workflow = any;

  $: agentId = $session.agentId;

  let workflows: Workflow[] = [];
  let loading = true;

  onMount(async () => {
    let aid = agentId;
    if (!aid) {
      try {
        const agent = await api.get<{ id: string }>('/agents/mine');
        session.setIds({ agentId: agent.id });
        aid = agent.id;
      } catch {
        loading = false;
        return;
      }
    }
    try {
      workflows = await api.get<Workflow[]>(`/workflows/agent/${aid}`);
    } catch {
      workflows = [];
    } finally {
      loading = false;
    }
  });

  async function createWorkflow() {
    const aid = $session.agentId;
    if (!aid) return;
    const wf = await api.post<Workflow>('/workflows', {
      agent_id: aid,
      name: 'New Workflow',
      description: '',
      graph: { nodes: [], edges: [] },
      is_universal: true,
    });
    goto(`/studio/workflows/${wf.id}`);
  }

  const TEMPLATES = [
    {
      name: 'Code Review',
      description: 'Analyze a PR diff and post a structured review comment',
      icon: '🔍',
      graph: {
        nodes: [
          { id: 'trigger',   node_type: 'trigger',         label: 'PR Opened',       config: {}, position: { x: 100, y: 200 } },
          { id: 'fetch',     node_type: 'github',           label: 'Fetch Diff',      config: { action: 'fetch_diff' }, position: { x: 300, y: 200 } },
          { id: 'analyze',   node_type: 'diff_analyzer',    label: 'Analyze Diff',    config: { diff_var: 'diff', output_var: 'diff_analysis' }, position: { x: 500, y: 200 } },
          { id: 'review',    node_type: 'llm_reasoning',    label: 'Write Review',    config: { prompt: 'Write a professional code review for this diff analysis: {{input}}', input_var: 'diff_analysis', output_var: 'review_draft' }, position: { x: 700, y: 200 } },
          { id: 'gate',      node_type: 'human_gate',       label: 'Review',          config: { input_var: 'review_draft' }, position: { x: 900, y: 200 } },
          { id: 'format',    node_type: 'formatter',        label: 'Format Comment',  config: { input_var: 'review_draft', output_var: 'formatted_output', format: 'markdown' }, position: { x: 1100, y: 200 } },
          { id: 'post',      node_type: 'github',           label: 'Post Comment',    config: { action: 'post_comment', comment_var: 'formatted_output' }, position: { x: 1300, y: 200 } },
          { id: 'output',    node_type: 'output',           label: 'Done',            config: { output_var: 'formatted_output' }, position: { x: 1500, y: 200 } },
        ],
        edges: [
          { id: 'e1', source: 'trigger', target: 'fetch' },
          { id: 'e2', source: 'fetch',   target: 'analyze' },
          { id: 'e3', source: 'analyze', target: 'review' },
          { id: 'e4', source: 'review',  target: 'gate' },
          { id: 'e5', source: 'gate',    target: 'format' },
          { id: 'e6', source: 'format',  target: 'post' },
          { id: 'e7', source: 'post',    target: 'output' },
        ],
      },
    },
    {
      name: 'Debugging',
      description: 'Parse a stack trace and propose a root-cause fix',
      icon: '🐛',
      graph: {
        nodes: [
          { id: 'trigger', node_type: 'trigger',           label: 'Bug Report',     config: {}, position: { x: 100, y: 200 } },
          { id: 'parse',   node_type: 'stack_trace_parser', label: 'Parse Trace',   config: { input_var: 'input', output_var: 'parsed_trace' }, position: { x: 300, y: 200 } },
          { id: 'reason',  node_type: 'llm_reasoning',     label: 'Diagnose',       config: { prompt: 'Diagnose this error and propose a fix: {{input}}', input_var: 'parsed_trace', output_var: 'diagnosis' }, position: { x: 550, y: 200 } },
          { id: 'gate',    node_type: 'human_gate',        label: 'Review',         config: { input_var: 'diagnosis' }, position: { x: 800, y: 200 } },
          { id: 'output',  node_type: 'output',            label: 'Done',           config: { output_var: 'diagnosis' }, position: { x: 1000, y: 200 } },
        ],
        edges: [
          { id: 'e1', source: 'trigger', target: 'parse' },
          { id: 'e2', source: 'parse',   target: 'reason' },
          { id: 'e3', source: 'reason',  target: 'gate' },
          { id: 'e4', source: 'gate',    target: 'output' },
        ],
      },
    },
    {
      name: 'Documentation',
      description: 'Generate or update technical documentation for a module',
      icon: '📝',
      graph: {
        nodes: [
          { id: 'trigger', node_type: 'trigger',      label: 'Module Input',   config: {}, position: { x: 100, y: 200 } },
          { id: 'reason',  node_type: 'llm_reasoning', label: 'Write Docs',   config: { prompt: 'Write clear technical documentation for this: {{input}}', input_var: 'trigger_output', output_var: 'docs_draft' }, position: { x: 350, y: 200 } },
          { id: 'gate',    node_type: 'human_gate',   label: 'Review',         config: { input_var: 'docs_draft' }, position: { x: 600, y: 200 } },
          { id: 'format',  node_type: 'formatter',    label: 'Format Markdown', config: { input_var: 'docs_draft', output_var: 'formatted_output', format: 'markdown' }, position: { x: 850, y: 200 } },
          { id: 'output',  node_type: 'output',       label: 'Done',           config: { output_var: 'formatted_output' }, position: { x: 1050, y: 200 } },
        ],
        edges: [
          { id: 'e1', source: 'trigger', target: 'reason' },
          { id: 'e2', source: 'reason',  target: 'gate' },
          { id: 'e3', source: 'gate',    target: 'format' },
          { id: 'e4', source: 'format',  target: 'output' },
        ],
      },
    },
  ];

  async function deleteWorkflow(id: string, e: MouseEvent) {
    e.stopPropagation();
    if (!confirm('Delete this workflow?')) return;
    await api.delete(`/workflows/${id}`);
    workflows = workflows.filter(wf => wf.id !== id);
  }

  async function useTemplate(t: typeof TEMPLATES[0]) {
    const aid = $session.agentId;
    if (!aid) return;
    const wf = await api.post<{ id: string }>('/workflows', {
      agent_id: aid,
      name: t.name,
      description: t.description,
      graph: t.graph,
      is_universal: true,
    });
    goto(`/studio/workflows/${wf.id}`);
  }
</script>

<svelte:head><title>Workflow Studio — Flaborful</title></svelte:head>

<div class="max-w-7xl mx-auto px-4 py-8">
  <div class="flex items-center justify-between mb-8">
    <div>
      <h1 class="text-2xl font-bold">Workflow Studio</h1>
      <p class="text-sand-500 text-sm mt-0.5">Define how your agent does its work</p>
    </div>
    <button class="btn-primary" on:click={createWorkflow}>+ New workflow</button>
  </div>

  <!-- Templates -->
  <div class="mb-10">
    <h2 class="text-sm font-semibold text-sand-500 uppercase tracking-wide mb-4">Start from a template</h2>
    <div class="grid sm:grid-cols-3 gap-4">
      {#each TEMPLATES as t}
        <button
          class="card p-5 text-left hover:shadow-md hover:border-brand-300 transition-all"
          on:click={() => useTemplate(t)}>
          <div class="text-3xl mb-3">{t.icon}</div>
          <div class="font-semibold mb-1">{t.name}</div>
          <div class="text-sm text-sand-500">{t.description}</div>
        </button>
      {/each}
    </div>
  </div>

  <!-- Existing workflows -->
  <div>
    <h2 class="text-sm font-semibold text-sand-500 uppercase tracking-wide mb-4">Your workflows</h2>

    {#if loading}
      <div class="space-y-3">
        {#each Array(3) as _}
          <div class="card p-4 animate-pulse"><div class="h-8 bg-sand-100 rounded"></div></div>
        {/each}
      </div>
    {:else if workflows.length === 0}
      <div class="card p-8 text-center text-sand-400 text-sm">
        No workflows yet. Use a template above or create a blank one.
      </div>
    {:else}
      <div class="space-y-2">
        {#each workflows as wf}
          <button
            class="card w-full px-5 py-4 flex items-center gap-4 text-left hover:bg-sand-50 transition-colors"
            on:click={() => goto(`/studio/workflows/${wf.id}`)}>
            <div class="flex-1 min-w-0">
              <div class="font-medium">{wf.name}</div>
              {#if wf.description}
                <div class="text-sm text-sand-400 truncate">{wf.description}</div>
              {/if}
            </div>
            <div class="text-xs text-sand-400">
              {wf.graph?.nodes?.length ?? 0} nodes
            </div>
            <span class="badge-{wf.is_universal ? 'blue' : 'purple'} shrink-0">
              {wf.is_universal ? 'universal' : 'job-specific'}
            </span>
            <span class="text-sand-300">›</span>
            <button
              class="ml-1 text-sand-300 hover:text-red-500 transition-colors px-1"
              title="Delete workflow"
              on:click={(e) => deleteWorkflow(wf.id, e)}>✕</button>
          </button>
        {/each}
      </div>
    {/if}
  </div>
</div>
