<script lang="ts">
  // Route guard for the professional side. The app is a pure client-rendered SPA
  // (see routes/+layout.ts: ssr=false), so this runs in the browser off the session store.
  // Logged-out visitors are sent to login; company accounts are bounced to their own portal.
  import { session, isLoggedIn } from '$lib/stores/session';
  import { goto } from '$app/navigation';
  import { page } from '$app/stores';

  // Reactive guard: also fires on a mid-session sign-out or role change, not just first mount.
  $: {
    if (!$isLoggedIn) {
      goto('/auth/login?next=' + encodeURIComponent($page.url.pathname));
    } else if ($session.role === 'company') {
      goto('/hire');
    }
  }

  // Only render children once the user is a confirmed professional, so gated pages don't flash
  // or fire authed fetches before the redirect lands.
  $: allowed = $isLoggedIn && $session.role !== 'company';
</script>

{#if allowed}
  <slot />
{/if}
