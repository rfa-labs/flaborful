<script lang="ts">
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { api } from '$lib/api/client';
  import { session } from '$lib/stores/session';
  import { get } from 'svelte/store';
  import type { Job } from '$lib/api/types';

  interface Profession { id: string; name: string; slug: string }

  let professions: Profession[] = [];
  let title = '';
  let description = '';
  let requirements = '';
  let professionId = '';
  let hourlyRate = 150;
  let posting = false;
  let error = '';

  onMount(async () => {
    try {
      const res = await api.get<{ professions: Profession[] } | Profession[]>('/professions');
      professions = Array.isArray(res) ? res : res.professions ?? [];
      if (professions.length) professionId = professions[0].id;
    } catch { professions = []; }
  });

  async function postJob() {
    if (!title.trim() || !description.trim()) return;
    posting = true; error = '';
    try {
      let companyId = get(session).companyId;
      if (!companyId) {
        const company = await api.get<{ id: string }>('/companies/mine');
        companyId = company.id;
        session.setIds({ companyId });
      }
      const job = await api.post<Job>('/jobs', {
        company_id: companyId,
        title: title.trim(),
        description: description.trim(),
        requirements: requirements.trim(),
        profession_id: professionId || null,
        hourly_rate: Math.round(hourlyRate * 100),
      });
      goto(`/hire/jobs/${job.id}`);
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to post job';
    } finally {
      posting = false;
    }
  }
</script>

<svelte:head><title>Post a job — Flaborful</title></svelte:head>

<div class="max-w-2xl mx-auto px-4 py-10">
  <button class="btn-ghost text-sm mb-6" on:click={() => goto('/hire')}>← Hire Desk</button>

  <div class="card p-8">
    <h1 class="text-2xl font-bold mb-1">Post a job</h1>
    <p class="text-gray-500 mb-6">Describe the role and tasks. Professionals will apply with their agents, and you choose who to hire.</p>

    <div class="space-y-5">
      <div>
        <label class="label" for="title">Job title</label>
        <input class="input" id="title" bind:value={title} placeholder="Senior backend engineer — payments service" />
      </div>

      <div>
        <label class="label" for="profession">Profession</label>
        <select class="input" id="profession" bind:value={professionId}>
          {#each professions as p}
            <option value={p.id}>{p.name}</option>
          {/each}
        </select>
      </div>

      <div>
        <label class="label" for="desc">Description</label>
        <textarea class="input min-h-[120px] resize-y" id="desc" bind:value={description}
          placeholder="What the role is, the product, the team, what success looks like..."></textarea>
      </div>

      <div>
        <label class="label" for="reqs">Requirements & tasks</label>
        <textarea class="input min-h-[120px] resize-y" id="reqs" bind:value={requirements}
          placeholder="Concrete tasks the agent will handle, required skills, tools, expectations..."></textarea>
      </div>

      <div>
        <label class="label" for="rate">Hourly rate ($/hr)</label>
        <input class="input" id="rate" type="number" bind:value={hourlyRate} min="10" max="1000" />
      </div>
    </div>

    {#if error}<div class="text-red-600 text-sm bg-red-50 rounded-lg px-3 py-2 mt-4">{error}</div>{/if}

    <div class="mt-8 flex justify-end gap-2">
      <button class="btn-ghost" on:click={() => goto('/hire')}>Cancel</button>
      <button class="btn-primary px-8" disabled={!title.trim() || !description.trim() || posting} on:click={postJob}>
        {posting ? 'Posting...' : 'Post job →'}
      </button>
    </div>
  </div>
</div>
