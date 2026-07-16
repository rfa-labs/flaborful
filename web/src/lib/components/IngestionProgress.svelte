<script lang="ts">
  // Live feed for the agentic code-ingestion run: a coding agent clones each repo, reads the
  // source, and writes vault notes. We open the SSE feed (GET /events/ingestion/:agentId) and
  // render per-repo status + a scrolling transcript, plus seed from the snapshot endpoint on
  // mount so reconnects/late-joins show current state immediately.
  import { onMount, onDestroy, createEventDispatcher } from 'svelte';
  import { getToken } from '$lib/stores/session';
  import { api } from '$lib/api/client';

  export let agentId: string;

  const dispatch = createEventDispatcher<{ complete: { totalNotes: number } }>();

  interface RepoStatus { repo: string; status: string; notes_count: number; detail: string }
  interface Ev { repo: string; kind: string; status: string | null; detail: string; notes_count: number }

  let repos: Record<string, RepoStatus> = {};
  let log: { repo: string; detail: string; kind: string }[] = [];
  let complete = false;
  let totalNotes = 0;
  let feed: EventSource | null = null;
  let logEl: HTMLDivElement;

  $: repoList = Object.values(repos);
  $: runningRepo = repoList.find((r) => r.status === 'running')?.repo ?? null;

  const dot = (s: string) =>
    s === 'done' ? 'bg-green-500' : s === 'failed' ? 'bg-red-500' : s === 'running' ? 'bg-amber-400 animate-pulse' : 'bg-sand-300';

  function apply(ev: Ev) {
    // Overall-complete sentinel (repo='').
    if (ev.kind === 'status' && ev.status === 'complete') {
      complete = true;
      dispatch('complete', { totalNotes });
      feed?.close();
      return;
    }
    if (ev.kind === 'status' && ev.repo) {
      repos[ev.repo] = {
        repo: ev.repo,
        status: ev.status ?? 'running',
        notes_count: ev.notes_count,
        detail: ev.detail,
      };
      repos = repos;
    }
    if (ev.detail) {
      log = [...log.slice(-200), { repo: ev.repo, detail: ev.detail, kind: ev.kind }];
      queueMicrotask(() => logEl?.scrollTo({ top: logEl.scrollHeight }));
    }
  }

  onMount(async () => {
    // Seed from the snapshot so a returning/late view isn't blank.
    try {
      const snap = await api.get<{ complete: boolean; total_notes: number; repos: RepoStatus[] }>(
        `/onboarding/ingestion/status/${agentId}`,
      );
      for (const r of snap.repos) repos[r.repo] = r;
      repos = repos;
      complete = snap.complete;
      totalNotes = snap.total_notes;
      if (complete) dispatch('complete', { totalNotes });
    } catch { /* no events yet — the live feed will fill in */ }

    // Always open the live feed, even if the snapshot said complete. A NEW run clears the prior
    // events server-side, so its first event (a `running` status) resets this view — without this,
    // a stale `complete` from a previous run would freeze the panel and block re-runs.
    const token = getToken();
    feed = new EventSource(`/api/v1/events/ingestion/${agentId}${token ? `?token=${token}` : ''}`);
    feed.addEventListener('ingestion', (e) => {
      try {
        const ev = JSON.parse((e as MessageEvent).data) as Ev;
        // A fresh run's first `running` event supersedes a stale completion.
        if (ev.kind === 'status' && ev.status === 'running' && complete) {
          complete = false;
          repos = {};
          log = [];
          totalNotes = 0;
        }
        if (ev.kind === 'note') totalNotes = Math.max(totalNotes, ev.notes_count);
        apply(ev);
      } catch { /* ignore malformed frame */ }
    });
  });

  let stopping = false;
  async function stop() {
    stopping = true;
    try {
      await api.post(`/onboarding/ingestion/cancel/${agentId}`, {});
    } catch { /* best-effort; the loop still stops via the flag */ }
  }

  onDestroy(() => feed?.close());
</script>

<div class="space-y-3">
  <div class="flex items-center justify-between">
    <h3 class="text-sm font-semibold text-sand-700">
      {#if complete}Knowledge vault built{:else}Building your knowledge vault…{/if}
    </h3>
    <div class="flex items-center gap-3">
      <span class="text-xs text-sand-500">{totalNotes} note{totalNotes === 1 ? '' : 's'} written</span>
      {#if !complete}
        <button class="text-xs text-red-600 hover:underline disabled:text-sand-400" disabled={stopping} on:click={stop}>
          {stopping ? 'Stopping…' : 'Stop ingestion'}
        </button>
      {/if}
    </div>
  </div>

  {#if !complete}
    <p class="text-xs text-sand-500">
      A coding agent is reading your repositories and documenting how you build software into a
      linked vault. This runs in the background — you can keep going, but training works best once
      it's done{runningRepo ? `, currently on ${runningRepo}` : ''}.
    </p>
  {/if}

  <!-- Per-repo status -->
  <ul class="space-y-1">
    {#each repoList as r (r.repo)}
      <li class="flex items-center gap-2 text-sm">
        <span class="w-2 h-2 rounded-full {dot(r.status)}"></span>
        <span class="font-mono text-sand-700 truncate">{r.repo}</span>
        <span class="text-xs text-sand-400 ml-auto">
          {r.status === 'failed' ? 'failed' : `${r.notes_count} notes`}
        </span>
      </li>
    {:else}
      <li class="text-xs text-sand-400">Waiting for the first repo to start…</li>
    {/each}
  </ul>

  <!-- Live transcript -->
  <div bind:this={logEl} class="bg-sand-900 text-sand-100 rounded p-2 text-[11px] font-mono h-40 overflow-y-auto leading-relaxed">
    {#each log as line}
      <div class={line.kind === 'note' ? 'text-green-300' : 'text-sand-300'}>
        <span class="text-sand-500">{line.repo ? `[${line.repo}] ` : ''}</span>{line.detail}
      </div>
    {:else}
      <div class="text-sand-500">…</div>
    {/each}
  </div>

  {#if complete}
    <div class="text-sm text-green-700 font-medium">
      ✓ Vault ready — {totalNotes} notes across {repoList.length} repos. Safe to continue to the interview and training.
    </div>
  {/if}
</div>
