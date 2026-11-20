<script lang="ts">
  // Skills are reusable procedural playbooks — a vault note with layer=skill. The professional
  // authors them here; the agent also self-authors them via the `write_skill` tool. On a hired
  // task, skills matching the task text are injected verbatim into the Execution Blueprint.
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { api } from '$lib/api/client';
  import { session } from '$lib/stores/session';
  import { get } from 'svelte/store';

  interface NoteMeta {
    id: string;
    slug: string;
    title: string;
    layer: string;
    visibility: string;
    domain: string | null;
  }
  interface NoteDetail extends NoteMeta {
    body: string;
  }

  let agentId = '';
  let skills: NoteMeta[] = [];
  let loading = true;
  let error = '';

  // Editor state. `editing` = the slug being edited (or '' for a new skill), null = list view.
  let editing: string | null = null;
  let form = { slug: '', title: '', when_to_use: '', body: '' };
  let saving = false;
  let isNew = false;

  const DOMAINS = ['', 'backend', 'frontend', 'infra', 'testing', 'product', 'general'];
  let domain = '';

  onMount(async () => {
    try {
      let id = get(session).agentId;
      if (!id) {
        const a = await api.get<{ id: string }>('/agents/mine');
        id = a.id;
        session.setIds({ agentId: id });
      }
      agentId = id!;
      await loadSkills();
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to load skills';
    } finally {
      loading = false;
    }
  });

  async function loadSkills() {
    const notes = await api.get<NoteMeta[]>(`/knowledge/agent/${agentId}/notes`);
    skills = notes.filter((n) => n.layer === 'skill').sort((a, b) => a.title.localeCompare(b.title));
  }

  // The stored body begins with "**When to use:** …" (matching the write_skill tool). Split it out
  // for the editor so the two concepts are edited separately, then recombine on save.
  function splitBody(body: string): { when: string; steps: string } {
    const m = body.match(/^\*\*When to use:\*\*\s*(.+?)\n\n([\s\S]*)$/);
    if (m) return { when: m[1].trim(), steps: m[2] };
    return { when: '', steps: body };
  }

  async function openSkill(slug: string) {
    error = '';
    try {
      const note = await api.get<NoteDetail>(`/knowledge/agent/${agentId}/note/${slug}`);
      const { when, steps } = splitBody(note.body);
      form = { slug: note.slug, title: note.title, when_to_use: when, body: steps };
      domain = note.domain ?? '';
      isNew = false;
      editing = slug;
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to open skill';
    }
  }

  function newSkill() {
    form = { slug: '', title: '', when_to_use: '', body: '' };
    domain = '';
    isNew = true;
    editing = '';
  }

  function slugify(s: string) {
    return s.toLowerCase().trim().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '');
  }

  async function saveSkill() {
    error = '';
    const slug = isNew ? slugify(form.slug || form.title) : form.slug;
    if (!slug) { error = 'A slug or title is required.'; return; }
    if (!form.body.trim()) { error = 'The playbook steps are required.'; return; }
    saving = true;
    try {
      const body = form.when_to_use.trim()
        ? `**When to use:** ${form.when_to_use.trim()}\n\n${form.body}`
        : form.body;
      await api.put(`/knowledge/agent/${agentId}/note/${slug}`, {
        title: form.title || slug,
        body,
        layer: 'skill',
        domain: domain || null,
        shareable: true,
      });
      await loadSkills();
      editing = null;
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to save skill';
    } finally {
      saving = false;
    }
  }

  async function deleteSkill(slug: string) {
    if (!confirm(`Delete skill "${slug}"? This can't be undone.`)) return;
    try {
      await api.delete(`/knowledge/agent/${agentId}/note/${slug}`);
      await loadSkills();
      if (editing === slug) editing = null;
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : 'Failed to delete skill';
    }
  }
</script>

<svelte:head><title>Skills — Flaborful</title></svelte:head>

<div class="max-w-4xl mx-auto px-4 py-8">
  <div class="flex items-start gap-4 mb-6">
    <div class="flex-1">
      <h1 class="text-2xl font-bold">Skills</h1>
      <p class="text-sand-500 text-sm mt-0.5">
        Reusable playbooks your agent follows. When a task matches a skill's trigger, its steps are
        loaded into the agent's brief — so it does the procedure your way, every time.
      </p>
    </div>
    {#if editing === null}
      <button class="btn-primary text-sm shrink-0" on:click={newSkill}>+ New skill</button>
    {:else}
      <button class="btn-ghost text-sm shrink-0" on:click={() => (editing = null)}>← Back to list</button>
    {/if}
  </div>

  {#if error}
    <div class="card p-4 mb-4 text-sm text-red-600 bg-red-50 border border-red-100">{error}</div>
  {/if}

  {#if loading}
    <div class="card p-8 animate-pulse"><div class="h-32 bg-sand-100 rounded"></div></div>
  {:else if editing !== null}
    <!-- Editor -->
    <div class="card p-6 space-y-4">
      <div class="grid sm:grid-cols-2 gap-4">
        <div>
          <label class="label text-xs" for="sk-title">Title</label>
          <input class="input text-sm" id="sk-title" placeholder="e.g. Add a database migration"
            bind:value={form.title} />
        </div>
        <div>
          <label class="label text-xs" for="sk-slug">Slug</label>
          <input class="input text-sm font-mono" id="sk-slug"
            placeholder={isNew ? 'auto from title' : ''}
            bind:value={form.slug} disabled={!isNew} />
          {#if !isNew}<p class="text-[11px] text-sand-400 mt-1">Slug can't change after creation.</p>{/if}
        </div>
      </div>

      <div>
        <label class="label text-xs" for="sk-when">When to use <span class="text-sand-400 font-normal">— the trigger that loads this skill</span></label>
        <input class="input text-sm" id="sk-when"
          placeholder="e.g. When adding or changing a Postgres migration"
          bind:value={form.when_to_use} />
      </div>

      <div>
        <label class="label text-xs" for="sk-domain">Domain</label>
        <select class="input text-sm" id="sk-domain" bind:value={domain}>
          {#each DOMAINS as d}<option value={d}>{d || '— none —'}</option>{/each}
        </select>
      </div>

      <div>
        <label class="label text-xs" for="sk-body">Playbook (numbered, prescriptive steps)</label>
        <textarea class="input text-sm min-h-[220px] font-mono text-xs" id="sk-body"
          placeholder={"1. …\n2. …\n3. …"}
          bind:value={form.body}></textarea>
        <p class="text-[11px] text-sand-400 mt-1">Markdown supported. Link related notes with [[slug]].</p>
      </div>

      <div class="flex gap-2 pt-1">
        <button class="btn-primary text-sm" disabled={saving} on:click={saveSkill}>
          {saving ? 'Saving…' : 'Save skill'}
        </button>
        {#if !isNew}
          <button class="btn-danger text-sm" on:click={() => deleteSkill(form.slug)}>Delete</button>
        {/if}
      </div>
    </div>
  {:else if skills.length === 0}
    <!-- Empty state -->
    <div class="card p-12 text-center max-w-xl mx-auto">
      <div class="text-4xl mb-4">📓</div>
      <h2 class="text-xl font-semibold mb-2">No skills yet</h2>
      <p class="text-sand-500 text-sm leading-relaxed mb-6">
        A skill is a step-by-step playbook for something you do a specific way — a PR checklist, how
        you structure a report, your migration process. Your agent loads the right one automatically
        when a task matches. It can also write its own after doing work well.
      </p>
      <button class="btn-primary" on:click={newSkill}>Write your first skill</button>
    </div>
  {:else}
    <!-- List -->
    <div class="card divide-y divide-sand-100">
      {#each skills as skill (skill.id)}
        <div class="px-5 py-4 flex items-center gap-4 hover:bg-sand-50 transition-colors">
          <button class="flex-1 min-w-0 text-left" on:click={() => openSkill(skill.slug)}>
            <div class="font-medium text-sm truncate">{skill.title}</div>
            <div class="text-xs text-sand-400 mt-0.5 font-mono truncate">{skill.slug}</div>
          </button>
          {#if skill.domain}<span class="badge-gray text-xs shrink-0">{skill.domain}</span>{/if}
          <button class="text-brand-600 hover:underline text-xs shrink-0" on:click={() => openSkill(skill.slug)}>Edit</button>
          <button class="text-red-400 hover:text-red-600 text-sm shrink-0" title="Delete" on:click={() => deleteSkill(skill.slug)}>×</button>
        </div>
      {/each}
    </div>
    <p class="text-xs text-sand-400 mt-3">
      Skills also appear in your <button class="text-brand-600 hover:underline" on:click={() => goto('/studio/knowledge')}>knowledge vault</button> graph, linked to the notes they reference.
    </p>
  {/if}
</div>
