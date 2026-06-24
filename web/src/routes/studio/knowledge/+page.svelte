<script lang="ts">
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { marked } from 'marked';
  import { api } from '$lib/api/client';
  import { session } from '$lib/stores/session';
  import VaultGraphView from '$lib/components/VaultGraph.svelte';

  import type { NoteView, NoteDetail, VaultGraph } from '$lib/api/types';

  let notes: NoteView[] = [];
  let loading = true;
  let error = '';
  let agentId = '';

  // List ⇄ Graph — one at a time. List is the default lens; graph is the Obsidian-style view.
  let view: 'list' | 'graph' = 'list';

  // Filter the list by note layer (the natural axis for a code-ingested vault) — 'all' or a layer.
  let filter = 'all';

  // Resume editor
  let resumeText = '';
  let savingResume = false;
  let resumeSaved = false;

  // Timeline
  interface TimelineEvent { id: string; chunk_id: string; event_type: string; source_type: string; summary: string | null; created_at: string }
  let timeline: TimelineEvent[] = [];
  let showTimeline = false;

  // Graph (lazy-loaded when first switching to the graph view).
  let graph: VaultGraph | null = null;
  let graphLoading = false;
  async function loadGraph() {
    if (!agentId || graph) return;
    graphLoading = true;
    try { graph = await api.get<VaultGraph>(`/knowledge/agent/${agentId}/graph`); }
    catch { graph = { nodes: [], edges: [] }; }
    finally { graphLoading = false; }
  }
  function setView(v: 'list' | 'graph') {
    view = v;
    if (v === 'graph') loadGraph();
  }

  // The opened note (full markdown), shown in a modal. Lazily fetched on row click.
  let openNote: NoteDetail | null = null;
  let openLoading = false;
  async function openNoteBySlug(slug: string) {
    openLoading = true;
    openNote = null;
    try { openNote = await api.get<NoteDetail>(`/knowledge/agent/${agentId}/note/${encodeURIComponent(slug)}`); }
    catch (e: unknown) { error = e instanceof Error ? e.message : 'Failed to load note'; }
    finally { openLoading = false; }
  }
  function md(s: string): string {
    try { return marked.parse(s, { async: false }) as string; } catch { return s; }
  }

  async function loadNotes(aid: string) {
    notes = await api.get<NoteView[]>(`/knowledge/agent/${aid}/notes`);
    // Seed the resume editor from the resume note if present.
    if (!resumeText) {
      const resume = notes.find(n => n.slug === 'resume' || n.source_type === 'resume');
      if (resume) {
        try {
          const full = await api.get<NoteDetail>(`/knowledge/agent/${aid}/note/${encodeURIComponent(resume.slug)}`);
          resumeText = full.body;
        } catch { /* non-blocking */ }
      }
    }
  }

  async function loadTimeline() {
    if (!agentId) return;
    try { timeline = await api.get<TimelineEvent[]>(`/knowledge/agent/${agentId}/timeline`); }
    catch { timeline = []; }
  }

  async function deleteNote(note: NoteView) {
    if (!confirm(`Delete "${note.title}" from your knowledge base? This cannot be undone.`)) return;
    const prev = notes;
    notes = notes.filter(n => n.slug !== note.slug); // optimistic
    if (openNote?.slug === note.slug) openNote = null;
    try {
      await api.delete(`/knowledge/agent/${agentId}/note/${encodeURIComponent(note.slug)}`);
      if (showTimeline) loadTimeline();
    } catch {
      notes = prev; // revert
    }
  }

  // Visibility toggle uses the chunk endpoint by note id (note id == chunk id, 1:1).
  async function toggleShare(note: NoteView) {
    const shareable = note.visibility !== 'shareable';
    note.visibility = shareable ? 'shareable' : 'private';
    notes = [...notes];
    try {
      await api.put(`/knowledge/chunk/${note.id}/visibility`, { shareable });
    } catch {
      note.visibility = shareable ? 'private' : 'shareable'; // revert
      notes = [...notes];
    }
  }

  async function saveResume() {
    if (!resumeText.trim() || !agentId) return;
    savingResume = true;
    try {
      await api.post(`/knowledge/agent/${agentId}/resume`, { resume_text: resumeText.trim() });
      await loadNotes(agentId);
      resumeSaved = true;
      setTimeout(() => resumeSaved = false, 2000);
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to save resume';
    } finally {
      savingResume = false;
    }
  }

  function timeAgo(iso: string) {
    const diff = Date.now() - new Date(iso).getTime();
    const m = Math.floor(diff / 60000);
    if (m < 60) return `${m}m ago`;
    const h = Math.floor(m / 60);
    if (h < 24) return `${h}h ago`;
    return `${Math.floor(h / 24)}d ago`;
  }

  onMount(async () => {
    try {
      let aid = $session.agentId;
      if (!aid) {
        const agent = await api.get<{ id: string }>('/agents/mine');
        session.setIds({ agentId: agent.id });
        aid = agent.id;
      }
      agentId = aid;
      await loadNotes(aid);
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to load knowledge vault';
    } finally {
      loading = false;
    }
  });

  // Layer-based filtering + labels/colors. Layers are the typed note kind (feature/pattern/etc.).
  const LAYER_LABELS: Record<string, string> = {
    feature: 'Feature',
    pattern: 'Pattern',
    tech: 'Tech',
    golden_snippet: 'Golden snippet',
    architecture: 'Architecture',
    style_rule: 'Style rule',
    note: 'Note',
  };
  const LAYER_COLORS: Record<string, string> = {
    feature: 'badge-blue',
    pattern: 'badge-purple',
    tech: 'badge-green',
    golden_snippet: 'badge-yellow',
    architecture: 'badge-gray',
    style_rule: 'badge-purple',
    note: 'badge-gray',
  };
  const layerLabel = (l: string) => LAYER_LABELS[l] ?? l.replace(/_/g, ' ');

  $: filtered = filter === 'all' ? notes : notes.filter(n => n.layer === filter);

  // Layers present in the vault, with counts — drives the filter tabs + breakdown.
  $: layerCounts = (() => {
    const m: Record<string, number> = {};
    for (const n of notes) m[n.layer] = (m[n.layer] ?? 0) + 1;
    return m;
  })();
  $: layerOrder = ['feature', 'pattern', 'tech', 'golden_snippet', 'architecture', 'style_rule', 'note']
    .filter(l => layerCounts[l]);
</script>

<svelte:head><title>Knowledge Vault — Flaborful Studio</title></svelte:head>

<div class="max-w-5xl mx-auto px-4 py-8">
  <button class="btn-ghost text-sm mb-6" on:click={() => goto('/studio')}>← Studio</button>

  <div class="flex items-start justify-between mb-6">
    <div>
      <h1 class="text-2xl font-bold">Knowledge Vault</h1>
      <p class="text-gray-500 text-sm mt-0.5">
        Everything your agent has learned, as linked markdown notes. Mark items <strong>shareable</strong> to attach them to job applications — everything else stays private and is used only for training.
      </p>
    </div>
    {#if !loading}
      <div class="flex items-center gap-3 shrink-0">
        <span class="text-sm text-gray-400">{notes.length} notes</span>
        <!-- List ⇄ Graph segmented toggle -->
        <div class="flex bg-gray-100 rounded-lg p-0.5">
          <button class="px-3 py-1 rounded-md text-sm font-medium transition-colors {view === 'list' ? 'bg-white shadow text-gray-900' : 'text-gray-500'}"
            on:click={() => setView('list')}>List</button>
          <button class="px-3 py-1 rounded-md text-sm font-medium transition-colors {view === 'graph' ? 'bg-white shadow text-gray-900' : 'text-gray-500'}"
            on:click={() => setView('graph')}>Graph</button>
        </div>
        <button class="btn-ghost text-sm"
          on:click={() => { showTimeline = !showTimeline; if (showTimeline) loadTimeline(); }}>
          {showTimeline ? 'Hide timeline' : 'Timeline'}
        </button>
      </div>
    {/if}
  </div>

  {#if showTimeline}
    <div class="card p-5 mb-6">
      <h2 class="font-semibold text-sm mb-3">Knowledge timeline</h2>
      {#if timeline.length === 0}
        <p class="text-sm text-gray-400">No changes recorded yet.</p>
      {:else}
        <ul class="space-y-2">
          {#each timeline as ev (ev.id)}
            <li class="flex items-start gap-3 text-sm">
              <span class="shrink-0 mt-0.5 w-2 h-2 rounded-full {ev.event_type === 'added' ? 'bg-green-400' : 'bg-red-400'}"></span>
              <div class="flex-1 min-w-0">
                <span class="font-medium capitalize">{ev.event_type}</span>
                <span class="text-gray-400">·</span>
                <span class="text-gray-600">{ev.source_type.replace(/_/g, ' ')}</span>
                {#if ev.summary}<div class="text-xs text-gray-500 truncate">{ev.summary}</div>{/if}
              </div>
              <span class="text-xs text-gray-400 shrink-0">{timeAgo(ev.created_at)}</span>
            </li>
          {/each}
        </ul>
      {/if}
    </div>
  {/if}

  <!-- Resume editor -->
  {#if !loading}
    <div class="card p-5 mb-6">
      <div class="flex items-center justify-between mb-2">
        <h2 class="font-semibold text-sm">Your resume</h2>
        <span class="badge-yellow text-xs">shareable by default</span>
      </div>
      <p class="text-xs text-gray-400 mb-3">Used as the default resume on job applications and added to your agent's training data.</p>
      <textarea class="input text-sm min-h-[120px] resize-y" bind:value={resumeText}
        placeholder="Paste your resume here..."></textarea>
      <div class="flex justify-end mt-2">
        <button class="btn-primary text-sm" disabled={!resumeText.trim() || savingResume} on:click={saveResume}>
          {savingResume ? 'Saving...' : resumeSaved ? '✓ Saved' : 'Save resume'}
        </button>
      </div>
    </div>
  {/if}

  {#if loading}
    <div class="space-y-3">
      {#each Array(6) as _}
        <div class="card p-4 animate-pulse"><div class="h-10 bg-gray-100 rounded"></div></div>
      {/each}
    </div>

  {:else if error}
    <div class="card p-8 text-center text-red-600">{error}</div>

  {:else if notes.length === 0}
    <div class="card p-12 text-center">
      <div class="text-4xl mb-4">📚</div>
      <h2 class="font-semibold text-gray-700 mb-2">No knowledge yet</h2>
      <p class="text-gray-400 text-sm mb-6">
        Complete onboarding to ingest your GitHub portfolio and build your knowledge base.
      </p>
      <button class="btn-primary" on:click={() => goto('/studio/onboarding')}>
        Start onboarding
      </button>
    </div>

  {:else if view === 'graph'}
    {#if graphLoading}
      <div class="card p-12 text-center text-sm text-gray-400 animate-pulse">Loading graph…</div>
    {:else if graph && graph.nodes.length > 0}
      <VaultGraphView {graph} height={760} />
      <p class="text-xs text-gray-400 mt-2">
        Your full knowledge vault — colored by layer, linked by <code>related</code> notes. Click a node to read it.
        This is the same graph companies see on your applications (they only see notes you've shared).
      </p>
    {:else}
      <div class="card p-8 text-center text-sm text-gray-400">No linked notes to graph yet.</div>
    {/if}

  {:else}
    <!-- LIST VIEW -->
    <!-- Layer filter tabs -->
    <div class="flex gap-1 mb-5 bg-gray-100 rounded-lg p-1 w-fit flex-wrap">
      <button class="px-3 py-1.5 rounded-md text-sm font-medium transition-colors {filter === 'all' ? 'bg-white shadow text-gray-900' : 'text-gray-500 hover:text-gray-700'}"
        on:click={() => (filter = 'all')}>All ({notes.length})</button>
      {#each layerOrder as l}
        <button class="px-3 py-1.5 rounded-md text-sm font-medium transition-colors {filter === l ? 'bg-white shadow text-gray-900' : 'text-gray-500 hover:text-gray-700'}"
          on:click={() => (filter = l)}>{layerLabel(l)} ({layerCounts[l]})</button>
      {/each}
    </div>

    {#if filtered.length === 0}
      <div class="card p-8 text-center text-sm text-gray-400">No {layerLabel(filter).toLowerCase()} notes.</div>
    {:else}
      <ul class="divide-y divide-gray-100 border border-gray-100 rounded-lg overflow-hidden">
        {#each filtered as note (note.slug)}
          <li class="flex items-center gap-3 px-4 py-3 hover:bg-gray-50">
            <button class="flex-1 min-w-0 text-left" on:click={() => openNoteBySlug(note.slug)}>
              <div class="flex items-center gap-2">
                <span class="text-sm font-medium text-gray-800 truncate">{note.title}</span>
              </div>
              <div class="flex items-center gap-1.5 mt-1 flex-wrap">
                <span class="{LAYER_COLORS[note.layer] ?? 'badge-gray'} text-[10px]">{layerLabel(note.layer)}</span>
                {#if note.domain}<span class="badge-gray text-[10px]">{note.domain}</span>{/if}
                {#if note.repo}<span class="text-[10px] text-gray-400 font-mono truncate">{note.repo}</span>{/if}
                {#if note.related.length}<span class="text-[10px] text-gray-400">· {note.related.length} link{note.related.length === 1 ? '' : 's'}</span>{/if}
              </div>
            </button>
            <div class="shrink-0 flex items-center gap-1.5">
              <button
                class="flex items-center gap-1 text-xs px-2 py-1 rounded-md transition-colors {note.visibility === 'shareable' ? 'bg-green-50 text-green-700 hover:bg-green-100' : 'bg-gray-100 text-gray-500 hover:bg-gray-200'}"
                title={note.visibility === 'shareable' ? 'Shareable with companies — click to make private' : 'Private — click to allow sharing on applications'}
                on:click|stopPropagation={() => toggleShare(note)}>
                {#if note.visibility === 'shareable'}🔓{:else}🔒{/if}
              </button>
              <button class="text-xs text-gray-400 hover:text-red-600 px-1.5 py-1" title="Delete"
                on:click|stopPropagation={() => deleteNote(note)}>✕</button>
            </div>
          </li>
        {/each}
      </ul>
    {/if}

    <!-- Breakdown by layer -->
    {#if layerOrder.length}
      <div class="mt-8 card p-5">
        <h2 class="font-semibold text-sm text-gray-700 mb-4">Breakdown</h2>
        <div class="grid grid-cols-3 sm:grid-cols-4 gap-4">
          {#each layerOrder as l}
            <div class="text-center">
              <div class="text-2xl font-bold text-brand-600">{layerCounts[l]}</div>
              <div class="text-xs text-gray-500 mt-1">{layerLabel(l)}</div>
            </div>
          {/each}
        </div>
      </div>
    {/if}
  {/if}
</div>

<!-- Full-note modal (no truncation) -->
{#if openLoading || openNote}
  <div class="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4" on:click={() => (openNote = null)} on:keydown={(e) => e.key === 'Escape' && (openNote = null)} role="button" tabindex="-1">
    <div class="bg-white rounded-xl max-w-3xl w-full max-h-[85vh] overflow-hidden flex flex-col" on:click|stopPropagation on:keydown|stopPropagation role="document">
      {#if openLoading}
        <div class="p-12 text-center text-sm text-gray-400 animate-pulse">Loading note…</div>
      {:else if openNote}
        <div class="flex items-start justify-between gap-3 px-5 py-4 border-b border-gray-100">
          <div class="min-w-0">
            <h2 class="text-lg font-bold text-gray-900">{openNote.title}</h2>
            <div class="flex items-center gap-1.5 mt-1 flex-wrap">
              <span class="{LAYER_COLORS[openNote.layer] ?? 'badge-gray'} text-[10px]">{layerLabel(openNote.layer)}</span>
              {#if openNote.domain}<span class="badge-gray text-[10px]">{openNote.domain}</span>{/if}
              {#if openNote.repo}<span class="text-[10px] text-gray-400 font-mono">{openNote.repo}</span>{/if}
              <span class="text-[10px] text-gray-400">{openNote.visibility}</span>
            </div>
          </div>
          <button class="text-gray-400 hover:text-gray-700 text-xl leading-none" on:click={() => (openNote = null)}>✕</button>
        </div>
        <div class="overflow-y-auto px-5 py-4 prose prose-sm max-w-none">
          {@html md(openNote.body)}
        </div>
        {#if openNote.related.length}
          <div class="px-5 py-3 border-t border-gray-100 text-xs text-gray-500">
            <span class="font-semibold">Linked:</span>
            {#each openNote.related as r}
              <button class="text-brand-600 hover:underline mr-2" on:click={() => openNoteBySlug(r)}>{r}</button>
            {/each}
          </div>
        {/if}
      {/if}
    </div>
  </div>
{/if}
