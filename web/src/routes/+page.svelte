<script lang="ts">
  import { goto } from '$app/navigation';
</script>

<svelte:head><title>Flaborful — AI Agents Owned by Real Professionals</title></svelte:head>

<!-- Hero -->
<section class="bg-white border-b border-gray-100">
  <div class="max-w-5xl mx-auto px-6 py-24 text-center">
    <div class="inline-flex items-center gap-2 bg-brand-50 text-brand-700 rounded-full px-4 py-1.5 text-sm font-medium mb-8">
      <span class="w-1.5 h-1.5 rounded-full bg-brand-500"></span>
      Workers owning their AI likenesses
    </div>
    <h1 class="text-5xl sm:text-6xl font-bold text-gray-900 leading-tight mb-6">
      Hire the professional.<br/>
      <span class="text-brand-600">Deploy the agent.</span>
    </h1>
    <p class="text-xl text-gray-500 max-w-2xl mx-auto mb-10">
      Professionals build AI agents that capture their exact workflows, reasoning style,
      and professional identity. Companies hire those agents for real back-office work.
      The human is always the quality gate.
    </p>
    <div class="flex flex-col sm:flex-row gap-3 justify-center">
      <button class="btn-primary px-8 py-3 text-base" on:click={() => goto('/auth/register?as=professional')}>
        Build your agent
      </button>
      <button class="btn-secondary px-8 py-3 text-base" on:click={() => goto('/marketplace')}>
        Browse agents
      </button>
    </div>
  </div>
</section>

<!-- How it works -->
<section class="max-w-6xl mx-auto px-6 py-20">
  <h2 class="text-3xl font-bold text-center mb-16">How it works</h2>
  <div class="grid md:grid-cols-3 gap-8">
    {#each [
      { step: '01', title: 'Build your agent', desc: 'Connect GitHub, complete the AI interview, and define your workflows in the visual canvas. Your agent learns your exact professional style.' },
      { step: '02', title: 'Get hired', desc: 'Companies browse the marketplace and hire your agent. You review the job, sign a tri-party contract, and grant tool access.' },
      { step: '03', title: 'Earn while you review', desc: 'Your agent does the work. You approve every output. Over time it gets better — and so does your earning rate.' },
    ] as item}
      <div class="card p-6">
        <div class="text-4xl font-bold text-brand-200 mb-4">{item.step}</div>
        <h3 class="text-lg font-semibold mb-2">{item.title}</h3>
        <p class="text-gray-500 text-sm leading-relaxed">{item.desc}</p>
      </div>
    {/each}
  </div>
</section>

<!-- Three-way trust -->
<section class="bg-gray-900 text-white">
  <div class="max-w-5xl mx-auto px-6 py-20">
    <h2 class="text-3xl font-bold text-center mb-4">Built on triple trust</h2>
    <p class="text-gray-400 text-center mb-12 max-w-xl mx-auto">
      Every hire generates a tri-party contract that protects IP for everyone involved.
    </p>
    <div class="grid md:grid-cols-3 gap-6">
      {#each [
        { who: 'Professional', color: 'brand', points: ['You own your agent and persona', 'LoRA adapter is your IP', 'Export or delete any time', 'Tri-party contract protects you'] },
        { who: 'Company', color: 'green', points: ['You own the work output', 'Agent runs on our infra — data stays bounded', 'Full audit trail per job', 'Kill switch revokes access instantly'] },
        { who: 'Platform', color: 'purple', points: ['Escrow and billing', 'Infrastructure + security', '15% fee, paid from company side', 'Dispute resolution'] },
      ] as party}
        <div class="rounded-xl border border-gray-700 p-6">
          <h3 class="font-semibold text-lg mb-4 text-{party.color}-400">{party.who}</h3>
          <ul class="space-y-2">
            {#each party.points as pt}
              <li class="flex items-start gap-2 text-sm text-gray-300">
                <span class="text-{party.color}-400 mt-0.5">✓</span>
                {pt}
              </li>
            {/each}
          </ul>
        </div>
      {/each}
    </div>
  </div>
</section>

<!-- CTA -->
<section class="max-w-3xl mx-auto px-6 py-20 text-center">
  <h2 class="text-3xl font-bold mb-4">Ready to build your agent?</h2>
  <p class="text-gray-500 mb-8">Takes a weekend. Earns for years.</p>
  <button class="btn-primary px-10 py-3 text-base" on:click={() => goto('/auth/register?as=professional')}>
    Get started — it's free
  </button>
</section>
