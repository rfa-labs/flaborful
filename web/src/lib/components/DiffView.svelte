<script lang="ts">
  // Colorized unified-diff renderer. The diff is already a unified-diff string (from the coding
  // agent / git) — we don't parse/recompute, just color each line by its leading marker. Files
  // are collapsible. No heavy diff library needed.
  export let diff: string;

  interface Line { kind: 'add' | 'del' | 'hunk' | 'file' | 'meta' | 'ctx'; text: string }
  interface FileBlock { header: string; lines: Line[] }

  function classify(line: string): Line['kind'] {
    if (line.startsWith('diff --git') || line.startsWith('+++') || line.startsWith('---')) return 'file';
    if (line.startsWith('@@')) return 'hunk';
    if (line.startsWith('+')) return 'add';
    if (line.startsWith('-')) return 'del';
    if (line.startsWith('index ') || line.startsWith('new file') || line.startsWith('deleted file') || line.startsWith('similarity ') || line.startsWith('rename ')) return 'meta';
    return 'ctx';
  }

  // Group into per-file blocks so each can collapse; non-diff preamble goes into a leading block.
  $: blocks = (() => {
    const out: FileBlock[] = [];
    let cur: FileBlock | null = null;
    for (const raw of (diff ?? '').split('\n')) {
      if (raw.startsWith('diff --git ')) {
        cur = { header: raw.replace('diff --git ', '').replace(/^a\//, '').split(' b/')[0], lines: [] };
        out.push(cur);
        continue;
      }
      // A bare `+++ b/<path>` (no `diff --git`) also starts a file.
      if (!cur && raw.startsWith('+++ ')) {
        cur = { header: raw.replace('+++ b/', '').replace('+++ ', ''), lines: [] };
        out.push(cur);
      }
      if (!cur) { cur = { header: 'changes', lines: [] }; out.push(cur); }
      cur.lines.push({ kind: classify(raw), text: raw });
    }
    return out;
  })();

  let collapsed: Record<number, boolean> = {};
  const lineClass: Record<Line['kind'], string> = {
    add: 'bg-green-50 text-green-800',
    del: 'bg-red-50 text-red-800',
    hunk: 'bg-blue-50 text-blue-600',
    file: 'text-sand-400',
    meta: 'text-sand-400',
    ctx: 'text-sand-700',
  };
</script>

{#if !diff?.trim()}
  <p class="text-xs text-sand-400">No changes.</p>
{:else}
  <div class="space-y-2">
    {#each blocks as block, i (i)}
      <div class="rounded border border-sand-200 overflow-hidden">
        <button class="w-full flex items-center justify-between px-2 py-1 bg-sand-100 text-left"
          on:click={() => collapsed = { ...collapsed, [i]: !collapsed[i] }}>
          <span class="text-xs font-mono font-semibold text-sand-700 truncate">{block.header}</span>
          <span class="text-[10px] text-sand-400">
            <span class="text-green-600">+{block.lines.filter((l) => l.kind === 'add').length}</span>
            <span class="text-red-600 ml-1">−{block.lines.filter((l) => l.kind === 'del').length}</span>
            <span class="ml-2">{collapsed[i] ? '▸' : '▾'}</span>
          </span>
        </button>
        {#if !collapsed[i]}
          <pre class="text-[11px] leading-snug overflow-x-auto"><code>{#each block.lines as l}<div class="px-2 {lineClass[l.kind]}">{l.text || ' '}</div>{/each}</code></pre>
        {/if}
      </div>
    {/each}
  </div>
{/if}
