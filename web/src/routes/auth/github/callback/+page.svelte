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
      const res = await api.post<{ github_login: string; agent_id: string | null }>(
        '/auth/github/callback',
        { code, state }
      );
      if (res.agent_id) {
        // Professional onboarding flow — resume it.
        goto(`/studio/onboarding?github_connected=1&agent_id=${res.agent_id}&github_login=${encodeURIComponent(res.github_login)}`);
      } else {
        // Company connect — return to wherever they kicked it off (e.g. the job's Integrations tab).
        const back = localStorage.getItem('gh_connect_return') || '/hire';
        localStorage.removeItem('gh_connect_return');
        goto(back);
      }
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'GitHub authorization failed';
    }
  });
</script>

<svelte:head><title>Connecting GitHub — Flaborful</title></svelte:head>

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
        <p class="text-sand-600 text-sm">Connecting your GitHub account...</p>
      </div>
    {/if}
  </div>
</div>
