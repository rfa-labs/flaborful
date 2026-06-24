<script lang="ts">
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { api } from '$lib/api/client';
  import { session } from '$lib/stores/session';

  interface Payout {
    id: string;
    invoice_id: string;
    amount: number;
    status: string;
    created_at: string;
    sent_at: string | null;
  }

  let payouts: Payout[] = [];
  let loading = true;
  let error = '';

  $: totalPaid = payouts.filter(p => p.status === 'sent').reduce((s, p) => s + p.amount, 0);
  $: totalPending = payouts.filter(p => p.status === 'pending').reduce((s, p) => s + p.amount, 0);

  onMount(async () => {
    try {
      let pid = $session.professionalId;
      if (!pid) {
        const prof = await api.get<{ id: string }>('/professionals/mine');
        session.setIds({ professionalId: prof.id });
        pid = prof.id;
      }
      payouts = await api.get<Payout[]>(`/billing/professional/${pid}/payouts`);
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to load payouts';
    } finally {
      loading = false;
    }
  });

  function timeAgo(iso: string) {
    const diff = Date.now() - new Date(iso).getTime();
    const d = Math.floor(diff / 86400000);
    if (d < 1) return 'today';
    if (d === 1) return 'yesterday';
    return `${d}d ago`;
  }
</script>

<svelte:head><title>Payouts — Flaborful Studio</title></svelte:head>

<div class="max-w-4xl mx-auto px-4 py-8">
  <button class="btn-ghost text-sm mb-6" on:click={() => goto('/studio')}>← Studio</button>

  <h1 class="text-2xl font-bold mb-1">Payouts</h1>
  <p class="text-gray-500 text-sm mb-8">Your earnings after the 15% platform fee.</p>

  <!-- Summary cards -->
  <div class="grid sm:grid-cols-2 gap-4 mb-8">
    <div class="card p-5">
      <div class="text-sm text-gray-500 mb-1">Total paid out</div>
      <div class="text-3xl font-bold text-green-600">${(totalPaid / 100).toFixed(2)}</div>
    </div>
    <div class="card p-5">
      <div class="text-sm text-gray-500 mb-1">Pending</div>
      <div class="text-3xl font-bold text-yellow-600">${(totalPending / 100).toFixed(2)}</div>
    </div>
  </div>

  {#if loading}
    <div class="space-y-3">
      {#each Array(3) as _}
        <div class="card p-4 animate-pulse"><div class="h-10 bg-gray-100 rounded"></div></div>
      {/each}
    </div>
  {:else if error}
    <div class="card p-6 text-center text-red-600">{error}</div>
  {:else if payouts.length === 0}
    <div class="card p-12 text-center text-gray-400 text-sm">
      No payouts yet. Payouts are generated weekly when jobs are billed.
    </div>
  {:else}
    <div class="card">
      <table class="w-full text-sm">
        <thead class="bg-gray-50 text-xs text-gray-500 uppercase tracking-wide">
          <tr>
            <th class="px-5 py-3 text-left">Date</th>
            <th class="px-5 py-3 text-right">Amount</th>
            <th class="px-5 py-3 text-left">Status</th>
            <th class="px-5 py-3 text-left">Sent</th>
          </tr>
        </thead>
        <tbody class="divide-y divide-gray-100">
          {#each payouts as p}
            <tr class="hover:bg-gray-50">
              <td class="px-5 py-3 text-gray-700">{timeAgo(p.created_at)}</td>
              <td class="px-5 py-3 text-right font-medium text-gray-900">${(p.amount / 100).toFixed(2)}</td>
              <td class="px-5 py-3">
                <span class="badge-{p.status === 'sent' ? 'green' : p.status === 'failed' ? 'red' : 'yellow'}">
                  {p.status}
                </span>
              </td>
              <td class="px-5 py-3 text-gray-400">{p.sent_at ? timeAgo(p.sent_at) : '—'}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</div>
