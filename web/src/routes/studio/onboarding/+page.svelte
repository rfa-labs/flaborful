<script lang="ts">
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { page } from '$app/stores';
  import { api } from '$lib/api/client';
  import { session } from '$lib/stores/session';
  import IngestionProgress from '$lib/components/IngestionProgress.svelte';

  let step = 1; // 1=Identity, 2=Knowledge, 3=Interview
  let submitting = false;
  let error = '';
  let hasExistingAgent = false; // true when returning user already has an agent

  // Step 1 — Identity
  let name = '';
  let bio = '';
  let hourlyRate = 150;
  let yearsExp = 5;
  let resumeText = '';
  // True when an existing user is editing their profile (not first-run). createAgent() then
  // upserts the professional and returns to studio instead of creating a new agent + advancing.
  let editingProfile = false;

  // Step 2 — Knowledge (GitHub OAuth)
  let agentId = ''; // set after agent creation
  let githubConnected = false;
  let githubLogin = '';
  let ingestDone = false;
  let ingestResult: { repos_analyzed: number; style_guide: Record<string, string[]> } | null = null;
  // The deep repo→vault documentation runs in the background after the ingest call returns. Track
  // its completion (via the live IngestionProgress feed) so we can gate "Start interview" — LoRA
  // trains from the vault, so it should be built first.
  let ingestionComplete = false;

  onMount(() => {
    const params = $page.url.searchParams;

    // Resume after GitHub OAuth redirect (new user mid-onboarding) — check this FIRST,
    // before the returning-user branch, because the callback page sets session.agentId
    // before navigating here. Without this ordering, a fresh user returning from GitHub
    // sees "Retrain your agent" instead of the ingest/style-guide flow.
    if (params.get('github_connected') === '1') {
      const returnedAgentId = params.get('agent_id');
      const login = params.get('github_login');
      if (returnedAgentId) {
        agentId = returnedAgentId;
        session.setIds({ agentId: returnedAgentId });
      }
      if (login) githubLogin = decodeURIComponent(login);
      githubConnected = true;
      step = 2;
      triggerIngest();
      return;
    }

    // Resume after Google OAuth redirect — Google is an optional capability connect,
    // not part of the knowledge flow, so just return the owner to their studio.
    if (params.get('google_connected') === '1') {
      goto('/studio');
      return;
    }

    // Returning user — already has an agent, skip the full flow.
    if ($session.agentId) {
      agentId = $session.agentId;
      hasExistingAgent = true;
      // Detect an existing GitHub connection so the GitHub step can offer re-ingestion directly
      // (instead of asking the returning user to re-connect).
      api.get<Array<{ provider: string; external_account: string }>>('/auth/connections/mine')
        .then((conns) => {
          const gh = conns.find((c) => c.provider === 'github');
          if (gh) { githubConnected = true; githubLogin = gh.external_account; ingestDone = true; }
        })
        .catch(() => { /* non-blocking */ });
      return;
    }
  });

  async function retrain() {
    submitting = true; error = '';
    try {
      await api.post(`/agents/${agentId}/retrain`, {});
      goto('/studio');
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to start training';
    } finally {
      submitting = false;
    }
  }

  // Step 3 — Slim, open-form interview (communication & reasoning style → style-rule vault notes).
  interface OpenQuestion { id: string; aspect_slug?: string; prompt: string }
  let questions: OpenQuestion[] = [];
  let answers: Record<string, string> = {};
  let interviewDone = false;

  // ── Step 1: create (first-run) or update (edit) professional ───────────────
  async function createAgent() {
    submitting = true; error = '';
    try {
      // POST /professionals is an upsert keyed on user_id — so this both creates (first run) and
      // edits (returning user) the basic info without a separate endpoint.
      const prof = await api.post<{ id: string }>('/professionals', {
        profession_id: 'a0000000-0000-0000-0000-000000000001', // Software Engineer
        name, bio,
        hourly_rate: hourlyRate * 100,
        years_experience: yearsExp,
      });

      if (editingProfile && agentId) {
        // Existing user editing their profile — don't create a duplicate agent or advance the
        // wizard. Save resume too, then return to studio.
        session.setIds({ professionalId: prof.id });
        if (resumeText.trim()) {
          try { await api.post(`/knowledge/agent/${agentId}/resume`, { resume_text: resumeText.trim() }); } catch { /* non-blocking */ }
        }
        goto('/studio');
        return;
      }

      // First run: create the agent and continue the wizard.
      const agent = await api.post<{ id: string }>('/agents', { professional_id: prof.id });
      agentId = agent.id;
      session.setIds({ professionalId: prof.id, agentId: agent.id });
      if (resumeText.trim()) {
        try { await api.post(`/knowledge/agent/${agent.id}/resume`, { resume_text: resumeText.trim() }); } catch { /* non-blocking */ }
      }
      step = 2;
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to save profile';
    } finally {
      submitting = false;
    }
  }

  // Prefill the identity form from the existing professional (for the edit path).
  async function loadProfileForEdit() {
    try {
      const p = await api.get<{ name: string; bio: string; hourly_rate: number; years_experience: number }>('/professionals/mine');
      name = p.name ?? '';
      bio = p.bio ?? '';
      hourlyRate = Math.round((p.hourly_rate ?? 15000) / 100);
      yearsExp = p.years_experience ?? 5;
    } catch { /* leave defaults */ }
  }

  // ── Step 2: GitHub OAuth + ingestion ──────────────────────────────────────
  async function connectGitHub() {
    submitting = true; error = '';
    try {
      const res = await api.get<{ url: string }>(`/auth/github/start?agent_id=${agentId}`);
      window.location.href = res.url;
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to start GitHub OAuth';
      submitting = false;
    }
  }

  // force=false (default): repos already in the vault are skipped — a re-run only fills in repos
  // that haven't been documented yet. force=true: re-document everything from scratch.
  async function triggerIngest(force = false) {
    submitting = true; error = '';
    try {
      ingestResult = await api.post<typeof ingestResult>('/onboarding/github/ingest', {
        agent_id: agentId,
        force,
      });
      ingestDone = true;
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'GitHub ingestion failed';
    } finally {
      submitting = false;
    }
  }

  // Load the slim, open-form interview: a few high-signal questions about communication &
  // reasoning style. One small Ollama call (5 short prompts), not the old 20-scenario battery.
  async function loadInterview() {
    submitting = true; error = '';
    try {
      const res = await api.get<{ questions: OpenQuestion[] }>('/onboarding/interview/questions/Software%20Engineer');
      questions = res.questions ?? [];
      answers = Object.fromEntries(questions.map(q => [q.id, '']));
      step = 3;
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to load interview';
    } finally {
      submitting = false;
    }
  }

  async function submitInterview() {
    submitting = true; error = '';
    try {
      await api.post('/onboarding/interview/open/submit', {
        agent_id: agentId,
        answers: questions
          .map(q => ({ question_id: q.id, prompt: q.prompt, answer: (answers[q.id] ?? '').trim() }))
          .filter(a => a.answer.length > 0),
      });
      interviewDone = true;
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to submit interview';
    } finally {
      submitting = false;
    }
  }

  // At least one answered question required to submit.
  $: answeredCount = questions.filter(q => (answers[q.id] ?? '').trim().length > 0).length;
</script>

<svelte:head><title>Build your agent — Flaborful</title></svelte:head>

<div class="max-w-3xl mx-auto px-4 py-10">

  {#if hasExistingAgent}
    <div class="card p-8">
      <h1 class="text-2xl font-bold mb-1">Enrich your agent</h1>
      <p class="text-gray-500 mb-8">
        Your agent is already set up. Improve any part independently — none of these interfere with
        the others, and nothing trains until you choose to. Train your agent from the
        <a href="/studio" class="text-brand-600 hover:underline">Studio dashboard</a> when you're ready.
      </p>

      <div class="space-y-3 mb-8">
        <button class="w-full text-left p-4 rounded-lg border-2 border-gray-200 hover:border-gray-300"
          on:click={() => goto('/studio/knowledge')}>
          <div class="font-semibold text-gray-800">📚 Knowledge vault</div>
          <div class="text-sm text-gray-500 mt-0.5">Browse your notes (list or graph), edit your resume.</div>
        </button>
        <button class="w-full text-left p-4 rounded-lg border-2 border-gray-200 hover:border-gray-300"
          on:click={() => { hasExistingAgent = false; step = 2; }}>
          <div class="font-semibold text-gray-800">🐙 GitHub knowledge</div>
          <div class="text-sm text-gray-500 mt-0.5">Re-run GitHub ingestion to document more repos into your vault.</div>
        </button>
        <button class="w-full text-left p-4 rounded-lg border-2 border-gray-200 hover:border-gray-300"
          on:click={() => { hasExistingAgent = false; step = 3; loadInterview(); }}>
          <div class="font-semibold text-gray-800">🎤 Style interview</div>
          <div class="text-sm text-gray-500 mt-0.5">Answer more open questions to capture your communication & reasoning style.</div>
        </button>
        <button class="w-full text-left p-4 rounded-lg border-2 border-gray-200 hover:border-gray-300"
          on:click={() => { editingProfile = true; loadProfileForEdit(); hasExistingAgent = false; step = 1; }}>
          <div class="font-semibold text-gray-800">👤 Profile & basic info</div>
          <div class="text-sm text-gray-500 mt-0.5">Update your name, bio, rate, and experience.</div>
        </button>
      </div>

      {#if error}<div class="text-red-600 text-sm bg-red-50 rounded-lg px-3 py-2">{error}</div>{/if}

      <div class="flex justify-between items-center">
        <a href="/studio" class="btn-ghost text-sm">← Back to studio</a>
      </div>
    </div>

  {:else}

  <!-- Steps indicator -->
  <div class="flex items-center gap-2 mb-10">
    {#each [
      { n: 1, label: 'Identity' },
      { n: 2, label: 'Knowledge' },
      { n: 3, label: 'Interview' },
    ] as s}
      <div class="flex items-center gap-2 {s.n < 3 ? 'flex-1' : ''}">
        <div class="w-8 h-8 rounded-full flex items-center justify-center text-sm font-bold shrink-0
          {step === s.n ? 'bg-brand-600 text-white' : step > s.n ? 'bg-green-500 text-white' : 'bg-gray-200 text-gray-500'}">
          {step > s.n ? '✓' : s.n}
        </div>
        <span class="text-sm font-medium {step === s.n ? 'text-gray-900' : 'text-gray-400'}">{s.label}</span>
        {#if s.n < 3}
          <div class="flex-1 h-px {step > s.n ? 'bg-green-400' : 'bg-gray-200'} mx-2"></div>
        {/if}
      </div>
    {/each}
  </div>

  <!-- ── Step 1: Identity ─────────────────────────────────────────── -->
  {#if step === 1}
    <div class="card p-8">
      <h1 class="text-2xl font-bold mb-1">Your professional identity</h1>
      <p class="text-gray-500 mb-6">This becomes the public face of your agent on the marketplace.</p>

      <div class="space-y-5">
        <div>
          <label class="label" for="name">Full name</label>
          <input class="input" id="name" bind:value={name} placeholder="Rashad Arafeh" required />
        </div>
        <div>
          <label class="label" for="bio">Professional bio <span class="text-gray-400 font-normal">(write in your own voice)</span></label>
          <textarea class="input min-h-[100px] resize-y" id="bio" bind:value={bio}
            placeholder="I'm a software engineer with 8 years building distributed systems. I care deeply about code clarity and pragmatic architecture..."></textarea>
        </div>
        <div class="grid grid-cols-2 gap-4">
          <div>
            <label class="label" for="rate">Hourly rate ($/hr)</label>
            <input class="input" id="rate" type="number" bind:value={hourlyRate} min="10" max="1000" />
          </div>
          <div>
            <label class="label" for="exp">Years of experience</label>
            <input class="input" id="exp" type="number" bind:value={yearsExp} min="0" max="50" />
          </div>
        </div>
        <div>
          <label class="label" for="resume">Resume <span class="text-gray-400 font-normal">(paste text — used on job applications and added to your knowledge base)</span></label>
          <textarea class="input min-h-[120px] resize-y text-sm" id="resume" bind:value={resumeText}
            placeholder="Paste your resume here. You can edit it later in your Knowledge vault."></textarea>
        </div>
      </div>

      {#if error}<div class="text-red-600 text-sm bg-red-50 rounded-lg px-3 py-2 mt-4">{error}</div>{/if}

      <div class="mt-8 flex justify-end">
        <button class="btn-primary px-8" disabled={!name || !bio || submitting} on:click={createAgent}>
          {submitting ? 'Saving...' : editingProfile ? 'Save profile' : 'Continue →'}
        </button>
      </div>
    </div>

  <!-- ── Step 2: Knowledge ─────────────────────────────────────────── -->
  {:else if step === 2}
    <div class="card p-8">
      <h1 class="text-2xl font-bold mb-1">Knowledge ingestion</h1>
      <p class="text-gray-500 mb-6">Connect your GitHub to extract your professional style guide. We'll read your public and private repos.</p>

      {#if !ingestDone}
        <div class="space-y-4">
          {#if !githubConnected}
            <button class="btn-primary w-full py-3 flex items-center justify-center gap-2" disabled={submitting} on:click={connectGitHub}>
              <svg class="w-5 h-5" viewBox="0 0 24 24" fill="currentColor"><path d="M12 0C5.37 0 0 5.37 0 12c0 5.31 3.435 9.795 8.205 11.385.6.105.825-.255.825-.57 0-.285-.015-1.23-.015-2.235-3.015.555-3.795-.735-4.035-1.41-.135-.345-.72-1.41-1.23-1.695-.42-.225-1.02-.78-.015-.795.945-.015 1.62.87 1.845 1.23 1.08 1.815 2.805 1.305 3.495.99.105-.78.42-1.305.765-1.605-2.67-.3-5.46-1.335-5.46-5.925 0-1.305.465-2.385 1.23-3.225-.12-.3-.54-1.53.12-3.18 0 0 1.005-.315 3.3 1.23.96-.27 1.98-.405 3-.405s2.04.135 3 .405c2.295-1.56 3.3-1.23 3.3-1.23.66 1.65.24 2.88.12 3.18.765.84 1.23 1.905 1.23 3.225 0 4.605-2.805 5.625-5.475 5.925.435.375.81 1.095.81 2.22 0 1.605-.015 2.895-.015 3.3 0 .315.225.69.825.57A12.02 12.02 0 0 0 24 12c0-6.63-5.37-12-12-12z"/></svg>
              {submitting ? 'Redirecting to GitHub...' : 'Connect GitHub'}
            </button>
          {:else}
            <div class="flex items-center gap-3 bg-gray-50 rounded-lg px-4 py-3">
              <svg class="w-5 h-5 text-gray-700 shrink-0" viewBox="0 0 24 24" fill="currentColor"><path d="M12 0C5.37 0 0 5.37 0 12c0 5.31 3.435 9.795 8.205 11.385.6.105.825-.255.825-.57 0-.285-.015-1.23-.015-2.235-3.015.555-3.795-.735-4.035-1.41-.135-.345-.72-1.41-1.23-1.695-.42-.225-1.02-.78-.015-.795.945-.015 1.62.87 1.845 1.23 1.08 1.815 2.805 1.305 3.495.99.105-.78.42-1.305.765-1.605-2.67-.3-5.46-1.335-5.46-5.925 0-1.305.465-2.385 1.23-3.225-.12-.3-.54-1.53.12-3.18 0 0 1.005-.315 3.3 1.23.96-.27 1.98-.405 3-.405s2.04.135 3 .405c2.295-1.56 3.3-1.23 3.3-1.23.66 1.65.24 2.88.12 3.18.765.84 1.23 1.905 1.23 3.225 0 4.605-2.805 5.625-5.475 5.925.435.375.81 1.095.81 2.22 0 1.605-.015 2.895-.015 3.3 0 .315.225.69.825.57A12.02 12.02 0 0 0 24 12c0-6.63-5.37-12-12-12z"/></svg>
              <span class="text-sm text-gray-700">@{githubLogin}</span>
              <span class="ml-auto text-xs text-brand-600 animate-pulse">{submitting ? 'Analyzing repos...' : ''}</span>
            </div>
          {/if}

          {#if githubConnected && !submitting}
            <button class="btn-primary w-full py-3" on:click={() => triggerIngest(false)}>
              Build knowledge vault from my repos →
            </button>
          {/if}

          {#if submitting && githubConnected}
            <div class="bg-brand-50 rounded-lg p-4 text-sm text-brand-700 animate-pulse">
              Fetching repos and extracting your style guide — this takes ~30 seconds...
            </div>
          {/if}
        </div>
      {:else if ingestResult}
        <div class="bg-green-50 rounded-lg p-4 mb-6">
          <div class="font-medium text-green-800 mb-1">✓ Style guide extracted</div>
          <div class="text-sm text-green-700">{ingestResult.repos_analyzed} repos analyzed</div>
        </div>

        {#if ingestResult.style_guide}
          <div class="space-y-3 mb-6">
            {#each Object.entries(ingestResult.style_guide) as [key, values]}
              <div>
                <div class="text-xs font-semibold text-gray-500 uppercase tracking-wide mb-1.5">
                  {key.replace(/_/g, ' ')}
                </div>
                <div class="flex flex-wrap gap-1.5">
                  {#each (Array.isArray(values) ? values : [values]) as v}
                    <span class="badge-blue text-xs">{v}</span>
                  {/each}
                </div>
              </div>
            {/each}
          </div>
        {/if}

        <!-- Live progress of the agentic repo→vault documentation (the deep knowledge base). -->
        <div class="border-t border-gray-100 pt-5 mt-5">
          <IngestionProgress {agentId} on:complete={() => (ingestionComplete = true)} />
          <div class="flex items-center gap-3 mt-3">
            <button class="btn-ghost text-sm" disabled={submitting} on:click={() => triggerIngest(false)}>
              ↻ Build missing repos
            </button>
            <button class="btn-ghost text-sm text-gray-400" disabled={submitting} on:click={() => triggerIngest(true)}>
              Re-do all from scratch
            </button>
          </div>
        </div>
      {/if}

      {#if error}<div class="text-red-600 text-sm bg-red-50 rounded-lg px-3 py-2 mt-4">{error}</div>{/if}

      <div class="mt-8 flex items-center justify-between">
        <button class="btn-ghost" on:click={() => step = 1}>← Back</button>
        <div class="flex items-center gap-3">
          {#if ingestDone && !ingestionComplete}
            <span class="text-xs text-gray-400">Vault still building — you can proceed, but training is best after it finishes.</span>
          {/if}
          <button class="btn-primary px-8" disabled={!ingestDone || submitting} on:click={loadInterview}>
            {submitting ? 'Loading...' : ingestionComplete ? 'Start interview →' : 'Continue anyway →'}
          </button>
        </div>
      </div>
    </div>

  <!-- ── Step 3: AI Interview ──────────────────────────────────────── -->
  {:else if step === 3}
    {#if interviewDone}
      <div class="card p-12 text-center">
        <div class="text-6xl mb-4">✅</div>
        <h2 class="text-2xl font-bold mb-2">Knowledge base ready</h2>
        <p class="text-gray-500 mb-2">We've captured your communication & reasoning style from {answeredCount} question{answeredCount === 1 ? '' : 's'}, added to your knowledge vault.</p>
        <p class="text-gray-400 text-sm mb-8">Nothing trains automatically. When you're happy with your knowledge base, click <strong>Train agent</strong> on the Studio dashboard to build your LoRA — you can keep enriching it (more repos, more interview answers) first.</p>
        <div class="flex gap-3 justify-center">
          <button class="btn-secondary" on:click={() => goto('/studio/knowledge')}>
            View knowledge vault
          </button>
          <button class="btn-primary px-8" on:click={() => goto('/studio')}>
            Go to Studio →
          </button>
        </div>
      </div>
    {:else}
      <div class="card p-8">
        <div class="mb-2">
          <h1 class="text-xl font-bold">Style interview</h1>
          <p class="text-sm text-gray-500 mt-1">
            A few open questions about how you communicate and reason — the part your code can't show.
            Answer in your own voice; a couple of sentences each is plenty. You can skip any.
          </p>
        </div>

        <div class="space-y-6 mt-6">
          {#each questions as q, i}
            <div>
              <label class="label" for={`q-${q.id}`}>
                <span class="text-brand-600 font-semibold mr-1">{i + 1}.</span>{q.prompt}
              </label>
              <textarea
                class="input min-h-[90px] resize-y"
                id={`q-${q.id}`}
                bind:value={answers[q.id]}
                placeholder="In my experience…"
              ></textarea>
            </div>
          {:else}
            <p class="text-sm text-gray-400">No questions loaded.</p>
          {/each}
        </div>

        {#if error}<div class="text-red-600 text-sm bg-red-50 rounded-lg px-3 py-2 mt-4">{error}</div>{/if}

        <div class="flex items-center justify-between mt-8">
          <span class="text-xs text-gray-400">{answeredCount} of {questions.length} answered</span>
          <button
            class="btn-primary px-8"
            disabled={answeredCount === 0 || submitting}
            on:click={submitInterview}>
            {submitting ? 'Submitting…' : 'Finish interview →'}
          </button>
        </div>
      </div>
    {/if}
  {/if}

  {/if}<!-- end hasExistingAgent -->
</div>
