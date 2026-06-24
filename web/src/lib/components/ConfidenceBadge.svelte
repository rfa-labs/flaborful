<script lang="ts">
  export let score: number | null;
  export let size: 'sm' | 'md' = 'sm';

  $: cls = score == null
    ? 'badge-gray'
    : score >= 0.9 ? 'badge-green'
    : score >= 0.7 ? 'badge-yellow'
    : 'badge-red';

  $: label = score == null ? '—' : `${Math.round(score * 100)}%`;
</script>

<span class="{cls} {size === 'md' ? 'text-sm px-3 py-1' : ''}">{label}</span>
