<script lang="ts">
  import { onMount } from 'svelte';
  import { browser } from '$app/environment';
  import { page } from '$app/stores';
  import { goto } from '$app/navigation';
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  import { Svelvet, Node as NodeC, Anchor as AnchorC, Background } from 'svelvet';
  const Node = NodeC as any;
  const Anchor = AnchorC as any;
  import { api } from '$lib/api/client';

  $: workflowId = $page.params.id;
  // When arriving from a pending task ("build a new workflow for this task"), this holds the task
  // id so we can offer "Assign to task & run" once the workflow is built.
  $: assignTaskId = $page.url.searchParams.get('assignTask');

  interface WFNode {
    id: string;
    node_type: string;
    label: string;
    config: Record<string, unknown>;
    position: { x: number; y: number };
  }

  interface WFEdge {
    id: string;
    source: string;
    target: string;
  }

  let workflow: { id: string; name: string; description: string; graph: { nodes: WFNode[]; edges: WFEdge[] }; is_universal: boolean } | null = null;
  let nodes: WFNode[] = [];
  // edges is the single source of truth — updated only via on:connection / on:disconnection
  let edges: WFEdge[] = [];
  let loading = true;
  let saving = false;
  let saved = false;
  let error = '';

  let selectedNode: WFNode | null = null;
  let showAddPanel = false;

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const nodeDisconnectors: Record<string, (targets: any) => void> = {};
  const livePositions: Record<string, { x: number; y: number }> = {};

  onMount(async () => {
    try {
      workflow = await api.get(`/workflows/${workflowId}`);
      if (workflow) {
        nodes = workflow.graph.nodes ?? [];
        edges = workflow.graph.edges ?? [];
      }
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to load workflow';
    } finally {
      loading = false;
    }
  });

  function onConnection(sourceNodeId: string, targetNodeId: string) {
    const edgeId = `${sourceNodeId}-${targetNodeId}`;
    if (!edges.find(e => e.id === edgeId)) {
      edges = [...edges, { id: edgeId, source: sourceNodeId, target: targetNodeId }];
    }
  }

  function onDisconnection(sourceNodeId: string, targetNodeId: string) {
    edges = edges.filter(e => !(e.source === sourceNodeId && e.target === targetNodeId));
  }

  // Svelvet emits untyped CustomEvents; these factories keep the markup tidy and typed.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const onNodeReleased = (nodeId: string) => (e: any) => {
    const pos = e.detail?.node?.position;
    if (pos && typeof pos.subscribe === 'function') {
      pos.subscribe((v: { x: number; y: number }) => { livePositions[nodeId] = v; })();
    }
  };
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const onAnchorConnect = (sourceId: string) => (e: any) => {
    const raw = e.detail?.connectedNode?.id ?? '';
    const targetId = raw.startsWith('N-') ? raw.slice(2) : raw;
    if (targetId) onConnection(sourceId, targetId);
  };
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const onAnchorDisconnect = (sourceId: string) => (e: any) => {
    const raw = e.detail?.connectedNode?.id ?? '';
    const targetId = raw.startsWith('N-') ? raw.slice(2) : raw;
    if (targetId) onDisconnection(sourceId, targetId);
  };

  function disconnectEdge(edge: WFEdge) {
    // Call Svelvet's internal disconnect on the source node's output anchor
    const fn = nodeDisconnectors[edge.source];
    if (fn) {
      fn([edge.target, `${edge.target}-in`]);
    }
    // Also update our edges array immediately (disconnection event may or may not fire)
    edges = edges.filter(e => e.id !== edge.id);
  }

  const NODE_TYPES = [
    { type: 'trigger',            label: 'Trigger',         color: '#7c4dff', group: 'Control' },
    { type: 'llm_reasoning',      label: 'LLM Reasoning',   color: '#6a34ff', group: 'AI' },
    { type: 'human_gate',         label: 'Human Gate',      color: '#f59e0b', group: 'Control' },
    { type: 'conditional_branch', label: 'Branch',          color: '#7c4dff', group: 'Control' },
    { type: 'output',             label: 'Output',          color: '#10b981', group: 'Control' },
    { type: 'formatter',          label: 'Formatter',       color: '#7c4dff', group: 'Utility' },
    { type: 'github',             label: 'GitHub',          color: '#1f2937', group: 'Tools' },
    { type: 'diff_analyzer',      label: 'Diff Analyzer',   color: '#1f2937', group: 'Tools' },
    { type: 'stack_trace_parser', label: 'Stack Trace',     color: '#1f2937', group: 'Tools' },
    { type: 'http_request',       label: 'HTTP Request',    color: '#0ea5e9', group: 'Tools' },
    { type: 'slack',              label: 'Slack',           color: '#4a154b', group: 'Tools' },
    { type: 'email',              label: 'Email',           color: '#ea4335', group: 'Tools' },
    { type: 'web_search',         label: 'Web Search',      color: '#34a853', group: 'Tools' },
    { type: 'google_docs',        label: 'Google Doc',      color: '#1a73e8', group: 'Tools' },
    { type: 'agent_implement',    label: 'Agent: Implement', color: '#0f766e', group: 'Software Engineering' },
    { type: 'sprite_command',     label: 'Run Command',     color: '#0e7490', group: 'Software Engineering' },
    { type: 'sprite_test',        label: 'Run Tests',       color: '#0891b2', group: 'Software Engineering' },
    { type: 'git_commit',         label: 'Git Commit',      color: '#374151', group: 'Software Engineering' },
    { type: 'git_push',           label: 'Git Push',        color: '#111827', group: 'Software Engineering' },
    { type: 'test_gate',          label: 'Test Gate',       color: '#b45309', group: 'Software Engineering' },
    { type: 'pr_create',          label: 'Create PR',       color: '#1f2937', group: 'Software Engineering' },
  ];

  // Some canvas node types are presentation-only aliases over a backend NodeType with
  // preset config. google_docs → document_read_write with provider=google_docs.
  const NODE_TYPE_PRESETS: Record<string, { node_type: string; config: Record<string, unknown> }> = {
    google_docs: {
      node_type: 'document_read_write',
      config: { provider: 'google_docs', action: 'create', model_var: 'llm_output' },
    },
  };

  // The canvas "kind" of a node: its alias if it has one (via config.canvas_kind),
  // otherwise its raw node_type. Used for color/label/config lookups so preset nodes
  // keep their identity after save/reload.
  function canvasKind(node: WFNode): string {
    return (node.config?.canvas_kind as string) ?? node.node_type;
  }

  function nodeColor(type: string) {
    return NODE_TYPES.find(n => n.type === type)?.color ?? '#6b7280';
  }

  function nodeLabel(type: string) {
    return NODE_TYPES.find(n => n.type === type)?.label ?? type;
  }

  function addNode(type: string) {
    const label = nodeLabel(type);
    const preset = NODE_TYPE_PRESETS[type];
    const newNode: WFNode = {
      id: `${type}_${Date.now()}`,
      node_type: preset?.node_type ?? type,
      label,
      // Seed preset config and tag the canvas kind so the alias survives reload.
      config: preset ? { ...preset.config, canvas_kind: type } : {},
      position: { x: 200 + nodes.length * 60, y: 200 + (nodes.length % 3) * 80 },
    };
    nodes = [...nodes, newNode];
    showAddPanel = false;
    selectedNode = newNode;
  }

  function removeNode(id: string) {
    nodes = nodes.filter(n => n.id !== id);
    edges = edges.filter(e => e.source !== id && e.target !== id);
    if (selectedNode?.id === id) selectedNode = null;
  }

  // Return `nodes` with each node's live (dragged) position merged in. Reassigning `nodes`
  // re-renders the canvas from each node's `position` field — which is the stale add-time
  // position unless we fold in `livePositions`. Without this, editing any node config snaps the
  // whole graph back to its ugly spawn layout.
  function syncedNodes(): WFNode[] {
    return nodes.map(n => livePositions[n.id] ? { ...n, position: livePositions[n.id] } : n);
  }

  function updateConfig(key: string, value: string) {
    if (!selectedNode) return;
    selectedNode.config[key] = value;
    nodes = syncedNodes();
  }

  // A sensible starting prompt for an LLM node that authors a document. The engine adds
  // the strict-JSON DocumentModel schema to the system prompt automatically when
  // document_author is on, so this only needs to describe the content to write.
  const DOC_AUTHOR_PROMPT_DEFAULT =
    'Write a professional document for the following request, in your own voice and to your usual standard:\n\n{{input}}';

  function updateConfigBool(key: string, value: boolean) {
    if (!selectedNode) return;
    selectedNode.config[key] = value;
    // Turning on document-author mode: seed a starter prompt if none is set yet.
    if (key === 'document_author' && value && !String(selectedNode.config.prompt ?? '').trim()) {
      selectedNode.config.prompt = DOC_AUTHOR_PROMPT_DEFAULT;
    }
    nodes = syncedNodes();
  }

  async function save() {
    if (!workflow) return;
    saving = true;
    try {
      const updatedNodes = syncedNodes();
      await api.put(`/workflows/${workflowId}`, {
        ...workflow,
        graph: { nodes: updatedNodes, edges },
      });
      saved = true;
      setTimeout(() => saved = false, 2000);
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Save failed';
    } finally {
      saving = false;
    }
  }

  // Save the workflow, then set it as the pending task's workflow (which dispatches the run) and
  // jump to that task's execution view.
  async function assignToTask() {
    if (!assignTaskId) return;
    await save();
    if (error) return;
    saving = true;
    try {
      await api.patch(`/tasks/${assignTaskId}/workflow`, { workflow_id: workflowId });
      goto(`/studio/executions/${assignTaskId}`);
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to assign to task';
    } finally {
      saving = false;
    }
  }

  const CONFIG_FIELDS: Record<string, Array<{ key: string; label: string; type: string; placeholder?: string }>> = {
    llm_reasoning:      [{ key: 'prompt', label: 'Prompt template', type: 'textarea', placeholder: 'Analyze this: {{input}}' }, { key: 'input_var', label: 'Input variable', type: 'text', placeholder: 'trigger_output' }, { key: 'output_var', label: 'Output variable', type: 'text', placeholder: 'llm_output' }, { key: 'document_author', label: 'Author a document (emit structured doc JSON for a Google Doc node)', type: 'checkbox' }],
    human_gate:         [{ key: 'input_var', label: 'Input variable to show', type: 'text', placeholder: 'llm_output' }],
    github:             [{ key: 'action', label: 'Action', type: 'select' }, { key: 'output_var', label: 'Output variable', type: 'text', placeholder: 'diff' }],
    formatter:          [{ key: 'input_var', label: 'Input variable', type: 'text', placeholder: 'llm_output' }, { key: 'format', label: 'Format', type: 'select' }, { key: 'output_var', label: 'Output variable', type: 'text', placeholder: 'formatted_output' }],
    http_request:       [{ key: 'url', label: 'URL', type: 'text', placeholder: 'https://api.example.com/...' }, { key: 'method', label: 'Method', type: 'select' }, { key: 'output_var', label: 'Output variable', type: 'text', placeholder: 'http_response' }],
    conditional_branch: [{ key: 'condition_var', label: 'Variable to evaluate', type: 'text', placeholder: 'llm_output' }, { key: 'conditions', label: 'Conditions (JSON)', type: 'textarea', placeholder: '{"if_contains_bug": "yes", "otherwise": "no"}' }],
    output:             [{ key: 'output_var', label: 'Variable to deliver', type: 'text', placeholder: 'llm_output' }],
    diff_analyzer:      [{ key: 'diff_var', label: 'Diff variable', type: 'text', placeholder: 'diff' }, { key: 'output_var', label: 'Output variable', type: 'text', placeholder: 'diff_analysis' }],
    stack_trace_parser: [{ key: 'input_var', label: 'Input variable', type: 'text', placeholder: 'input' }, { key: 'output_var', label: 'Output variable', type: 'text', placeholder: 'parsed_trace' }],
    google_docs:        [
      { key: 'model_var', label: 'Document model variable', type: 'text', placeholder: 'llm_output' },
      { key: 'file_name', label: 'File name', type: 'text', placeholder: 'defaults to the doc title' },
      { key: 'shared_drive_id', label: 'Shared Drive ID', type: 'text', placeholder: 'inherits from job grant if blank' },
      { key: 'parent_folder_id', label: 'Folder ID (optional)', type: 'text', placeholder: 'drive root if blank' },
      { key: 'output_var', label: 'Output variable', type: 'text', placeholder: 'created_doc' },
    ],
    agent_implement:    [
      { key: 'task', label: 'Task for the agent', type: 'textarea', placeholder: 'Describe the change to implement (or leave blank to use task_var)' },
      { key: 'task_var', label: 'Task variable', type: 'text', placeholder: 'input' },
      { key: 'instructions', label: 'Agent instructions (how to run the loop)', type: 'textarea', placeholder: 'e.g. Consult the principal on design decisions. Write unit tests and run them. Use concise, conventional commit messages.' },
      { key: 'repo', label: 'Repo (owner/name)', type: 'text', placeholder: 'inherits from ctx.vars[repo]' },
      { key: 'base_branch', label: 'Base branch', type: 'text', placeholder: 'main' },
      { key: 'branch', label: 'Work branch', type: 'text', placeholder: 'auto: flaborful/task-…' },
      { key: 'backend', label: 'Backend override', type: 'select' },
      { key: 'output_var', label: 'Output variable', type: 'text', placeholder: 'implementation' },
    ],
    sprite_command:     [
      { key: 'command', label: 'Command', type: 'textarea', placeholder: 'e.g. npm run build (or leave blank to use command_var)' },
      { key: 'command_var', label: 'Command variable', type: 'text', placeholder: 'command' },
      { key: 'timeout_s', label: 'Timeout (seconds)', type: 'text', placeholder: '300' },
      { key: 'output_var', label: 'Output variable', type: 'text', placeholder: 'command_result' },
    ],
    sprite_test:        [
      { key: 'test_command', label: 'Test command', type: 'text', placeholder: 'npm test' },
      { key: 'timeout_s', label: 'Timeout (seconds)', type: 'text', placeholder: '600' },
      { key: 'output_var', label: 'Output variable', type: 'text', placeholder: 'test_results' },
    ],
    git_commit:         [
      { key: 'message', label: 'Commit message', type: 'textarea', placeholder: 'Changes by your Flaborful agent (or use message_var)' },
      { key: 'message_var', label: 'Message variable', type: 'text', placeholder: 'commit_message' },
      { key: 'add_paths', label: 'Paths to stage', type: 'text', placeholder: '-A' },
      { key: 'output_var', label: 'Output variable', type: 'text', placeholder: 'commit_result' },
    ],
    git_push:           [
      { key: 'branch', label: 'Branch', type: 'text', placeholder: 'inherits from implementation.branch' },
      { key: 'force', label: 'Force push', type: 'checkbox' },
      { key: 'output_var', label: 'Output variable', type: 'text', placeholder: 'push_result' },
    ],
    test_gate:          [
      { key: 'test_command', label: 'Test command', type: 'text', placeholder: 'npm test' },
      { key: 'lint_command', label: 'Lint command (optional)', type: 'text', placeholder: 'npm run lint' },
      { key: 'max_retries', label: 'Max fix attempts', type: 'text', placeholder: '2' },
      { key: 'timeout_s', label: 'Timeout (seconds)', type: 'text', placeholder: '600' },
      { key: 'output_var', label: 'Output variable', type: 'text', placeholder: 'gate_result' },
    ],
    pr_create:          [
      { key: 'repo', label: 'Repo (owner/name)', type: 'text', placeholder: 'inherits from ctx.vars[repo]' },
      { key: 'source_var', label: 'Branch source variable', type: 'text', placeholder: 'push_result' },
      { key: 'base', label: 'Base branch', type: 'text', placeholder: 'main' },
      { key: 'title', label: 'PR title', type: 'text', placeholder: 'Changes by your Flaborful agent' },
      { key: 'body_var', label: 'PR body variable', type: 'text', placeholder: 'llm_output' },
      { key: 'draft', label: 'Open as draft', type: 'checkbox' },
      { key: 'output_var', label: 'Output variable', type: 'text', placeholder: 'pr_result' },
    ],
  };

  const SELECT_OPTIONS: Record<string, string[]> = {
    action: ['fetch_diff', 'post_comment'],
    format: ['markdown', 'json', 'email', 'plain'],
    method: ['GET', 'POST', 'PUT', 'PATCH', 'DELETE'],
    backend: ['', 'managed', 'sprite'],
  };

  // Initial connections for a node's output anchor (only used at mount time by Svelvet)
  function initialConnections(nodeId: string): [string, string][] {
    return edges
      .filter(e => e.source === nodeId)
      .map(e => [e.target, `${e.target}-in`]);
  }

  $: selectedNodeEdges = selectedNode
    ? edges.filter(e => e.source === selectedNode!.id || e.target === selectedNode!.id)
    : [];
</script>

<svelte:head><title>{workflow?.name ?? 'Workflow'} — Flaborful</title></svelte:head>

<div class="h-[calc(100vh-56px)] flex flex-col">
  <!-- Toolbar -->
  <div class="bg-white border-b border-sand-200 px-4 py-2 flex items-center gap-3 z-10">
    <button class="btn-ghost text-sm" on:click={() => goto('/studio/workflows')}>← Workflows</button>
    <div class="h-4 w-px bg-sand-200"></div>

    {#if workflow}
      <input
        class="text-sm font-semibold bg-transparent border-0 focus:outline-none focus:ring-1 focus:ring-brand-400 rounded px-2 py-1 -ml-2"
        bind:value={workflow.name}
      />
    {/if}

    <div class="flex-1"></div>

    <button class="btn-ghost text-sm" on:click={() => showAddPanel = !showAddPanel}>
      + Add node
    </button>
    {#if assignTaskId}
      <button class="btn-ghost text-sm" disabled={saving} on:click={save}>
        {#if saving}Saving...{:else if saved}✓ Saved{:else}Save{/if}
      </button>
      <button class="btn-primary text-sm" disabled={saving} on:click={assignToTask}>
        Assign to task &amp; run
      </button>
    {:else}
      <button class="btn-primary text-sm" disabled={saving} on:click={save}>
        {#if saving}Saving...{:else if saved}✓ Saved{:else}Save{/if}
      </button>
    {/if}
  </div>

  {#if assignTaskId}
    <div class="bg-amber-50 border-b border-amber-200 px-4 py-1.5 text-xs text-amber-800">
      Building a workflow for a pending task — when you're done, click
      <strong>Assign to task &amp; run</strong> to start it.
    </div>
  {/if}

  <div class="flex-1 flex overflow-hidden relative">
    <!-- Node type panel -->
    {#if showAddPanel}
      <div class="absolute left-4 top-4 z-20 bg-white rounded-xl border border-sand-200 shadow-lg w-56 py-2">
        <div class="px-3 py-1 text-xs font-semibold text-sand-400 uppercase tracking-wide">Add node</div>
        {#each ['Control', 'AI', 'Software Engineering', 'Tools', 'Utility'] as group}
          {@const items = NODE_TYPES.filter(n => n.group === group)}
          {#if items.length}
            <div class="px-3 py-1 mt-1 text-xs font-medium text-sand-500">{group}</div>
            {#each items as nt}
              <button
                class="w-full text-left px-3 py-2 text-sm hover:bg-sand-50 flex items-center gap-2"
                on:click={() => addNode(nt.type)}>
                <span class="w-2 h-2 rounded-full shrink-0" style="background:{nt.color}"></span>
                {nt.label}
              </button>
            {/each}
          {/if}
        {/each}
      </div>
    {/if}

    <!-- Config panel -->
    {#if selectedNode}
      <div class="absolute right-4 top-4 z-20 bg-white rounded-xl border border-sand-200 shadow-lg w-72 max-h-[80vh] overflow-y-auto">
        <div class="px-4 py-3 border-b border-sand-100 flex items-center justify-between">
          <span class="font-semibold text-sm">{selectedNode.label}</span>
          <button class="text-sand-400 hover:text-sand-600 text-lg leading-none" on:click={() => selectedNode = null}>×</button>
        </div>
        <div class="p-4 space-y-3">
          <div>
            <label class="label text-xs" for="node-label">Node label</label>
            <input class="input text-sm" id="node-label"
              bind:value={selectedNode.label}
              on:input={() => { nodes = [...nodes]; }} />
          </div>

          {#if canvasKind(selectedNode) === 'google_docs'}
            <p class="text-xs text-sand-500 bg-blue-50 border border-blue-100 rounded-lg p-2.5 leading-relaxed">
              Creates a formatted Google Doc from a structured document model. Feed this node
              from an LLM Reasoning node with <strong>“Author a document”</strong> enabled — that
              makes it emit the document JSON this node renders.
            </p>
          {/if}

          {#each CONFIG_FIELDS[canvasKind(selectedNode)] ?? [] as field}
            {#if field.type === 'checkbox'}
              <label class="flex items-start gap-2 text-xs text-sand-600 cursor-pointer">
                <input type="checkbox" class="mt-0.5"
                  checked={Boolean(selectedNode.config[field.key])}
                  on:change={(e) => updateConfigBool(field.key, e.currentTarget.checked)} />
                <span>{field.label}</span>
              </label>
            {:else}
            <div>
              <label class="label text-xs" for="cfg-{field.key}">{field.label}</label>
              {#if field.type === 'textarea'}
                <textarea class="input text-sm min-h-[70px] resize-none font-mono text-xs" id="cfg-{field.key}"
                  placeholder={field.placeholder}
                  on:input={(e) => updateConfig(field.key, e.currentTarget.value)}
                >{String(selectedNode.config[field.key] ?? '')}</textarea>
              {:else if field.type === 'select'}
                <select class="input text-sm" id="cfg-{field.key}"
                  value={String(selectedNode.config[field.key] ?? '')}
                  on:change={(e) => updateConfig(field.key, e.currentTarget.value)}>
                  <option value="">— select —</option>
                  {#each SELECT_OPTIONS[field.key] ?? [] as opt}
                    <option value={opt}>{opt}</option>
                  {/each}
                </select>
              {:else}
                <input class="input text-sm" id="cfg-{field.key}"
                  placeholder={field.placeholder}
                  value={String(selectedNode.config[field.key] ?? '')}
                  on:input={(e) => updateConfig(field.key, e.currentTarget.value)}
                />
              {/if}
            </div>
            {/if}
          {/each}

          <!-- Connections list -->
          {#if selectedNodeEdges.length > 0}
            <div>
              <div class="text-xs font-medium text-sand-500 mb-1">Connections</div>
              <div class="space-y-1">
                {#each selectedNodeEdges as edge (edge.id)}
                  {@const isOut = edge.source === selectedNode.id}
                  {@const otherId = isOut ? edge.target : edge.source}
                  {@const otherNode = nodes.find(n => n.id === otherId)}
                  <div class="flex items-center gap-2 text-xs bg-sand-50 rounded px-2 py-1.5">
                    <span class="text-sand-400 font-mono w-4 shrink-0">{isOut ? '→' : '←'}</span>
                    <span class="flex-1 truncate text-sand-700">{otherNode?.label ?? otherId}</span>
                    <button
                      class="text-red-400 hover:text-red-600 text-sm font-bold leading-none px-1 shrink-0"
                      title="Remove connection"
                      on:click={() => disconnectEdge(edge)}
                    >×</button>
                  </div>
                {/each}
              </div>
            </div>
          {/if}

          <button class="btn-danger w-full text-sm mt-2" on:click={() => { if (selectedNode) removeNode(selectedNode.id); }}>
            Remove node
          </button>
        </div>
      </div>
    {/if}

    <!-- Canvas -->
    {#if loading}
      <div class="flex-1 flex items-center justify-center text-sand-400">Loading workflow...</div>
    {:else if error}
      <div class="flex-1 flex items-center justify-center text-red-500">{error}</div>
    {:else}
      <div class="flex-1">
        <Svelvet
          width={browser ? window.innerWidth : 1200}
          height={browser ? window.innerHeight - 56 : 600}
          minimap
          controls
          theme="light"
        >
          <Background slot="background" />

          {#each nodes as node (node.id)}
            <Node
              id={node.id}
              position={node.position}
              label={node.label}
              bgColor={nodeColor(canvasKind(node))}
              textColor="white"
              width={160}
              height={48}
              borderRadius={8}
              on:nodeClicked={() => { selectedNode = node; }}
              on:nodeReleased={onNodeReleased(node.id)}
              let:disconnect
            >
              {(nodeDisconnectors[node.id] = disconnect, '')}

              <div class="flex flex-col items-center justify-center w-full h-full px-2 pointer-events-none select-none">
                <span class="text-xs font-semibold text-white truncate max-w-[140px]">{node.label}</span>
                <span class="text-[10px] text-white opacity-70">{canvasKind(node).replace(/_/g, ' ')}</span>
              </div>

              <svelte:fragment slot="anchorWest">
                <Anchor
                  id="{node.id}-in"
                  key="{node.id}-in"
                  input
                  direction="west"
                />
              </svelte:fragment>
              <svelte:fragment slot="anchorEast">
                <Anchor
                  id="{node.id}-out"
                  key="{node.id}-out"
                  output
                  multiple
                  direction="east"
                  connections={initialConnections(node.id)}
                  on:connection={onAnchorConnect(node.id)}
                  on:disconnection={onAnchorDisconnect(node.id)}
                />
              </svelte:fragment>
            </Node>
          {/each}
        </Svelvet>
      </div>
    {/if}
  </div>
</div>
