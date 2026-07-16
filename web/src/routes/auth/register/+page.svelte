<script lang="ts">
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { page } from '$app/stores';
  import { api } from '$lib/api/client';
  import { session, isLoggedIn } from '$lib/stores/session';

  onMount(() => {
    if ($isLoggedIn) goto($session.role === 'company' ? '/hire' : '/studio');
  });

  $: asPortal = $page.url.searchParams.get('as') ?? 'professional';

  let email = '';
  let password = '';
  let name = '';
  let submitting = false;
  let error = '';

  async function submit() {
    submitting = true;
    error = '';
    try {
      const role = asPortal === 'company' ? 'company' : 'professional';
      const res = await api.post<{ user_id: string; token: string; role: string }>('/auth/register', {
        email, password, role,
      });
      session.setToken(res.token, res.user_id, res.role);
      goto(asPortal === 'company' ? '/hire' : '/studio/onboarding');
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Registration failed';
    } finally {
      submitting = false;
    }
  }
</script>

<svelte:head><title>Register — Flaborful</title></svelte:head>

<div class="min-h-[calc(100vh-56px)] flex items-center justify-center px-4">
  <div class="w-full max-w-md">
    <div class="card p-8">
      <h1 class="text-2xl font-bold mb-1">Create account</h1>
      <p class="text-sand-500 text-sm mb-6">
        {asPortal === 'company' ? 'Hire AI agents for your team.' : 'Build your professional AI agent.'}
      </p>

      <!-- Portal toggle -->
      <div class="flex bg-sand-100 rounded-lg p-0.5 mb-6 text-sm font-medium gap-0.5">
        <a href="?as=professional"
           class="flex-1 text-center py-2 rounded-md transition-colors {asPortal === 'professional' ? 'bg-white shadow' : 'text-sand-500 hover:text-sand-700'}">
          Professional
        </a>
        <a href="?as=company"
           class="flex-1 text-center py-2 rounded-md transition-colors {asPortal === 'company' ? 'bg-white shadow' : 'text-sand-500 hover:text-sand-700'}">
          Company
        </a>
      </div>

      <form on:submit|preventDefault={submit} class="space-y-4">
        <div>
          <label class="label" for="name">Full name</label>
          <input class="input" id="name" bind:value={name} required placeholder="Rashad Arafeh" />
        </div>
        <div>
          <label class="label" for="email">Email</label>
          <input class="input" id="email" type="email" bind:value={email} required placeholder="you@company.com" />
        </div>
        <div>
          <label class="label" for="password">Password</label>
          <input class="input" id="password" type="password" bind:value={password} required placeholder="••••••••" minlength="8" />
        </div>

        {#if error}
          <div class="text-red-600 text-sm bg-red-50 rounded-lg px-3 py-2">{error}</div>
        {/if}

        <button type="submit" class="btn-primary w-full py-2.5" disabled={submitting}>
          {submitting ? 'Creating account...' : 'Create account'}
        </button>
      </form>

      <p class="text-center text-sm text-sand-500 mt-4">
        Already have an account? <a href="/auth/login" class="text-brand-600 hover:underline">Sign in</a>
      </p>
    </div>
  </div>
</div>
