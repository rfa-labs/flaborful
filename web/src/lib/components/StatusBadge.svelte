<script lang="ts">
  export let status: string;

  const map: Record<string, string> = {
    onboarding: 'badge-gray',
    training:   'badge-yellow',
    ready:      'badge-green',
    deployed:   'badge-blue',
    suspended:  'badge-red',
    draft:      'badge-gray',
    open:       'badge-blue',
    trial:      'badge-yellow',
    active:     'badge-green',
    completed:  'badge-gray',
    cancelled:  'badge-red',
    pending:         'badge-gray',
    running:         'badge-blue',
    awaiting_review: 'badge-yellow',
    approved:        'badge-green',
    amended:         'badge-purple',
    rejected:        'badge-red',
    delivered:       'badge-green',
    failed:          'badge-red',
  };

  $: cls = map[status] ?? 'badge-gray';
  $: label = status.replace(/_/g, ' ');
</script>

<span class={cls}>{label}</span>
