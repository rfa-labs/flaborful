<script lang="ts">
  import { onMount } from 'svelte';
  import { page } from '$app/stores';
  import { goto } from '$app/navigation';
  import { api } from '$lib/api/client';
  import { session } from '$lib/stores/session';
  import { get } from 'svelte/store';
  import type { Job, KnowledgeChunkView } from '$lib/api/types';

  $: jobId = $page.params.id;

  let job: Job | null = null;
  let agentId = '';
  let chunks: KnowledgeChunkView[] = [];
  let loading = true;
  let error = '';
  let submitting = false;
  let submitted = false;

  // Application form
  let resumeText = '';
  let coverLetter = '';
  let videoUrl = '';
  let selectedChunkIds = new Set<string>();

  // Only shareable chunks can be attached to an application.
  $: shareableChunks = chunks.filter(c => c.visibility === 'shareable');

  onMount(async () => {
    try {
      agentId = get(session).agentId ?? (await api.get<{ id: string }>('/agents/mine')).id;
      const [j, kb] = await Promise.all([
        api.get<Job>(`/jobs/${jobId}`),
        api.get<{ chunks: KnowledgeChunkView[] }>(`/knowledge/agent/${agentId}`),
      ]);
      job = j;
      chunks = kb.chunks ?? [];
      // Pre-fill resume from the stored resume chunk if present.
      const resume = chunks.find(c => c.source_type === 'resume');
      if (resume) resumeText = resume.content_preview;
      // Pre-select all shareable chunks by default; the owner can deselect.
      selectedChunkIds = new Set(shareableChunks.map(c => c.id));
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to load job';
    } finally {
      loading = false;
    }
  });

  function toggleChunk(id: string) {
    if (selectedChunkIds.has(id)) selectedChunkIds.delete(id);
    else selectedChunkIds.add(id);
    selectedChunkIds = new Set(selectedChunkIds);
  }

  async function apply() {
    if (!coverLetter.trim()) { error = 'A cover letter is required.'; return; }
    submitting = true; error = '';
    try {
      await api.post(`/jobs/${jobId}/apply`, {
        resume_text: resumeText.trim() || null,
        cover_letter: coverLetter.trim(),
        video_url: videoUrl.trim() || null,
        shared_chunk_ids: Array.from(selectedChunkIds),
      });
      submitted = true;
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to apply';
    } finally {
      submitting = false;
    }
  }
</script>

<svelte:head><title>{job?.title ?? 'Apply'} — Flaborful</title></svelte:head>

<div class="max-w-3xl mx-auto px-4 py-8">
  <button class="btn-ghost text-sm mb-6" on:click={() => goto('/studio/jobs/board')}>← Job board</button>

  {#if loading}
    <div class="card p-8 animate-pulse"><div class="h-40 bg-sand-100 rounded"></div></div>
  {:else if error && !job}
    <div class="card p-8 text-center text-red-600">{error}</div>
  {:else if submitted}
    <div class="card p-12 text-center">
      <div class="text-6xl mb-4">✅</div>
      <h2 class="text-2xl font-bold mb-2">Application submitted</h2>
      <p class="text-sand-500 mb-6">The company will review your agent and reach out if there's a fit. Your cover letter has been added to your agent's knowledge base.</p>
      <button class="btn-primary px-8" on:click={() => goto('/studio/jobs/board')}>Back to job board</button>
    </div>
  {:else if job}
    <!-- Job posting -->
    <div class="card p-6 mb-6">
      <h1 class="text-2xl font-bold mb-1">{job.title}</h1>
      <div class="text-sm text-sand-400 mb-4">${(job.hourly_rate / 100).toFixed(0)}/hr · posted {new Date(job.created_at).toLocaleDateString()}</div>
      <div class="prose prose-sm max-w-none">
        <p class="text-sand-700 whitespace-pre-wrap">{job.description}</p>
        {#if job.requirements}
          <h3 class="text-sm font-semibold text-sand-800 mt-4 mb-1">Requirements & tasks</h3>
          <p class="text-sand-700 whitespace-pre-wrap">{job.requirements}</p>
        {/if}
      </div>
    </div>

    <!-- Application -->
    <div class="card p-6 space-y-5">
      <h2 class="text-lg font-semibold">Apply with your agent</h2>

      <div>
        <label class="label" for="cover">Cover letter <span class="text-sand-400 font-normal">— why your agent is a fit</span></label>
        <textarea class="input min-h-[120px] resize-y" id="cover" bind:value={coverLetter}
          placeholder="Introduce yourself and your agent, and explain why you're a strong fit for this role..."></textarea>
      </div>

      <div>
        <label class="label" for="resume">Resume</label>
        <textarea class="input min-h-[100px] resize-y text-sm" id="resume" bind:value={resumeText}
          placeholder="Paste your resume. Defaults from your saved resume if you have one."></textarea>
      </div>

      <div>
        <label class="label" for="video">Self-intro video URL <span class="text-sand-400 font-normal">(optional)</span></label>
        <input class="input" id="video" bind:value={videoUrl} placeholder="https://loom.com/... or YouTube link" />
        <p class="text-xs text-sand-400 mt-1">A short video pitch the company can watch. Adds to your agent's knowledge base too.</p>
      </div>

      <!-- Knowledge sharing (IP control) -->
      <div>
        <div class="label">Work samples to share</div>
        <p class="text-xs text-sand-400 mb-2">
          Choose which knowledge-base items the company may see. Only items you've marked <strong>shareable</strong> appear here — manage that in your
          <button class="text-brand-600 hover:underline" on:click={() => goto('/studio/knowledge')}>Knowledge vault</button>.
        </p>
        {#if shareableChunks.length === 0}
          <div class="text-sm text-amber-700 bg-amber-50 border border-amber-200 rounded-lg px-3 py-2">
            You haven't marked any knowledge as shareable. You can still apply with just a cover letter and resume.
          </div>
        {:else}
          <div class="space-y-2 max-h-72 overflow-y-auto">
            {#each shareableChunks as c}
              <label class="flex items-start gap-2 p-2 rounded-lg border cursor-pointer transition-colors {selectedChunkIds.has(c.id) ? 'border-brand-300 bg-brand-50' : 'border-sand-200 hover:border-sand-300'}">
                <input type="checkbox" class="mt-1" checked={selectedChunkIds.has(c.id)} on:change={() => toggleChunk(c.id)} />
                <div class="min-w-0">
                  <span class="text-[10px] uppercase tracking-wide text-sand-400 block">{c.source_type.replace(/_/g, ' ')}</span>
                  <span class="text-sm text-sand-700 line-clamp-2">{c.content_preview}</span>
                </div>
              </label>
            {/each}
          </div>
          <p class="text-xs text-sand-400 mt-1">{selectedChunkIds.size} of {shareableChunks.length} selected</p>
        {/if}
      </div>

      {#if error}<div class="text-red-600 text-sm bg-red-50 rounded-lg px-3 py-2">{error}</div>{/if}

      <div class="flex justify-end gap-2">
        <button class="btn-ghost" on:click={() => goto('/studio/jobs/board')}>Cancel</button>
        <button class="btn-primary px-8" disabled={!coverLetter.trim() || submitting} on:click={apply}>
          {submitting ? 'Submitting...' : 'Submit application'}
        </button>
      </div>
    </div>
  {/if}
</div>
