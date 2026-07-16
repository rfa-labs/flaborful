<script lang="ts">
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { api } from '$lib/api/client';
  import { session, isLoggedIn } from '$lib/stores/session';

  onMount(() => {
    if ($isLoggedIn) goto($session.role === 'company' ? '/hire' : '/studio');
  });

  let email = '';
  let password = '';
  let submitting = false;
  let error = '';

  async function submit() {
    submitting = true;
    error = '';
    try {
      const res = await api.post<{ user_id: string; token: string; role: string }>('/auth/login', {
        email, password,
      });
      session.setToken(res.token, res.user_id, res.role);

      // Restore professional/agent/company IDs so the session is complete after login
      if (res.role === 'professional') {
        try {
          const prof = await api.get<{ id: string }>('/professionals/mine');
          const agent = await api.get<{ id: string }>('/agents/mine');
          session.setIds({ professionalId: prof.id, agentId: agent.id });
        } catch { /* first-time user — no profile yet, onboarding will set these */ }
      } else if (res.role === 'company') {
        try {
          const company = await api.get<{ id: string }>('/companies/mine');
          session.setIds({ companyId: company.id });
        } catch { /* first-time company user */ }
      }

      goto(res.role === 'company' ? '/hire' : '/studio');
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Login failed';
    } finally {
      submitting = false;
    }
  }
</script>

<svelte:head><title>Sign in — Flaborful</title></svelte:head>

<div class="min-h-[calc(100vh-56px)] flex items-center justify-center px-4">
  <div class="w-full max-w-sm">
    <div class="card p-8">
      <h1 class="text-2xl font-bold mb-1">Sign in</h1>
      <p class="text-sand-500 text-sm mb-6">Welcome back.</p>

      <form on:submit|preventDefault={submit} class="space-y-4">
        <div>
          <label class="label" for="email">Email</label>
          <input class="input" id="email" type="email" bind:value={email} required placeholder="you@company.com" />
        </div>
        <div>
          <label class="label" for="password">Password</label>
          <input class="input" id="password" type="password" bind:value={password} required placeholder="••••••••" />
        </div>

        {#if error}
          <div class="text-red-600 text-sm bg-red-50 rounded-lg px-3 py-2">{error}</div>
        {/if}

        <button type="submit" class="btn-primary w-full py-2.5" disabled={submitting}>
          {submitting ? 'Signing in...' : 'Sign in'}
        </button>
      </form>

      <p class="text-center text-sm text-sand-500 mt-4">
        No account? <a href="/auth/register" class="text-brand-600 hover:underline">Create one</a>
      </p>
    </div>
  </div>
</div>
