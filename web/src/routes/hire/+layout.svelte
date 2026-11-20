<script lang="ts">
  // Route guard for the company side. Mirror of studio/+layout.svelte: logged-out visitors go
  // to login; professional accounts are bounced to their own portal.
  import { session, isLoggedIn } from '$lib/stores/session';
  import { goto } from '$app/navigation';
  import { page } from '$app/stores';

  $: {
    if (!$isLoggedIn) {
      goto('/auth/login?next=' + encodeURIComponent($page.url.pathname));
    } else if ($session.role !== 'company') {
      goto('/studio');
    }
  }

  $: allowed = $isLoggedIn && $session.role === 'company';
</script>

{#if allowed}
  <slot />
{/if}
