<script lang="ts">
  // Renders one manifest field against a node's config object. Handles text/textarea/number/
  // boolean/enum/key_value/repeater, and calls `onChange` with the new value for its key.
  import type { FieldSpec } from '$lib/api/nodeManifests';
  import Self from './FieldRenderer.svelte';

  export let field: FieldSpec;
  export let config: Record<string, unknown>;
  export let onChange: (key: string, value: unknown) => void;

  // key_value + repeater edit array values in place, then bubble the whole array up.
  type KV = { key: string; value: string };

  $: kvRows = (Array.isArray(config[field.key]) ? (config[field.key] as KV[]) : []) as KV[];
  $: repeaterRows = (Array.isArray(config[field.key])
    ? (config[field.key] as Record<string, unknown>[])
    : []) as Record<string, unknown>[];

  function setKv(rows: KV[]) {
    onChange(field.key, rows);
  }
  function addKv() {
    setKv([...kvRows, { key: '', value: '' }]);
  }
  function updateKv(i: number, patch: Partial<KV>) {
    setKv(kvRows.map((r, idx) => (idx === i ? { ...r, ...patch } : r)));
  }
  function removeKv(i: number) {
    setKv(kvRows.filter((_, idx) => idx !== i));
  }

  function setRepeater(rows: Record<string, unknown>[]) {
    onChange(field.key, rows);
  }
  function addRow() {
    setRepeater([...repeaterRows, {}]);
  }
  function updateRow(i: number, key: string, value: unknown) {
    setRepeater(repeaterRows.map((r, idx) => (idx === i ? { ...r, [key]: value } : r)));
  }
  function removeRow(i: number) {
    setRepeater(repeaterRows.filter((_, idx) => idx !== i));
  }

  const str = (v: unknown) => (v === undefined || v === null ? '' : String(v));
</script>

<div>
  {#if field.type === 'boolean'}
    <label class="flex items-start gap-2 text-xs text-sand-600 cursor-pointer">
      <input
        type="checkbox"
        class="mt-0.5"
        checked={Boolean(config[field.key])}
        on:change={(e) => onChange(field.key, e.currentTarget.checked)} />
      <span>{field.label}{#if field.required}<span class="text-rose-500"> *</span>{/if}</span>
    </label>
    {#if field.help}<p class="text-[11px] text-sand-400 mt-1 ml-6">{field.help}</p>{/if}
  {:else}
    <div class="flex items-center gap-1">
      <label class="label text-xs" for="cfg-{field.key}">
        {field.label}{#if field.required}<span class="text-rose-500"> *</span>{/if}
      </label>
      {#if field.help}
        <span class="text-sand-300 cursor-help text-xs" title={field.help}>ⓘ</span>
      {/if}
    </div>

    {#if field.type === 'textarea'}
      <textarea
        class="input text-sm min-h-[70px] resize-none font-mono text-xs"
        id="cfg-{field.key}"
        placeholder={field.placeholder}
        value={str(config[field.key])}
        on:input={(e) => onChange(field.key, e.currentTarget.value)} />
    {:else if field.type === 'enum'}
      <select
        class="input text-sm"
        id="cfg-{field.key}"
        value={str(config[field.key])}
        on:change={(e) => onChange(field.key, e.currentTarget.value)}>
        <option value="">— select —</option>
        {#each field.options ?? [] as opt}
          <option value={opt}>{opt}</option>
        {/each}
      </select>
    {:else if field.type === 'number'}
      <input
        class="input text-sm"
        type="number"
        id="cfg-{field.key}"
        min={field.min}
        max={field.max}
        step={field.min !== undefined && field.max !== undefined ? 'any' : undefined}
        placeholder={field.placeholder}
        value={str(config[field.key])}
        on:input={(e) => onChange(field.key, e.currentTarget.value)} />
    {:else if field.type === 'key_value'}
      <div class="space-y-1.5">
        {#each kvRows as row, i}
          <div class="flex items-center gap-1">
            <input class="input text-xs flex-1" placeholder="Key" value={row.key}
              on:input={(e) => updateKv(i, { key: e.currentTarget.value })} />
            <input class="input text-xs flex-1" placeholder="Value" value={row.value}
              on:input={(e) => updateKv(i, { value: e.currentTarget.value })} />
            <button class="text-rose-400 hover:text-rose-600 text-sm px-1" on:click={() => removeKv(i)}>×</button>
          </div>
        {/each}
        <button class="text-xs text-brand-600 hover:underline" on:click={addKv}>+ Add pair</button>
      </div>
    {:else if field.type === 'repeater'}
      <div class="space-y-2">
        {#each repeaterRows as row, i}
          <div class="rounded-lg border border-sand-200 p-2 space-y-2 relative">
            <button
              class="absolute top-1 right-1 text-rose-400 hover:text-rose-600 text-sm leading-none"
              title="Remove row" on:click={() => removeRow(i)}>×</button>
            {#each field.item ?? [] as sub}
              <Self
                field={sub}
                config={row}
                onChange={(k, v) => updateRow(i, k, v)} />
            {/each}
          </div>
        {/each}
        <button class="text-xs text-brand-600 hover:underline" on:click={addRow}>+ Add {field.label.toLowerCase()}</button>
      </div>
    {:else}
      <input
        class="input text-sm"
        id="cfg-{field.key}"
        placeholder={field.placeholder}
        value={str(config[field.key])}
        on:input={(e) => onChange(field.key, e.currentTarget.value)} />
    {/if}
  {/if}
</div>
