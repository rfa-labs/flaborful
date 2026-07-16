<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { goto } from '$app/navigation';
  import { marked } from 'marked';
  import { api } from '$lib/api/client';
  import { session } from '$lib/stores/session';
  import VaultGraphView from '$lib/components/VaultGraph.svelte';
  import type { NoteView, NoteDetail, VaultGraph } from '$lib/api/types';

  let companyId = '';
  let notes: NoteView[] = [];
  let loading = true;
  let error = '';

  // List ⇄ Graph, same lens model as the professional vault.
  let view: 'list' | 'graph' = 'list';
  let filter = 'all';

  // Repo ingestion (per-repo, one at a time).
  interface RepoOption { full_name: string; default_branch: string; documented: boolean }
  let repos: RepoOption[] = [];
  let reposLoading = false;
  let reposError = '';

  interface RepoStatus { repo: string; status: string; detail: string; notes_count: number }
  interface IngestionStatus { complete: boolean; total_notes: number; repos: RepoStatus[] }
  let ingestion: IngestionStatus | null = null;
  let poll: ReturnType<typeof setInterval> | null = null;
  let ingesting = false;

  // Graph (lazy).
  let graph: VaultGraph | null = null;
  let graphLoading = false;
  async function loadGraph() {
    if (!companyId || graph) return;
    graphLoading = true;
    try { graph = await api.get<VaultGraph>(`/companies/${companyId}/vault/graph`); }
    catch { graph = { nodes: [], edges: [] }; }
    finally { graphLoading = false; }
  }
  function setView(v: 'list' | 'graph') {
    view = v;
    if (v === 'graph') { graph = null; loadGraph(); }
  }

  // Opened note modal.
  let openNote: NoteDetail | null = null;
  let openLoading = false;
  async function openNoteBySlug(slug: string) {
    openLoading = true; openNote = null;
    try { openNote = await api.get<NoteDetail>(`/companies/${companyId}/vault/note/${encodeURIComponent(slug)}`); }
    catch (e: unknown) { error = e instanceof Error ? e.message : 'Failed to load note'; }
    finally { openLoading = false; }
  }
  const md = (s: string) => { try { return marked.parse(s, { async: false }) as string; } catch { return s; } };

  async function loadNotes() {
    notes = await api.get<NoteView[]>(`/companies/${companyId}/vault/notes`);
  }

  async function loadRepos() {
    reposLoading = true; reposError = '';
    try { repos = await api.get<RepoOption[]>(`/companies/${companyId}/vault/repos`); }
    catch (e: unknown) { reposError = e instanceof Error ? e.message : 'Connect GitHub to list repos'; }
    finally { reposLoading = false; }
  }

  async function ingestRepo(repo: RepoOption) {
    ingesting = true;
    try {
      await api.post(`/companies/${companyId}/vault/ingest`, { repo: repo.full_name, branch: repo.default_branch });
      startPolling();
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to start ingestion';
      ingesting = false;
    }
  }

  function startPolling() {
    if (poll) return;
    poll = setInterval(refreshStatus, 2500);
    refreshStatus();
  }
  async function refreshStatus() {
    if (!companyId) return;
    try {
      ingestion = await api.get<IngestionStatus>(`/companies/${companyId}/vault/ingestion/status`);
      if (ingestion.complete) {
        ingesting = false;
        if (poll) { clearInterval(poll); poll = null; }
        await loadNotes();
        await loadRepos();
        graph = null; // invalidate so the graph re-fetches with new notes
      }
    } catch { /* keep polling */ }
  }

  async function cancelIngestion() {
    try { await api.post(`/companies/${companyId}/vault/ingestion/cancel`, {}); } catch { /* best-effort */ }
  }

  onMount(async () => {
    try {
      let cid = $session.companyId;
      if (!cid) {
        const company = await api.get<{ id: string }>('/companies/mine');
        session.setIds({ companyId: company.id });
        cid = company.id;
      }
      companyId = cid;
      await loadNotes();
      await loadRepos();
      // If a run is mid-flight (page reload), resume polling.
      await refreshStatus();
      if (ingestion && !ingestion.complete && ingestion.repos.some(r => r.status === 'running')) {
        ingesting = true; startPolling();
      }
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to load company vault';
    } finally {
      loading = false;
    }
  });
  onDestroy(() => { if (poll) clearInterval(poll); });

  const LAYER_LABELS: Record<string, string> = {
    feature: 'Feature', pattern: 'Pattern', tech: 'Tech', golden_snippet: 'Golden snippet',
    architecture: 'Architecture', style_rule: 'Style rule', note: 'Note',
  };
  const LAYER_COLORS: Record<string, string> = {
    feature: 'badge-blue', pattern: 'badge-purple', tech: 'badge-green',
    golden_snippet: 'badge-yellow', architecture: 'badge-gray', style_rule: 'badge-purple', note: 'badge-gray',
  };
  const layerLabel = (l: string) => LAYER_LABELS[l] ?? l.replace(/_/g, ' ');

  $: filtered = filter === 'all' ? notes : notes.filter(n => n.layer === filter);
  $: layerCounts = (() => { const m: Record<string, number> = {}; for (const n of notes) m[n.layer] = (m[n.layer] ?? 0) + 1; return m; })();
  $: layerOrder = ['feature', 'pattern', 'tech', 'golden_snippet', 'architecture', 'style_rule', 'note'].filter(l => layerCounts[l]);
  const dot = (s: string) => s === 'done' ? 'bg-green-500' : s === 'failed' ? 'bg-red-500' : s === 'running' ? 'bg-amber-400 animate-pulse' : 'bg-sand-300';
</script>

<svelte:head><title>Company Knowledge Vault — Flaborful</title></svelte:head>

<div class="max-w-5xl mx-auto px-4 py-8">
  <button class="btn-ghost text-sm mb-6" on:click={() => goto('/hire')}>← Dashboard</button>

  <div class="flex items-start justify-between mb-6">
    <div>
      <h1 class="text-2xl font-bold">Company Knowledge Vault</h1>
      <p class="text-sand-500 text-sm mt-0.5">
        A linked knowledge graph of your codebases. Document a repo here, and when you hire a professional for a task on that repo, their agent gets this context automatically — and writes back what it changes.
      </p>
    </div>
    {#if !loading}
      <div class="flex items-center gap-3 shrink-0">
        <span class="text-sm text-sand-400">{notes.length} notes</span>
        <div class="flex bg-sand-100 rounded-lg p-0.5">
          <button class="px-3 py-1 rounded-md text-sm font-medium transition-colors {view === 'list' ? 'bg-white shadow text-sand-900' : 'text-sand-500'}" on:click={() => setView('list')}>List</button>
          <button class="px-3 py-1 rounded-md text-sm font-medium transition-colors {view === 'graph' ? 'bg-white shadow text-sand-900' : 'text-sand-500'}" on:click={() => setView('graph')}>Graph</button>
        </div>
      </div>
    {/if}
  </div>

  <!-- Repo ingestion -->
  {#if !loading}
    <div class="card p-5 mb-6">
      <div class="flex items-center justify-between mb-3">
        <h2 class="font-semibold text-sm">Document a repository</h2>
        <button class="btn-ghost text-xs" on:click={loadRepos} disabled={reposLoading}>{reposLoading ? 'Loading…' : 'Refresh repos'}</button>
      </div>
      {#if reposError}
        <p class="text-sm text-sand-400">{reposError}</p>
      {:else if reposLoading}
        <p class="text-sm text-sand-400 animate-pulse">Loading your GitHub repos…</p>
      {:else if repos.length === 0}
        <p class="text-sm text-sand-400">No repos found.</p>
      {:else}
        <ul class="divide-y divide-sand-100 border border-sand-100 rounded-lg overflow-hidden">
          {#each repos as r (r.full_name)}
            <li class="flex items-center gap-3 px-4 py-2.5">
              <div class="flex-1 min-w-0">
                <span class="text-sm font-mono text-sand-800 truncate">{r.full_name}</span>
                <span class="text-[10px] text-sand-400 ml-2">{r.default_branch}</span>
              </div>
              {#if r.documented}
                <span class="badge-green text-[10px]">documented</span>
              {/if}
              <button class="btn-secondary text-xs" disabled={ingesting} on:click={() => ingestRepo(r)}>
                {r.documented ? 'Re-document' : 'Document'}
              </button>
            </li>
          {/each}
        </ul>
      {/if}

      {#if ingestion && ingestion.repos.length > 0 && !ingestion.complete}
        <div class="mt-4 border-t border-sand-100 pt-3">
          <div class="flex items-center justify-between mb-2">
            <span class="text-xs font-semibold text-sand-600">Ingestion in progress</span>
            <button class="text-xs text-sand-400 hover:text-red-600" on:click={cancelIngestion}>Cancel</button>
          </div>
          <ul class="space-y-1.5">
            {#each ingestion.repos as rs (rs.repo)}
              <li class="flex items-center gap-2 text-xs">
                <span class="w-2 h-2 rounded-full {dot(rs.status)}"></span>
                <span class="font-mono truncate flex-1">{rs.repo}</span>
                <span class="text-sand-400">{rs.notes_count} notes</span>
              </li>
            {/each}
          </ul>
        </div>
      {/if}
    </div>
  {/if}

  {#if loading}
    <div class="space-y-3">{#each Array(5) as _}<div class="card p-4 animate-pulse"><div class="h-10 bg-sand-100 rounded"></div></div>{/each}</div>
  {:else if error}
    <div class="card p-8 text-center text-red-600">{error}</div>
  {:else if notes.length === 0}
    <div class="card p-12 text-center">
      <div class="text-4xl mb-4">🏛️</div>
      <h2 class="font-semibold text-sand-700 mb-2">No knowledge yet</h2>
      <p class="text-sand-400 text-sm">Document a repository above to build your company knowledge graph.</p>
    </div>
  {:else if view === 'graph'}
    {#if graphLoading}
      <div class="card p-12 text-center text-sm text-sand-400 animate-pulse">Loading graph…</div>
    {:else if graph && graph.nodes.length > 0}
      <VaultGraphView {graph} height={760} />
      <p class="text-xs text-sand-400 mt-2">Your company knowledge vault — colored by layer, linked by <code>related</code> notes. Click a node to read it.</p>
    {:else}
      <div class="card p-8 text-center text-sm text-sand-400">No linked notes to graph yet.</div>
    {/if}
  {:else}
    <div class="flex gap-1 mb-5 bg-sand-100 rounded-lg p-1 w-fit flex-wrap">
      <button class="px-3 py-1.5 rounded-md text-sm font-medium transition-colors {filter === 'all' ? 'bg-white shadow text-sand-900' : 'text-sand-500 hover:text-sand-700'}" on:click={() => (filter = 'all')}>All ({notes.length})</button>
      {#each layerOrder as l}
        <button class="px-3 py-1.5 rounded-md text-sm font-medium transition-colors {filter === l ? 'bg-white shadow text-sand-900' : 'text-sand-500 hover:text-sand-700'}" on:click={() => (filter = l)}>{layerLabel(l)} ({layerCounts[l]})</button>
      {/each}
    </div>
    {#if filtered.length === 0}
      <div class="card p-8 text-center text-sm text-sand-400">No {layerLabel(filter).toLowerCase()} notes.</div>
    {:else}
      <ul class="divide-y divide-sand-100 border border-sand-100 rounded-lg overflow-hidden">
        {#each filtered as note (note.slug)}
          <li class="flex items-center gap-3 px-4 py-3 hover:bg-sand-50">
            <button class="flex-1 min-w-0 text-left" on:click={() => openNoteBySlug(note.slug)}>
              <span class="text-sm font-medium text-sand-800 truncate">{note.title}</span>
              <div class="flex items-center gap-1.5 mt-1 flex-wrap">
                <span class="{LAYER_COLORS[note.layer] ?? 'badge-gray'} text-[10px]">{layerLabel(note.layer)}</span>
                {#if note.domain}<span class="badge-gray text-[10px]">{note.domain}</span>{/if}
                {#if note.repo}<span class="text-[10px] text-sand-400 font-mono truncate">{note.repo}</span>{/if}
                {#if note.related.length}<span class="text-[10px] text-sand-400">· {note.related.length} link{note.related.length === 1 ? '' : 's'}</span>{/if}
              </div>
            </button>
          </li>
        {/each}
      </ul>
    {/if}
  {/if}
</div>

{#if openLoading || openNote}
  <div class="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4" on:click={() => (openNote = null)} on:keydown={(e) => e.key === 'Escape' && (openNote = null)} role="button" tabindex="-1">
    <div class="bg-white rounded-xl max-w-3xl w-full max-h-[85vh] overflow-hidden flex flex-col" on:click|stopPropagation on:keydown|stopPropagation role="document">
      {#if openLoading}
        <div class="p-12 text-center text-sm text-sand-400 animate-pulse">Loading note…</div>
      {:else if openNote}
        <div class="flex items-start justify-between gap-3 px-5 py-4 border-b border-sand-100">
          <div class="min-w-0">
            <h2 class="text-lg font-bold text-sand-900">{openNote.title}</h2>
            <div class="flex items-center gap-1.5 mt-1 flex-wrap">
              <span class="{LAYER_COLORS[openNote.layer] ?? 'badge-gray'} text-[10px]">{layerLabel(openNote.layer)}</span>
              {#if openNote.domain}<span class="badge-gray text-[10px]">{openNote.domain}</span>{/if}
              {#if openNote.repo}<span class="text-[10px] text-sand-400 font-mono">{openNote.repo}</span>{/if}
            </div>
          </div>
          <button class="text-sand-400 hover:text-sand-700 text-xl leading-none" on:click={() => (openNote = null)}>✕</button>
        </div>
        <div class="overflow-y-auto px-5 py-4 prose prose-sm max-w-none">{@html md(openNote.body)}</div>
        {#if openNote.related.length}
          <div class="px-5 py-3 border-t border-sand-100 text-xs text-sand-500">
            <span class="font-semibold">Linked:</span>
            {#each openNote.related as r}<button class="text-brand-600 hover:underline mr-2" on:click={() => openNoteBySlug(r)}>{r}</button>{/each}
          </div>
        {/if}
      {/if}
    </div>
  </div>
{/if}
