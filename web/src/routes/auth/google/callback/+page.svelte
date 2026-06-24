<script lang="ts">
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { page } from '$app/stores';
  import { api } from '$lib/api/client';

  let error = '';

  onMount(async () => {
    const code = $page.url.searchParams.get('code');
    const state = $page.url.searchParams.get('state');

    if (!code || !state) {
      error = 'Missing OAuth parameters. Please try again.';
      return;
    }

    try {
      const res = await api.post<{ google_email: string; agent_id: string }>(
        '/auth/google/callback',
        { code, state }
      );
      goto(`/studio/onboarding?google_connected=1&agent_id=${res.agent_id}&google_email=${encodeURIComponent(res.google_email)}`);
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Google authorization failed';
    }
  });
</script>

<svelte:head><title>Connecting Google — Flaborful</title></svelte:head>

<div class="min-h-[calc(100vh-56px)] flex items-center justify-center px-4">
  <div class="w-full max-w-sm text-center">
    {#if error}
      <div class="card p-8">
        <div class="text-red-600 text-sm bg-red-50 rounded-lg px-3 py-2 mb-4">{error}</div>
        <a href="/studio/onboarding" class="btn-secondary text-sm">Back to onboarding</a>
      </div>
    {:else}
      <div class="card p-8">
        <div class="text-brand-600 text-4xl mb-4 animate-pulse">↻</div>
        <p class="text-gray-600 text-sm">Connecting your Google account...</p>
      </div>
    {/if}
  </div>
</div>
