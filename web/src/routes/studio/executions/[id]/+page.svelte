<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { browser } from '$app/environment';
  import { page } from '$app/stores';
  import { goto } from '$app/navigation';
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  import { Svelvet, Node as NodeC, Anchor as AnchorC, Background } from 'svelvet';
  const Node = NodeC as any;
  const Anchor = AnchorC as any;
  import { api } from '$lib/api/client';
  import { getToken } from '$lib/stores/session';
  import type { Task, Workflow, WFNode, WFEdge, NodeExecution } from '$lib/api/types';
  import NodeOutput from '$lib/components/NodeOutput.svelte';

  $: taskId = $page.params.id;

  let task: Task | null = null;
  let nodes: WFNode[] = [];
  let edges: WFEdge[] = [];
  let execByNode: Record<string, NodeExecution> = {};
  let selectedNode: WFNode | null = null;
  let loading = true;
  let error = '';
  let nodeFeed: EventSource | null = null;

  // Evaluation grade for this task (present once delivered + the agent has criteria).
  interface CriterionResult { name: string; passed: boolean; priority: string; reasoning: string }
  interface Evaluation { grade: string; criteria_results: CriterionResult[]; tags: string[]; summary: string | null }
  let evaluation: Evaluation | null = null;
  async function loadEvaluation() {
    try {
      evaluation = await api.get<Evaluation | null>(`/evaluations/task/${taskId}`);
    } catch { evaluation = null; }
  }
  const GRADE_BADGE: Record<string, string> = { pass: 'badge-green', warning: 'badge-yellow', critical: 'badge-red' };
  let pollTimer: ReturnType<typeof setInterval> | null = null;

  // Node type registry — same palette as the workflow editor, plus a canvas-kind helper.
  const NODE_TYPES: Record<string, { label: string; color: string }> = {
    trigger: { label: 'Trigger', color: '#7c4dff' },
    llm_reasoning: { label: 'LLM Reasoning', color: '#6a34ff' },
    human_gate: { label: 'Human Gate', color: '#f59e0b' },
    conditional_branch: { label: 'Branch', color: '#7c4dff' },
    output: { label: 'Output', color: '#10b981' },
    formatter: { label: 'Formatter', color: '#7c4dff' },
    github: { label: 'GitHub', color: '#1f2937' },
    diff_analyzer: { label: 'Diff Analyzer', color: '#1f2937' },
    stack_trace_parser: { label: 'Stack Trace', color: '#1f2937' },
    http_request: { label: 'HTTP Request', color: '#0ea5e9' },
    slack: { label: 'Slack', color: '#4a154b' },
    email: { label: 'Email', color: '#ea4335' },
    web_search: { label: 'Web Search', color: '#34a853' },
    document_read_write: { label: 'Document', color: '#1a73e8' },
  };

  function canvasKind(node: WFNode): string {
    return (node.config?.canvas_kind as string) ?? node.node_type;
  }

  // Status → ring color for the node. The execution status drives the highlight.
  function nodeStatus(nodeId: string): string {
    const exec = execByNode[nodeId];
    if (!exec) {
      // No record yet: it's the current node if the task says so, else pending.
      return task?.current_node_id === nodeId ? 'current' : 'pending';
    }
    return exec.status;
  }

  function ringColor(status: string): string {
    switch (status) {
      case 'completed': return '#10b981';      // green
      case 'running': return '#f59e0b';         // amber
      case 'awaiting_review': return '#f59e0b'; // amber
      case 'awaiting_effect': return '#f97316'; // orange — needs a decision
      case 'current': return '#f59e0b';
      case 'failed': return '#ef4444';          // red
      default: return '#d1d5db';                // gray (pending)
    }
  }

  function nodeColor(node: WFNode): string {
    return NODE_TYPES[canvasKind(node)]?.color ?? '#6b7280';
  }

  function initialConnections(nodeId: string): [string, string][] {
    return edges.filter(e => e.source === nodeId).map(e => [e.target, `${e.target}-in`]);
  }

  async function loadExecutions() {
    const execs = await api.get<NodeExecution[]>(`/tasks/${taskId}/node-executions`);
    // Keep the latest attempt per node.
    const map: Record<string, NodeExecution> = {};
    for (const e of execs) {
      const prev = map[e.node_id];
      if (!prev || e.attempt >= prev.attempt) map[e.node_id] = e;
    }
    execByNode = map;
  }

  onMount(async () => {
    try {
      task = await api.get<Task>(`/tasks/${taskId}`);
      if (task.workflow_id) {
        const wf = await api.get<Workflow>(`/workflows/${task.workflow_id}`);
        nodes = wf.graph.nodes ?? [];
        edges = wf.graph.edges ?? [];
      }
      await loadExecutions();
      if (isTerminal(task.status)) await loadEvaluation();
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to load execution';
    } finally {
      loading = false;
    }

    if (browser) {
      const token = getToken();
      nodeFeed = new EventSource(`/api/v1/events/node-executions/${taskId}${token ? `?token=${token}` : ''}`);
      nodeFeed.addEventListener('node_update', refreshAll);
      // SSE can drop (long sprite runs, server restarts, network blips). On error the browser
      // auto-reconnects, but the event that fired while disconnected is lost — so refetch on
      // reconnect, and also poll on a timer so the DAG always converges to true state.
      nodeFeed.onerror = () => { refreshAll(); };
      pollTimer = setInterval(() => {
        if (isTerminal(task?.status)) { stopPolling(); return; }
        refreshAll();
      }, 4000);
    }
  });

  // Refresh the execution map + task. Tolerant of transient failures (kept polling).
  function refreshAll() {
    loadExecutions().catch(() => {});
    api.get<Task>(`/tasks/${taskId}`).then(t => {
      task = t;
      if (isTerminal(t.status) && !evaluation) loadEvaluation();
    }).catch(() => {});
  }

  function isTerminal(status: string | undefined): boolean {
    return status === 'delivered' || status === 'failed';
  }

  function stopPolling() {
    if (pollTimer) { clearInterval(pollTimer); pollTimer = null; }
  }

  onDestroy(() => { nodeFeed?.close(); stopPolling(); });

  $: selectedExec = selectedNode ? execByNode[selectedNode.id] : null;

  // ── Per-node chat ───────────────────────────────────────────────────────────
  interface ChatMessage { id: string; role: string; content: string; created_at: string }
  let chatOpen = false;
  let chatMessages: ChatMessage[] = [];
  let chatInput = '';
  let chatSending = false;
  let correctionText = '';
  let applying = false;
  let rerunning = false;

  async function openChat() {
    if (!selectedNode) return;
    chatOpen = true;
    try {
      chatMessages = await api.get<ChatMessage[]>(`/node-chat/${taskId}/${selectedNode.id}`);
    } catch { chatMessages = []; }
    // Seed the correction box with the latest assistant message if any.
    const lastAssistant = [...chatMessages].reverse().find(m => m.role === 'assistant');
    if (lastAssistant) correctionText = lastAssistant.content;
  }

  async function sendChat() {
    if (!selectedNode || !chatInput.trim()) return;
    chatSending = true;
    const text = chatInput.trim();
    chatInput = '';
    // Optimistic user message.
    chatMessages = [...chatMessages, { id: `tmp-${Date.now()}`, role: 'user', content: text, created_at: new Date().toISOString() }];
    try {
      const reply = await api.post<ChatMessage>(`/node-chat/${taskId}/${selectedNode.id}`, { message: text });
      chatMessages = [...chatMessages, reply];
      correctionText = reply.content;
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Message failed';
    } finally {
      chatSending = false;
    }
  }

  async function applyCorrection() {
    if (!selectedNode || !correctionText.trim()) return;
    applying = true;
    try {
      await api.post(`/node-chat/${taskId}/${selectedNode.id}/apply`, { corrected_output: correctionText.trim() });
      await loadExecutions(); // refresh dirty flags + output
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Apply failed';
    } finally {
      applying = false;
    }
  }

  async function rerunFrom() {
    if (!selectedNode) return;
    rerunning = true;
    try {
      await api.post(`/tasks/${taskId}/rerun/${selectedNode.id}`, {});
      // The SSE feed will stream the re-execution; refresh shortly.
      setTimeout(loadExecutions, 1500);
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Rerun failed';
    } finally {
      rerunning = false;
    }
  }

  // Reset chat state when switching nodes.
  $: if (selectedNode) { chatOpen = false; chatMessages = []; correctionText = ''; }

  $: anyDirty = Object.values(execByNode).some(e => e.dirty);

  // ── Human gate controls ─────────────────────────────────────────────────────
  // The selected node is the LIVE human gate when the task is awaiting_review and this node's
  // execution is the one paused there.
  $: isGate = !!selectedExec
    && selectedExec.status === 'awaiting_review'
    && task?.status === 'awaiting_review';
  let gateBusy = false;

  async function approveGate() {
    if (!selectedExec) return;
    gateBusy = true;
    try {
      // Approve the work as-is (no amendment) — the workflow continues past the gate.
      const approved = typeof selectedExec.output_json === 'string'
        ? selectedExec.output_json
        : JSON.stringify(selectedExec.output_json ?? '');
      await api.post(`/tasks/${taskId}/approve`, { approved_output: approved, amended: false });
      refreshAll();
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Approve failed';
    } finally {
      gateBusy = false;
    }
  }

  async function rejectGate() {
    gateBusy = true;
    try {
      // Send the task back to in-progress; the professional fixes via Chat → Apply → Rerun below.
      await api.post(`/tasks/${taskId}/reject`, {});
      refreshAll();
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Reject failed';
    } finally {
      gateBusy = false;
    }
  }

  async function killTask() {
    if (!confirm('Kill this execution? The sprite is torn down and this run is reset. The task is kept — you can run it again (and refine the workflow) without recreating it.')) return;
    gateBusy = true;
    try {
      await api.post(`/tasks/${taskId}/kill`, {});
      // The task is now re-runnable — the executions list shows a "Run again" card for it.
      goto('/studio/executions');
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Kill failed';
    } finally {
      gateBusy = false;
    }
  }

  // ── Side-effect gate ────────────────────────────────────────────────────────
  let deciding = false;
  $: gateExec = selectedNode && selectedExec?.status === 'awaiting_effect' ? selectedExec : null;

  // True if a completed node's output records an external resource (i.e. it did a write).
  function didSideEffect(exec: NodeExecution | null): boolean {
    if (!exec || exec.status !== 'completed') return false;
    const out = exec.output_json as Record<string, unknown> | null;
    return !!out && typeof out === 'object' && 'resource_kind' in out;
  }

  async function resolveEffect(decision: 'reuse' | 'update' | 'refire') {
    if (!selectedNode) return;
    deciding = true;
    try {
      await api.post(`/tasks/${taskId}/node/${selectedNode.id}/effect-decision`, { decision });
      setTimeout(() => { loadExecutions(); api.get<Task>(`/tasks/${taskId}`).then(t => { task = t; }).catch(() => {}); }, 1200);
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Decision failed';
    } finally {
      deciding = false;
    }
  }
</script>

<svelte:head><title>Execution — Flaborful Studio</title></svelte:head>

<div class="h-[calc(100vh-56px)] flex flex-col">
  <!-- Toolbar -->
  <div class="bg-white border-b border-sand-200 px-4 py-2 flex items-center gap-3 z-10">
    <button class="btn-ghost text-sm" on:click={() => goto('/studio/executions')}>← Executions</button>
    <div class="h-4 w-px bg-sand-200"></div>
    {#if task}
      <span class="text-sm font-semibold">Task {task.id.slice(0, 8)}</span>
      <span class="badge-{task.status === 'delivered' ? 'green' : task.status === 'failed' ? 'red' : task.status === 'awaiting_review' ? 'yellow' : 'blue'} text-xs capitalize">{task.status.replace(/_/g, ' ')}</span>
      {#if evaluation}
        <span class="{GRADE_BADGE[evaluation.grade] ?? 'badge-gray'} text-xs capitalize"
          title={evaluation.summary ?? ''}>
          Eval: {evaluation.grade}
          {#if evaluation.criteria_results?.length}
            ({evaluation.criteria_results.filter(c => c.passed).length}/{evaluation.criteria_results.length})
          {/if}
        </span>
      {/if}
    {/if}
    <div class="flex-1"></div>
    <!-- Legend -->
    <div class="flex items-center gap-3 text-xs text-sand-500">
      <span class="flex items-center gap-1"><span class="w-2.5 h-2.5 rounded-full" style="background:#10b981"></span>done</span>
      <span class="flex items-center gap-1"><span class="w-2.5 h-2.5 rounded-full" style="background:#f59e0b"></span>current</span>
      <span class="flex items-center gap-1"><span class="w-2.5 h-2.5 rounded-full" style="background:#dccabc"></span>pending</span>
      <span class="flex items-center gap-1"><span class="w-2.5 h-2.5 rounded-full" style="background:#ef4444"></span>failed</span>
    </div>
  </div>

  <div class="flex-1 flex overflow-hidden relative">
    <!-- Inspector -->
    {#if selectedNode}
      <div class="absolute right-4 top-4 z-20 bg-white rounded-xl border border-sand-200 shadow-lg w-96 max-h-[85vh] overflow-y-auto">
        <div class="px-4 py-3 border-b border-sand-100 flex items-center justify-between">
          <div>
            <span class="font-semibold text-sm">{selectedNode.label}</span>
            <span class="block text-xs text-sand-400">{canvasKind(selectedNode).replace(/_/g, ' ')}</span>
          </div>
          <button class="text-sand-400 hover:text-sand-600 text-lg leading-none" on:click={() => selectedNode = null}>×</button>
        </div>
        <div class="p-4 space-y-3 text-sm">
          <div class="flex items-center gap-2 flex-wrap">
            <span class="w-2.5 h-2.5 rounded-full shrink-0" style="background:{ringColor(nodeStatus(selectedNode.id))}"></span>
            <span class="capitalize">{nodeStatus(selectedNode.id).replace(/_/g, ' ')}</span>
            {#if selectedExec && selectedExec.attempt > 1}<span class="text-xs text-sand-400">attempt {selectedExec.attempt}</span>{/if}
            {#if !gateExec && didSideEffect(selectedExec)}
              <span class="badge-yellow text-xs" title="This node performed an external write">⚡ side effect applied</span>
            {/if}
          </div>

          <!-- Side-effect gate. Two flavors:
               • First-run policy gate (prior_output is null): the agent is ABOUT to act and your
                 tool-approval policy asked for a check → Approve / Skip.
               • Rerun gate (prior_output present): the node already wrote → reuse / update / refire. -->
          {#if gateExec?.pending_effect}
            {@const isFirstRun = gateExec.pending_effect.prior_output == null}
            <div class="bg-amber-50 border border-amber-200 rounded-lg p-3 text-xs space-y-2">
              {#if isFirstRun}
                <div class="font-semibold text-amber-800">Your agent wants to take an external action ({gateExec.pending_effect.effect_kind.replace(/_/g, ' ')}).</div>
                <p class="text-amber-700">Your approval policy asks you to OK this before it happens.</p>
                <div class="flex flex-col gap-1.5">
                  <button class="btn-primary text-xs" disabled={deciding} on:click={() => resolveEffect('refire')}>
                    Approve — go ahead and do it
                  </button>
                  <button class="btn-secondary text-xs" disabled={deciding} on:click={() => resolveEffect('reuse')}>
                    Skip — don't do this action
                  </button>
                </div>
              {:else}
                <div class="font-semibold text-amber-800">This stage already did an external action ({gateExec.pending_effect.effect_kind.replace(/_/g, ' ')}).</div>
                <p class="text-amber-700">Re-running could duplicate it. How should we handle it?</p>
                <div class="flex flex-col gap-1.5">
                  <button class="btn-secondary text-xs" disabled={deciding} on:click={() => resolveEffect('reuse')}>
                    Reuse — keep the existing one, don't redo it
                  </button>
                  {#if gateExec.pending_effect.supports_update}
                    <button class="btn-primary text-xs" disabled={deciding} on:click={() => resolveEffect('update')}>
                      Update — edit the existing one with the new content
                    </button>
                  {/if}
                  <button class="btn-danger text-xs" disabled={deciding} on:click={() => resolveEffect('refire')}>
                    Post fresh — create a new one
                  </button>
                </div>
                {#if !gateExec.pending_effect.supports_update}
                  <p class="text-[11px] text-amber-600">(This action type can't be edited in place — only reuse or post fresh.)</p>
                {/if}
              {/if}
            </div>
          {/if}

          {#if isGate}
            <div class="bg-amber-50 border border-amber-200 rounded-lg px-3 py-3 space-y-2">
              <div class="font-semibold text-amber-800 text-sm">Awaiting your review</div>
              <p class="text-xs text-amber-700">
                Approve to continue the workflow, reject to send it back for changes, or kill this run.
              </p>
              <div class="flex flex-col gap-1.5">
                <button class="btn-primary text-xs" disabled={gateBusy} on:click={approveGate}>
                  ✓ Approve &amp; continue
                </button>
                <button class="btn-secondary text-xs" disabled={gateBusy} on:click={rejectGate}>
                  ↩ Reject — request changes
                </button>
                <button class="btn-danger text-xs" disabled={gateBusy} on:click={killTask}>
                  ✕ Kill run (keep task, run again later)
                </button>
              </div>
              <p class="text-[11px] text-amber-600">
                After rejecting, use Chat → Apply correction → Rerun below to fix and re-run.
              </p>
            </div>
          {/if}

          {#if selectedExec}
            <div>
              <div class="text-xs font-semibold text-sand-500 uppercase tracking-wide mb-1">Input</div>
              <NodeOutput value={selectedExec.input_json} />
            </div>
            <div>
              <div class="text-xs font-semibold text-sand-500 uppercase tracking-wide mb-1">Output</div>
              <NodeOutput value={selectedExec.output_json} />
            </div>
            {#if selectedExec.logs}
              <div>
                <div class="text-xs font-semibold text-sand-500 uppercase tracking-wide mb-1">Logs</div>
                <pre class="bg-sand-50 rounded p-2 text-xs whitespace-pre-wrap break-words max-h-32 overflow-y-auto">{selectedExec.logs}</pre>
              </div>
            {/if}

            {#if selectedExec.dirty}
              <div class="bg-amber-50 border border-amber-200 rounded-lg px-3 py-2 text-xs text-amber-800">
                This node has a staged correction that hasn't been re-run yet.
                <button class="block mt-1.5 btn-primary text-xs" disabled={rerunning} on:click={rerunFrom}>
                  {rerunning ? 'Re-running…' : 'Rerun from here ↓'}
                </button>
              </div>
            {/if}

            <!-- Chat about this node's work -->
            <div class="border-t border-sand-100 pt-3">
              {#if !chatOpen}
                <button class="btn-secondary text-xs w-full" on:click={openChat}>💬 Chat about this work</button>
              {:else}
                <div class="text-xs font-semibold text-sand-500 uppercase tracking-wide mb-2">Chat with your agent</div>
                <div class="space-y-2 max-h-56 overflow-y-auto mb-2">
                  {#each chatMessages as m (m.id)}
                    <div class="text-xs rounded-lg px-2.5 py-1.5 {m.role === 'user' ? 'bg-brand-50 text-brand-900 ml-6' : 'bg-sand-100 text-sand-700 mr-6'}">
                      {m.content}
                    </div>
                  {/each}
                  {#if chatSending}<div class="text-xs text-sand-400">Agent is thinking…</div>{/if}
                </div>
                <div class="flex gap-1.5 mb-3">
                  <input class="input text-xs flex-1" placeholder="Ask the agent to fix or revise…"
                    bind:value={chatInput}
                    on:keydown={(e) => e.key === 'Enter' && sendChat()} />
                  <button class="btn-primary text-xs" disabled={chatSending || !chatInput.trim()} on:click={sendChat}>Send</button>
                </div>

                <!-- Apply a correction -->
                <div class="text-xs font-semibold text-sand-500 uppercase tracking-wide mb-1">Corrected output</div>
                <textarea class="input text-xs min-h-[80px] resize-y" bind:value={correctionText}
                  placeholder="Edit the work here, then apply. This stages the change; downstream nodes are marked for re-run."></textarea>
                <button class="btn-secondary text-xs w-full mt-1.5" disabled={applying || !correctionText.trim()} on:click={applyCorrection}>
                  {applying ? 'Applying…' : 'Apply correction (stage)'}
                </button>
              {/if}
            </div>
          {:else}
            <p class="text-xs text-sand-400">This node hasn't run yet.</p>
          {/if}
        </div>
      </div>
    {/if}

    <!-- Canvas -->
    {#if loading}
      <div class="flex-1 flex items-center justify-center text-sand-400">Loading execution…</div>
    {:else if error}
      <div class="flex-1 flex items-center justify-center text-red-500">{error}</div>
    {:else if nodes.length === 0}
      <div class="flex-1 flex items-center justify-center text-sand-400">No workflow attached to this task.</div>
    {:else}
      <div class="flex-1">
        <Svelvet
          width={browser ? window.innerWidth : 1200}
          height={browser ? window.innerHeight - 56 : 600}
          minimap controls theme="light">
          <Background slot="background" />

          {#each nodes as node (node.id)}
            <Node
              id={node.id}
              position={node.position}
              bgColor={nodeColor(node)}
              textColor="white"
              borderColor={ringColor(nodeStatus(node.id))}
              borderWidth={4}
              width={170}
              height={56}
              borderRadius={8}
              on:nodeClicked={() => { selectedNode = node; }}>

              <div class="flex flex-col items-center justify-center w-full h-full px-2 pointer-events-none select-none relative">
                {#if execByNode[node.id]?.dirty}
                  <span class="absolute top-1 right-1.5 text-[10px]" title="Staged correction — needs re-run">●</span>
                {/if}
                <span class="text-xs font-semibold text-white truncate max-w-[150px]">{node.label}</span>
                <span class="text-[10px] text-white opacity-80 capitalize">{nodeStatus(node.id).replace(/_/g, ' ')}</span>
              </div>

              <svelte:fragment slot="anchorWest">
                <Anchor id="{node.id}-in" key="{node.id}-in" input direction="west" />
              </svelte:fragment>
              <svelte:fragment slot="anchorEast">
                <Anchor id="{node.id}-out" key="{node.id}-out" output multiple direction="east"
                  connections={initialConnections(node.id)} />
              </svelte:fragment>
            </Node>
          {/each}
        </Svelvet>
      </div>
    {/if}
  </div>
</div>
