<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { browser } from '$app/environment';
  import { page } from '$app/stores';
  import { goto } from '$app/navigation';
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  import { Svelvet, Node as NodeC, Anchor as AnchorC, Background } from 'svelvet';
  const Node = NodeC as any;
  const Anchor = AnchorC as any;
  import { api } from '$lib/api/client';
  import {
    loadManifests,
    CATEGORY_LABELS,
    CATEGORY_ORDER,
    type NodeManifest,
  } from '$lib/api/nodeManifests';
  import FieldRenderer from '$lib/components/nodes/FieldRenderer.svelte';
  import PositionTracker from '$lib/components/PositionTracker.svelte';

  // Node type metadata now comes from the backend registry (GET /node-manifests), keyed by type.
  let manifests: NodeManifest[] = [];
  let manifestByType: Record<string, NodeManifest> = {};
  let paletteSearch = '';

  // Focus the palette search when it opens (action avoids the autofocus a11y lint).
  function focusOnMount(node: HTMLElement) {
    node.focus();
  }

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

  let workflow: { id: string; agent_id: string; name: string; description: string; graph: { nodes: WFNode[]; edges: WFEdge[] }; is_universal: boolean; execution_environment: string | null } | null = null;
  let showSettings = false;
  // Other workflows belonging to this agent — the pick-list for a Subflow node's target.
  let siblingWorkflows: { id: string; name: string }[] = [];
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
  // Live (dragged) positions reported continuously by <PositionTracker>. Keyed by node id.
  const livePositions: Record<string, { x: number; y: number }> = {};

  // Auto-center the graph once after the workflow loads. PositionTracker.fit() reads
  // svelvet's node bounds from the graph context and applies the same math as svelvet's own
  // fit-view control. Bounds only populate after the <Node> components mount (one or two
  // ticks after the parent), so we retry until fit() reports it actually applied.
  let positionTracker: PositionTracker | null = null;
  let didAutoFit = false;
  async function tryAutoFit() {
    for (let i = 0; i < 10 && !didAutoFit; i++) {
      await tick();
      if (positionTracker?.fit()) didAutoFit = true;
    }
  }
  $: if (!loading && nodes.length > 0 && !didAutoFit) tryAutoFit();

  onMount(async () => {
    try {
      const [wf, mans] = await Promise.all([
        api.get<typeof workflow>(`/workflows/${workflowId}`),
        loadManifests(),
      ]);
      manifests = mans;
      manifestByType = Object.fromEntries(mans.map((m) => [m.type, m]));
      workflow = wf;
      if (workflow) {
        nodes = workflow.graph.nodes ?? [];
        edges = workflow.graph.edges ?? [];
        // Load the agent's other workflows so a Subflow node can pick which one to run.
        api
          .get<{ id: string; name: string }[]>(`/workflows/agent/${workflow.agent_id}`)
          .then((list) => { siblingWorkflows = list.filter((w) => w.id !== workflow!.id); })
          .catch(() => { siblingWorkflows = []; });
      }
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to load workflow';
    } finally {
      loading = false;
    }
  });

  // The manifest for a node, resolved via its canvas kind (alias-aware).
  function manifestFor(node: WFNode): NodeManifest | undefined {
    return manifestByType[canvasKind(node)];
  }

  // Fields visible for the currently-selected node, honoring `visible_when` and advanced toggle.
  $: selectedManifest = selectedNode ? manifestFor(selectedNode) : undefined;
  // A node is upgradeable when its stored version is behind the manifest's current version.
  $: nodeVersion = Number(selectedNode?.config?._node_version ?? 1);
  $: canUpgrade = !!selectedManifest && nodeVersion < selectedManifest.version;
  function upgradeNode() {
    if (!selectedNode || !selectedManifest) return;
    selectedNode.config._node_version = selectedManifest.version;
    // Seed any new-in-this-version fields with their defaults (non-destructive to existing config).
    for (const f of selectedManifest.inputs) {
      if (f.default !== undefined && selectedNode.config[f.key] === undefined) {
        selectedNode.config[f.key] = f.default;
      }
    }
    nodes = syncedNodes();
  }
  let showAdvanced = false;
  $: visibleFields = (selectedManifest?.inputs ?? []).filter((f) => {
    if (f.visible_when && selectedNode) {
      const [dep, val] = f.visible_when;
      if (String(selectedNode.config[dep] ?? '') !== val) return false;
    }
    return true;
  });
  $: basicFields = visibleFields.filter((f) => !f.advanced);
  $: advancedFields = visibleFields.filter((f) => f.advanced);

  // Palette grouped by category, filtered by search.
  $: palette = CATEGORY_ORDER.map((cat) => ({
    cat,
    label: CATEGORY_LABELS[cat],
    items: manifests.filter(
      (m) =>
        m.category === cat &&
        (!paletteSearch ||
          m.label.toLowerCase().includes(paletteSearch.toLowerCase()) ||
          m.type.includes(paletteSearch.toLowerCase())),
    ),
  })).filter((g) => g.items.length);

  function onConnection(sourceNodeId: string, targetNodeId: string) {
    const edgeId = `${sourceNodeId}-${targetNodeId}`;
    if (!edges.find(e => e.id === edgeId)) {
      edges = [...edges, { id: edgeId, source: sourceNodeId, target: targetNodeId }];
    }
  }

  function onDisconnection(sourceNodeId: string, targetNodeId: string) {
    edges = edges.filter(e => !(e.source === sourceNodeId && e.target === targetNodeId));
  }

  // on:nodeReleased in svelvet only fires on a click (mouse moved < 4px), NOT on a real
  // drag, so it is useless for capturing dragged positions. Live positions are instead
  // tracked continuously by <PositionTracker> (see the <Svelvet> block), which subscribes
  // to svelvet's internal per-node position stores via the graph context and writes them
  // into `livePositions` via on:position. syncedNodes() then folds those live positions
  // back into WFNode.position before any `nodes = [...]` reassignment so the {#each}
  // re-render doesn't snap the graph back to its add-time spawn layout.
  // PositionTracker dispatches { id, x, y }; we just stash it into livePositions.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const onPosition = (e: any) => {
    const { id, x, y } = e.detail ?? {};
    if (id && typeof x === 'number' && typeof y === 'number') {
      livePositions[id] = { x, y };
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

  // Some canvas node types are presentation-only aliases over a backend NodeType with
  // preset config. google_docs → document_read_write with provider=google_docs.
  const NODE_TYPE_PRESETS: Record<string, { node_type: string; config: Record<string, unknown> }> = {
    google_docs: {
      node_type: 'document_read_write',
      config: { provider: 'google_docs', action: 'create', model_var: 'llm_output' },
    },
  };

  // The canvas "kind" of a node: its alias if it has one (via config.canvas_kind),
  // otherwise its raw node_type. Used for manifest/config lookups so preset nodes
  // keep their identity after save/reload.
  function canvasKind(node: WFNode): string {
    return (node.config?.canvas_kind as string) ?? node.node_type;
  }

  function nodeColor(type: string) {
    return manifestByType[type]?.color ?? '#6b7280';
  }

  function nodeLabel(type: string) {
    return manifestByType[type]?.label ?? type;
  }

  function addNode(type: string) {
    const label = nodeLabel(type);
    const preset = NODE_TYPE_PRESETS[type];
    const manifest = manifestByType[type];
    // Seed config with the manifest's field defaults (and preset config for aliases).
    const seeded: Record<string, unknown> = { ...(preset?.config ?? {}) };
    for (const f of manifest?.inputs ?? []) {
      if (f.default !== undefined && seeded[f.key] === undefined) seeded[f.key] = f.default;
    }
    if (preset) seeded.canvas_kind = type;
    // Stamp the manifest version this node was authored at, so a later manifest bump can offer
    // an opt-in upgrade rather than silently changing existing graphs.
    seeded._node_version = manifest?.version ?? 1;
    const newNode: WFNode = {
      id: `${type}_${Date.now()}`,
      node_type: preset?.node_type ?? type,
      label,
      config: seeded,
      position: { x: 200 + nodes.length * 60, y: 200 + (nodes.length % 3) * 80 },
    };
    nodes = [...syncedNodes(), newNode];
    showAddPanel = false;
    selectedNode = newNode;
  }

  function removeNode(id: string) {
    nodes = syncedNodes().filter(n => n.id !== id);
    edges = edges.filter(e => e.source !== id && e.target !== id);
    delete livePositions[id];
    if (selectedNode?.id === id) selectedNode = null;
  }

  // Return `nodes` with each node's live (dragged) position merged in. Reassigning `nodes`
  // re-renders the canvas from each node's `position` field — which is the stale add-time
  // position unless we fold in `livePositions`. Without this, editing any node config snaps the
  // whole graph back to its ugly spawn layout.
  function syncedNodes(): WFNode[] {
    return nodes.map(n => livePositions[n.id] ? { ...n, position: livePositions[n.id] } : n);
  }

  // A sensible starting prompt for an LLM node that authors a document. The engine adds
  // the strict-JSON DocumentModel schema to the system prompt automatically when
  // document_author is on, so this only needs to describe the content to write.
  const DOC_AUTHOR_PROMPT_DEFAULT =
    'Write a professional document for the following request, in your own voice and to your usual standard:\n\n{{input}}';

  // Single generic config setter driven by the manifest field renderer — handles every field type
  // (string, number-as-string, boolean, key_value/repeater arrays).
  function updateConfigValue(key: string, value: unknown) {
    if (!selectedNode) return;
    selectedNode.config[key] = value;
    // Preserve the doc-author convenience: seed a starter prompt when it's turned on.
    if (key === 'document_author' && value && !String(selectedNode.config.prompt ?? '').trim()) {
      selectedNode.config.prompt = DOC_AUTHOR_PROMPT_DEFAULT;
    }
    nodes = syncedNodes();
  }

  function duplicateNode() {
    if (!selectedNode) return;
    const kind = canvasKind(selectedNode);
    const copy: WFNode = {
      id: `${kind}_${Date.now()}`,
      node_type: selectedNode.node_type,
      label: selectedNode.label,
      config: JSON.parse(JSON.stringify(selectedNode.config)),
      position: { x: selectedNode.position.x + 40, y: selectedNode.position.y + 40 },
    };
    nodes = [...syncedNodes(), copy];
    selectedNode = copy;
  }

  function onEnvChange(e: Event) {
    if (!workflow) return;
    const v = (e.currentTarget as HTMLSelectElement).value;
    workflow.execution_environment = v === '' ? null : v;
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

  // ── Checkpoints (graph version history) ──────────────────────────────────
  interface Checkpoint { id: string; number: number; label: string | null; created_at: string }
  let checkpoints: Checkpoint[] = [];
  let showHistory = false;

  async function loadCheckpoints() {
    try {
      checkpoints = await api.get<Checkpoint[]>(`/workflows/${workflowId}/checkpoints`);
    } catch { checkpoints = []; }
  }
  async function saveCheckpoint() {
    await save(); // snapshot the current saved graph
    if (error) return;
    try {
      const label = prompt('Checkpoint label (optional):') ?? undefined;
      await api.post(`/workflows/${workflowId}/checkpoints`, { label });
      await loadCheckpoints();
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to save checkpoint';
    }
  }
  async function restoreCheckpoint(cp: Checkpoint) {
    if (!confirm(`Restore checkpoint #${cp.number}? Your current draft is saved as a checkpoint first.`)) return;
    try {
      const res = await api.post<{ graph: { nodes: WFNode[]; edges: WFEdge[] } }>(`/workflows/${workflowId}/checkpoints/${cp.id}/restore`, {});
      nodes = res.graph.nodes ?? [];
      edges = res.graph.edges ?? [];
      if (workflow) workflow.graph = res.graph;
      selectedNode = null;
      await loadCheckpoints();
      showHistory = false;
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to restore checkpoint';
    }
  }
  function toggleHistory() {
    showHistory = !showHistory;
    if (showHistory) loadCheckpoints();
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

    <button class="btn-ghost text-sm" on:click={() => showSettings = !showSettings}>Settings</button>
    <button class="btn-ghost text-sm" on:click={toggleHistory}>History</button>
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
    <!-- Workflow settings -->
    {#if showSettings && workflow}
      <div class="absolute right-4 top-4 z-30 bg-white rounded-xl border border-sand-200 shadow-lg w-80 max-h-[85vh] flex flex-col">
        <div class="px-4 py-3 border-b border-sand-100 flex items-center justify-between">
          <span class="font-semibold text-sm">Workflow settings</span>
          <button class="text-sand-400 hover:text-sand-600 text-lg leading-none" on:click={() => showSettings = false}>×</button>
        </div>
        <div class="p-4 space-y-2">
          <label class="label text-xs" for="exec-env">Execution environment</label>
          <select
            class="input text-sm w-full"
            id="exec-env"
            value={workflow.execution_environment ?? ''}
            on:change={onEnvChange}
          >
            <option value="">Optimizer fbx-agent (default · cheapest)</option>
            <option value="optimizer_fbx">Optimizer fbx-agent (cheapest)</option>
            <option value="claude_code_byok">Claude Code (your Anthropic key)</option>
            <option value="managed_cloud">Managed Claude cloud (premium)</option>
          </select>
          <p class="text-xs text-sand-400">
            How this workflow's software-engineering steps run. Applies to every sprite node in the
            workflow so the coding agent stays consistent across the run. Save to apply.
          </p>
        </div>
      </div>
    {/if}

    <!-- Checkpoint history -->
    {#if showHistory}
      <div class="absolute right-4 top-4 z-30 bg-white rounded-xl border border-sand-200 shadow-lg w-72 max-h-[85vh] flex flex-col">
        <div class="px-4 py-3 border-b border-sand-100 flex items-center justify-between">
          <span class="font-semibold text-sm">Version history</span>
          <button class="text-sand-400 hover:text-sand-600 text-lg leading-none" on:click={() => showHistory = false}>×</button>
        </div>
        <div class="p-3">
          <button class="btn-secondary text-sm w-full mb-2" on:click={saveCheckpoint}>Save checkpoint</button>
          {#if checkpoints.length === 0}
            <p class="text-xs text-sand-400 text-center py-3">No checkpoints yet. Save one to snapshot this graph.</p>
          {:else}
            <ul class="space-y-1 overflow-y-auto">
              {#each checkpoints as cp (cp.id)}
                <li class="flex items-center gap-2 px-2 py-1.5 rounded hover:bg-sand-50">
                  <div class="flex-1 min-w-0">
                    <div class="text-sm font-medium">#{cp.number}{#if cp.label} · {cp.label}{/if}</div>
                    <div class="text-[11px] text-sand-400">{new Date(cp.created_at).toLocaleString()}</div>
                  </div>
                  <button class="text-xs text-brand-600 hover:underline shrink-0" on:click={() => restoreCheckpoint(cp)}>Restore</button>
                </li>
              {/each}
            </ul>
          {/if}
        </div>
      </div>
    {/if}

    <!-- Node palette (manifest-driven, categorized + searchable) -->
    {#if showAddPanel}
      <div class="absolute left-4 top-4 z-20 bg-white rounded-xl border border-sand-200 shadow-lg w-64 max-h-[85vh] flex flex-col">
        <div class="p-2 border-b border-sand-100">
          <input
            class="input text-sm"
            placeholder="Search all nodes…"
            bind:value={paletteSearch}
            use:focusOnMount />
        </div>
        <div class="overflow-y-auto py-1">
          {#each palette as group}
            <div class="px-3 py-1 mt-1 text-xs font-semibold text-sand-500">{group.label}</div>
            {#each group.items as m}
              <button
                class="w-full text-left px-3 py-2 text-sm hover:bg-sand-50 flex items-start gap-2.5"
                title={m.help}
                on:click={() => addNode(m.type)}>
                <span class="text-base leading-none mt-0.5" style="filter:grayscale(0)">{m.icon}</span>
                <span class="flex-1 min-w-0">
                  <span class="flex items-center gap-1.5">
                    <span class="w-2 h-2 rounded-full shrink-0" style="background:{m.color}"></span>
                    <span class="font-medium text-sand-800 truncate">{m.label}</span>
                  </span>
                  <span class="block text-[11px] text-sand-400 truncate">{m.help}</span>
                </span>
              </button>
            {/each}
          {/each}
          {#if palette.length === 0}
            <div class="px-3 py-4 text-xs text-sand-400 text-center">No nodes match “{paletteSearch}”.</div>
          {/if}
        </div>
      </div>
    {/if}

    <!-- Config panel (manifest-driven) -->
    {#if selectedNode}
      <div class="absolute right-4 top-4 z-20 bg-white rounded-xl border border-sand-200 shadow-lg w-80 max-h-[85vh] overflow-y-auto">
        <div class="px-4 py-3 border-b border-sand-100 flex items-center gap-2">
          <span class="text-base leading-none">{selectedManifest?.icon ?? '⬦'}</span>
          <span class="font-semibold text-sm flex-1 truncate">{selectedNode.label}</span>
          <button class="text-sand-400 hover:text-sand-600 text-xs" title="Duplicate node" on:click={duplicateNode}>⧉</button>
          <button class="text-sand-400 hover:text-sand-600 text-lg leading-none" on:click={() => selectedNode = null}>×</button>
        </div>
        <div class="p-4 space-y-3">
          {#if selectedManifest?.help}
            <p class="text-xs text-sand-500 bg-sand-50 border border-sand-100 rounded-lg p-2.5 leading-relaxed">
              {selectedManifest.help}
            </p>
          {/if}

          {#if canUpgrade}
            <div class="text-xs bg-amber-50 border border-amber-200 rounded-lg p-2.5 flex items-center gap-2">
              <span class="text-amber-800 flex-1">This node is v{nodeVersion}; v{selectedManifest?.version} is available.</span>
              <button class="btn-secondary text-xs shrink-0" on:click={upgradeNode}>Upgrade</button>
            </div>
          {/if}

          <div>
            <label class="label text-xs" for="node-label">Node label</label>
            <input class="input text-sm" id="node-label"
              bind:value={selectedNode.label}
              on:input={() => { nodes = syncedNodes(); }} />
          </div>

          {#if selectedManifest?.loopable}
            <label class="flex items-center gap-2 text-xs text-sand-600 cursor-pointer">
              <input type="checkbox" class="accent-brand-600"
                checked={Boolean(selectedNode.config.loop_mode)}
                on:change={(e) => updateConfigValue('loop_mode', e.currentTarget.checked)} />
              <span class="font-medium">Loop Mode</span>
              <span class="text-sand-400">— run once per item over a list</span>
            </label>
          {/if}

          {#each basicFields as field (field.key)}
            {#if field.key === 'workflow_id'}
              <!-- Subflow target: a pick-list of the agent's other workflows. -->
              <div>
                <label class="label text-xs" for="cfg-workflow_id">{field.label}<span class="text-rose-500"> *</span></label>
                <select class="input text-sm" id="cfg-workflow_id"
                  value={String(selectedNode.config.workflow_id ?? '')}
                  on:change={(e) => updateConfigValue('workflow_id', e.currentTarget.value)}>
                  <option value="">— select a workflow —</option>
                  {#each siblingWorkflows as wf}
                    <option value={wf.id}>{wf.name}</option>
                  {/each}
                </select>
                {#if siblingWorkflows.length === 0}
                  <p class="text-[11px] text-sand-400 mt-1">No other workflows yet — create one to run it as a subflow.</p>
                {/if}
              </div>
            {:else}
              <FieldRenderer {field} config={selectedNode.config} onChange={updateConfigValue} />
            {/if}
          {/each}

          {#if advancedFields.length}
            <button class="text-xs text-brand-600 hover:underline" on:click={() => showAdvanced = !showAdvanced}>
              {showAdvanced ? '− Hide' : '+ Show more'} options
            </button>
            {#if showAdvanced}
              <div class="space-y-3 pt-1">
                {#each advancedFields as field (field.key)}
                  <FieldRenderer {field} config={selectedNode.config} onChange={updateConfigValue} />
                {/each}
              </div>
            {/if}
          {/if}

          {#if selectedManifest && selectedManifest.outputs.length}
            <div>
              <div class="text-xs font-medium text-sand-500 mb-1">Outputs</div>
              <div class="flex flex-wrap gap-1.5">
                {#each selectedManifest.outputs as port}
                  <span class="badge-gray text-[11px]" title="type: {port.type}">{port.label}</span>
                {/each}
              </div>
            </div>
          {/if}

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
      <div class="flex-1 min-h-0">
        <Svelvet
          minimap
          controls
          theme="light"
        >
          <Background slot="background" />

          <PositionTracker bind:this={positionTracker} on:position={onPosition} />

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
