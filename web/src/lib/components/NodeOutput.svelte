<script lang="ts">
  // Typed renderer for a workflow node's output (and input). Detects the shape and renders a
  // readable view instead of one raw JSON blob: diffs get the diff viewer, agent summaries render
  // as markdown, test gates show pass/fail, side-effect writes link the artifact. Falls back to
  // pretty JSON (collapsible) for anything unrecognized. Keeps the engine output contract in mind:
  //  - AgentImplement → { diff, files_changed, transcript, summary, test_summary, branch, status }
  //  - TestGate       → { passed, attempts, check }
  //  - write nodes    → { status, resource_kind, resource_id, pr_url?, url? }
  import { marked } from 'marked';
  import DiffView from './DiffView.svelte';

  export let value: unknown;

  // marked v18 escapes raw HTML by default — safe for agent-authored markdown summaries.
  function md(s: string): string {
    try { return marked.parse(s, { async: false }) as string; } catch { return s; }
  }
  function pretty(v: unknown): string {
    if (v == null) return '—';
    if (typeof v === 'string') return v;
    try { return JSON.stringify(v, null, 2); } catch { return String(v); }
  }
  const isObj = (v: unknown): v is Record<string, unknown> =>
    typeof v === 'object' && v !== null && !Array.isArray(v);

  // Narrow a record field to a string (or '') without inline casts (Svelte 4 chokes on `as` in
  // template expressions, so all narrowing lives here in the script).
  function str(o: Record<string, unknown> | null, k: string): string {
    return o && typeof o[k] === 'string' ? (o[k] as string) : '';
  }
  function arr(o: Record<string, unknown> | null, k: string): unknown[] {
    return o && Array.isArray(o[k]) ? (o[k] as unknown[]) : [];
  }
  function num(o: Record<string, unknown> | null, k: string): number {
    return o && typeof o[k] === 'number' ? (o[k] as number) : 0;
  }
  // Format a USD cost: 4 sig-ish figures for small values, so $0.0293 doesn't show as $0.03.
  function usd(n: number): string {
    if (n === 0) return '$0';
    if (n < 0.01) return `$${n.toFixed(4)}`;
    return `$${n.toFixed(2)}`;
  }

  $: obj = isObj(value) ? value : null;
  $: diffText = str(obj, 'diff');
  $: hasDiff = diffText.length > 0;
  $: isTestGate = !!obj && typeof obj.passed === 'boolean' && 'attempts' in obj;
  $: gatePassed = !!obj && obj.passed === true;
  $: resourceKind = str(obj, 'resource_kind') || null;
  $: status = str(obj, 'status');
  $: summaryText = str(obj, 'summary');
  $: testSummary = str(obj, 'test_summary');
  $: transcript = str(obj, 'transcript');
  $: filesChanged = arr(obj, 'files_changed');
  $: resourceId = str(obj, 'resource_id');
  $: artifactUrl = str(obj, 'pr_url') || str(obj, 'url') || '';
  $: stringValue = typeof value === 'string' ? value : '';

  // Optimizer cost/savings (fbx-agent runs route every LLM call through the edge). Present only when
  // the run used the optimizer. `served` = the honest optimized-path cost; `saved` = vs the baseline
  // model. `saved ≈ 0` with actual > served means an *exploration* run (the optimizer paid for two
  // calls to learn this task class — savings kick in once it's learned-safe).
  $: cost = isObj(obj?.['cost']) ? (obj!['cost'] as Record<string, unknown>) : null;
  $: costServed = num(cost, 'served');
  $: costSaved = num(cost, 'saved');
  $: costActual = num(cost, 'actual');
  $: costCalls = num(cost, 'llm_calls');
  $: costCacheHits = num(cost, 'cache_hits');
  $: isExploration = !!cost && costSaved < 0.000001 && costActual > costServed + 0.000001;

  let showTranscript = false;
  let showRaw = false;
</script>

{#if typeof value === 'string'}
  <!-- Plain string output (e.g. a delivered deliverable / formatted text). -->
  <div class="prose prose-sm max-w-none bg-sand-50 rounded p-2 max-h-72 overflow-y-auto">{@html md(stringValue)}</div>

{:else if isTestGate && obj}
  <!-- Test Gate result. -->
  <div class="bg-sand-50 rounded p-2 space-y-1">
    {#if gatePassed}
      <div class="text-sm font-semibold text-green-700">✓ Tests passed</div>
    {:else}
      <div class="text-sm font-semibold text-red-700">✗ Tests failed</div>
    {/if}
    <div class="text-xs text-sand-500">
      check: <span class="font-mono">{obj.check}</span> · attempt {obj.attempts}
    </div>
  </div>

{:else if hasDiff && obj}
  <!-- AgentImplement-style output: summary (md) + diff + files + transcript. -->
  <div class="space-y-3">
    {#if status}
      <span class="text-[10px] uppercase tracking-wide px-1.5 py-0.5 rounded {status === 'completed' ? 'bg-green-100 text-green-700' : 'bg-amber-100 text-amber-700'}">{status}</span>
    {/if}

    {#if cost}
      <!-- Optimizer cost/savings for this run (routed through the edge). -->
      <div class="rounded-md border border-sand-200 bg-sand-50 px-3 py-2">
        <div class="flex items-baseline gap-3 flex-wrap">
          <span class="text-[11px] font-semibold text-sand-500 uppercase tracking-wide">Optimizer</span>
          <span class="text-sm text-sand-800">cost <span class="font-semibold">{usd(costServed)}</span></span>
          {#if costSaved > 0.000001}
            <span class="text-sm text-green-700">saved <span class="font-semibold">{usd(costSaved)}</span> vs baseline</span>
          {:else if isExploration}
            <span class="text-xs text-sand-500">learning this task type — savings begin once it's proven safe to downshift</span>
          {/if}
          <span class="text-[11px] text-sand-400 ml-auto">
            {costCalls} call{costCalls === 1 ? '' : 's'}{#if costCacheHits > 0} · {costCacheHits} cached{/if}
          </span>
        </div>
      </div>
    {/if}

    {#if summaryText}
      <div>
        <div class="text-[11px] font-semibold text-sand-500 uppercase tracking-wide mb-1">Summary</div>
        <div class="prose prose-sm max-w-none">{@html md(summaryText)}</div>
      </div>
    {/if}

    {#if filesChanged.length}
      <div>
        <div class="text-[11px] font-semibold text-sand-500 uppercase tracking-wide mb-1">Files changed ({filesChanged.length})</div>
        <ul class="text-xs font-mono text-sand-600 space-y-0.5">
          {#each filesChanged as f}<li>{f}</li>{/each}
        </ul>
      </div>
    {/if}

    <div>
      <div class="text-[11px] font-semibold text-sand-500 uppercase tracking-wide mb-1">Diff</div>
      <DiffView diff={diffText} />
    </div>

    {#if testSummary}
      <div class="text-xs text-sand-600"><span class="font-semibold">Tests:</span> {testSummary}</div>
    {/if}

    {#if transcript}
      <div>
        <button class="text-xs text-brand-600 hover:underline" on:click={() => showTranscript = !showTranscript}>
          {showTranscript ? 'Hide' : 'Show'} agent transcript
        </button>
        {#if showTranscript}
          <pre class="mt-1 bg-sand-50 rounded p-2 text-[11px] whitespace-pre-wrap break-words max-h-72 overflow-y-auto">{transcript}</pre>
        {/if}
      </div>
    {/if}
  </div>

{:else if resourceKind && obj}
  <!-- A side-effecting write node: show status + a link to the real artifact. -->
  <div class="bg-sand-50 rounded p-2 space-y-1">
    <div class="text-sm text-sand-700">
      <span class="font-semibold capitalize">{status || 'done'}</span>
      <span class="text-sand-400">· {resourceKind.replace(/_/g, ' ')}</span>
    </div>
    {#if artifactUrl}
      <a class="text-sm text-brand-600 hover:underline break-all" href={artifactUrl} target="_blank" rel="noopener">{artifactUrl}</a>
    {/if}
    {#if resourceId}
      <div class="text-[11px] text-sand-400 font-mono break-all">{resourceId}</div>
    {/if}
  </div>

{:else}
  <!-- Fallback: pretty JSON, collapsible. -->
  <button class="text-xs text-brand-600 hover:underline mb-1" on:click={() => showRaw = !showRaw}>
    {showRaw ? 'Hide' : 'Show'} raw
  </button>
  {#if showRaw || (typeof value !== 'object')}
    <pre class="bg-sand-50 rounded p-2 text-xs whitespace-pre-wrap break-words max-h-72 overflow-y-auto">{pretty(value)}</pre>
  {:else}
    <pre class="bg-sand-50 rounded p-2 text-xs whitespace-pre-wrap break-words max-h-48 overflow-y-auto">{pretty(value)}</pre>
  {/if}
{/if}
