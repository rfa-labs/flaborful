<script lang="ts">
  import { onMount } from 'svelte';
  import { page } from '$app/stores';
  import { goto } from '$app/navigation';
  import { api } from '$lib/api/client';
  import type { AgentListing } from '$lib/api/types';

  $: agentId = $page.params.id;

  let listing: AgentListing | null = null;
  let loading = true;
  let error = '';

  onMount(async () => {
    try {
      const res = await api.get<{ agent_id: string } & AgentListing>(`/marketplace/${agentId}`);
      listing = res;
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Agent not found';
    } finally {
      loading = false;
    }
  });

  function formatRate(cents: number) {
    return `$${(cents / 100).toFixed(0)}/hr`;
  }
</script>

<svelte:head><title>{listing?.professional_name ?? 'Agent'} — Flaborful</title></svelte:head>

{#if loading}
  <div class="max-w-4xl mx-auto px-4 py-10">
    <div class="card p-8 animate-pulse">
      <div class="flex items-center gap-4 mb-6">
        <div class="w-20 h-20 rounded-full bg-gray-200"></div>
        <div class="flex-1">
          <div class="h-6 bg-gray-200 rounded w-48 mb-2"></div>
          <div class="h-4 bg-gray-100 rounded w-32"></div>
        </div>
      </div>
      <div class="h-24 bg-gray-100 rounded"></div>
    </div>
  </div>
{:else if error || !listing}
  <div class="max-w-4xl mx-auto px-4 py-10">
    <div class="card p-8 text-center text-red-600">{error || 'Not found'}</div>
  </div>
{:else}
  <div class="max-w-4xl mx-auto px-4 py-10">
    <button class="btn-ghost text-sm mb-6" on:click={() => goto('/marketplace')}>← Back to marketplace</button>

    <div class="grid lg:grid-cols-3 gap-6">
      <!-- Main profile -->
      <div class="lg:col-span-2 space-y-5">
        <!-- Header -->
        <div class="card p-6">
          <div class="flex items-start gap-4">
            <div class="w-20 h-20 rounded-full bg-brand-100 flex items-center justify-center text-brand-600 font-bold text-3xl shrink-0">
              {listing.professional_name[0]}
            </div>
            <div class="flex-1">
              <h1 class="text-2xl font-bold mb-0.5">{listing.professional_name}</h1>
              <div class="text-brand-600 font-medium mb-2">{listing.profession_name}</div>
              {#if listing.bio}
                <p class="text-gray-600 text-sm leading-relaxed">{listing.bio}</p>
              {/if}
            </div>
          </div>
        </div>

        <!-- Aspects -->
        <div class="card p-5">
          <h2 class="font-semibold mb-3">Skills & Aspects</h2>
          <div class="flex flex-wrap gap-2">
            {#each listing.aspects_offered as aspect}
              <span class="badge-blue px-3 py-1">{aspect}</span>
            {/each}
          </div>
        </div>

        <!-- Stats -->
        <div class="card p-5">
          <h2 class="font-semibold mb-4">Track record</h2>
          <div class="grid grid-cols-2 gap-6">
            <div class="text-center">
              <div class="text-3xl font-bold text-brand-600">{listing.completed_jobs}</div>
              <div class="text-sm text-gray-500 mt-1">Completed jobs</div>
            </div>
            <div class="text-center">
              <div class="text-3xl font-bold text-brand-600">{listing.approved_outputs}</div>
              <div class="text-sm text-gray-500 mt-1">Approved outputs</div>
            </div>
          </div>
        </div>

        <!-- How it works -->
        <div class="card p-5">
          <h2 class="font-semibold mb-3">What you're hiring</h2>
          <ul class="space-y-2 text-sm text-gray-600">
            {#each [
              'An AI agent trained on this professional\'s actual work history and style',
              'Every output reviewed and approved by the professional before delivery',
              'Tri-party contract auto-generated protecting your IP and theirs',
              'Full audit trail — every action logged for your records',
              'Kill switch — revoke access instantly at any time',
            ] as point}
              <li class="flex items-start gap-2">
                <span class="text-brand-500 mt-0.5 shrink-0">✓</span>
                {point}
              </li>
            {/each}
          </ul>
        </div>
      </div>

      <!-- Discovery card: hiring happens via job postings + applications -->
      <div class="space-y-4">
        <div class="card p-5 sticky top-20">
          <div class="text-3xl font-bold text-gray-900 mb-1">{formatRate(listing.hourly_rate)}</div>
          <div class="text-sm text-gray-500 mb-5">Typical rate · billed weekly · 15% platform fee</div>

          <button class="btn-primary w-full py-3 text-base" on:click={() => goto('/hire/jobs/new')}>
            Post a job
          </button>
          <p class="text-xs text-gray-400 text-center mt-3">
            Post a role and this professional can apply with their agent. You choose who to hire.
          </p>
        </div>
      </div>
    </div>
  </div>
{/if}
