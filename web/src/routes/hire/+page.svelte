<script lang="ts">
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { api } from '$lib/api/client';
  import { session } from '$lib/stores/session';

  let dashboard: {
    active_agents: number;
    tasks_delivered_this_week: number;
    tasks_pending: number;
    total_hours_billed: number;
    recent_deliveries: Array<{
      task_id: string;
      job_title: string;
      professional_name: string;
      delivered_at: string | null;
      confidence_score: number | null;
    }>;
  } | null = null;

  let loading = true;
  let error = '';
  let showCreateForm = false;
  let creating = false;
  let companyName = '';
  let companyIndustry = '';
  let companyWebsite = '';

  import type { Job } from '$lib/api/types';
  let jobs: Job[] = [];

  async function loadJobs(companyId: string) {
    try { jobs = await api.get<Job[]>(`/jobs/company/${companyId}`); } catch { jobs = []; }
  }

  function jobStatusBadge(s: string) {
    if (s === 'reviewing') return 'badge-yellow';
    if (s === 'active' || s === 'trial' || s === 'open') return 'badge-green';
    if (s === 'completed') return 'badge-gray';
    return 'badge-gray';
  }

  async function createCompany() {
    if (!companyName.trim()) return;
    creating = true;
    try {
      const company = await api.post<{ id: string }>('/companies', {
        name: companyName.trim(),
        industry: companyIndustry.trim() || null,
        website: companyWebsite.trim() || null,
      });
      session.setIds({ companyId: company.id });
      dashboard = await api.get(`/dashboard/company/${company.id}`);
      showCreateForm = false;
      error = '';
    } catch (e: any) {
      error = e.message ?? 'Failed to create company';
    } finally {
      creating = false;
    }
  }

  onMount(async () => {
    try {
      let cid = $session.companyId;
      if (!cid) {
        const company = await api.get<{ id: string }>('/companies/mine');
        session.setIds({ companyId: company.id });
        cid = company.id;
      }
      dashboard = await api.get(`/dashboard/company/${cid}`);
      if (cid) await loadJobs(cid);
    } catch {
      error = 'No company found. Set up your company first.';
    } finally {
      loading = false;
    }
  });
</script>

<svelte:head><title>Hire Desk — Flaborful</title></svelte:head>

<div class="max-w-7xl mx-auto px-4 py-8">
  <div class="flex items-center justify-between mb-8">
    <div>
      <h1 class="text-2xl font-bold">Hire Desk</h1>
      <p class="text-gray-500 text-sm mt-0.5">Manage your hired AI agents</p>
    </div>
    <button class="btn-primary" on:click={() => goto('/hire/jobs/new')}>
      Post a job
    </button>
  </div>

  {#if loading}
    <div class="grid sm:grid-cols-4 gap-4 mb-8">
      {#each Array(4) as _}
        <div class="card p-4 animate-pulse"><div class="h-12 bg-gray-100 rounded"></div></div>
      {/each}
    </div>
  {:else if error || !dashboard}
    <div class="card p-12 text-center max-w-md mx-auto">
      <div class="text-5xl mb-4">🏢</div>
      <h2 class="text-xl font-semibold mb-2">Set up your company</h2>
      <p class="text-gray-500 mb-6">Create your company profile to start hiring agents.</p>
      {#if !showCreateForm}
        <button class="btn-primary px-8" on:click={() => showCreateForm = true}>Create company profile</button>
      {:else}
        <div class="text-left space-y-4">
          <div>
            <label class="block text-sm font-medium text-gray-700 mb-1">Company name *</label>
            <input class="input w-full" bind:value={companyName} placeholder="Acme Corp" />
          </div>
          <div>
            <label class="block text-sm font-medium text-gray-700 mb-1">Industry</label>
            <input class="input w-full" bind:value={companyIndustry} placeholder="Software, Finance, etc." />
          </div>
          <div>
            <label class="block text-sm font-medium text-gray-700 mb-1">Website</label>
            <input class="input w-full" bind:value={companyWebsite} placeholder="https://acme.com" />
          </div>
          {#if error}
            <p class="text-red-500 text-sm">{error}</p>
          {/if}
          <div class="flex gap-3 pt-2">
            <button class="btn-primary flex-1" on:click={createCompany} disabled={creating}>
              {creating ? 'Creating...' : 'Create'}
            </button>
            <button class="btn-ghost flex-1" on:click={() => showCreateForm = false}>Cancel</button>
          </div>
        </div>
      {/if}
    </div>
  {:else}
    <!-- Stats -->
    <div class="grid sm:grid-cols-2 lg:grid-cols-4 gap-4 mb-8">
      {#each [
        { label: 'Active agents', value: dashboard.active_agents },
        { label: 'Delivered this week', value: dashboard.tasks_delivered_this_week },
        { label: 'In progress', value: dashboard.tasks_pending },
        { label: 'Hours billed', value: dashboard.total_hours_billed.toFixed(1) },
      ] as s}
        <div class="card p-4">
          <div class="text-xs text-gray-500 font-medium uppercase tracking-wide mb-1">{s.label}</div>
          <div class="text-2xl font-bold">{s.value}</div>
        </div>
      {/each}
    </div>

    <!-- Job postings -->
    <div class="card mb-6">
      <div class="px-5 py-4 border-b border-gray-100 flex items-center justify-between">
        <h2 class="font-semibold">Your job postings</h2>
        <button class="btn-secondary text-sm" on:click={() => goto('/hire/jobs/new')}>+ Post a job</button>
      </div>
      {#if jobs.length === 0}
        <div class="p-8 text-center text-gray-400 text-sm">
          No postings yet. <button class="text-brand-600 hover:underline" on:click={() => goto('/hire/jobs/new')}>Post your first job</button> to start receiving applications.
        </div>
      {:else}
        <ul class="divide-y divide-gray-100">
          {#each jobs as job}
            <li class="px-5 py-4 flex items-center gap-4 hover:bg-gray-50 transition-colors cursor-pointer"
              on:click={() => goto(`/hire/jobs/${job.id}`)}
              on:keydown={(e) => e.key === 'Enter' && goto(`/hire/jobs/${job.id}`)}
              role="button" tabindex="0">
              <div class="flex-1 min-w-0">
                <div class="font-medium text-sm truncate">{job.title}</div>
                <div class="text-xs text-gray-400 mt-0.5">${(job.hourly_rate / 100).toFixed(0)}/hr · posted {new Date(job.created_at).toLocaleDateString()}</div>
              </div>
              <span class="{jobStatusBadge(job.status)} text-xs capitalize shrink-0">{job.status === 'reviewing' ? 'Accepting applicants' : job.status}</span>
              {#if job.status === 'reviewing'}
                <span class="text-xs text-brand-600 shrink-0">Review applicants →</span>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
    </div>

    <!-- Recent deliveries -->
    <div class="card">
      <div class="px-5 py-4 border-b border-gray-100">
        <h2 class="font-semibold">Recent deliveries</h2>
      </div>
      {#if dashboard.recent_deliveries.length === 0}
        <div class="p-8 text-center text-gray-400 text-sm">No deliveries yet.</div>
      {:else}
        <table class="w-full text-sm">
          <thead class="bg-gray-50 text-xs text-gray-500 uppercase tracking-wide">
            <tr>
              <th class="px-5 py-3 text-left">Job</th>
              <th class="px-5 py-3 text-left">Professional</th>
              <th class="px-5 py-3 text-left">Delivered</th>
              <th class="px-5 py-3 text-left">Confidence</th>
              <th class="px-5 py-3"></th>
            </tr>
          </thead>
          <tbody class="divide-y divide-gray-100">
            {#each dashboard.recent_deliveries as d}
              <tr class="hover:bg-gray-50 transition-colors">
                <td class="px-5 py-3 font-medium">{d.job_title}</td>
                <td class="px-5 py-3 text-gray-600">{d.professional_name}</td>
                <td class="px-5 py-3 text-gray-500">
                  {d.delivered_at ? new Date(d.delivered_at).toLocaleDateString() : '—'}
                </td>
                <td class="px-5 py-3">
                  {#if d.confidence_score != null}
                    <span class="badge-blue">{Math.round(d.confidence_score * 100)}%</span>
                  {:else}
                    <span class="text-gray-400">—</span>
                  {/if}
                </td>
                <td class="px-5 py-3 text-right">
                  <button class="btn-ghost text-xs"
                    on:click={() => goto(`/hire/tasks/${d.task_id}`)}>
                    View
                  </button>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </div>
  {/if}
</div>
