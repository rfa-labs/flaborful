<script lang="ts">
  import '../app.css';
  import { session, isLoggedIn, type Portal } from '$lib/stores/session';
  import { page } from '$app/stores';
  import { goto } from '$app/navigation';

  function switchPortal(p: Portal) {
    session.setPortal(p);
    goto(p === 'professional' ? '/studio' : '/hire');
  }

  function signOut() {
    session.clear();
    goto('/');
  }

  $: isStudio = $page.url.pathname.startsWith('/studio');
  $: isHire   = $page.url.pathname.startsWith('/hire');
  $: isMkt    = $page.url.pathname.startsWith('/marketplace');
</script>

<div class="min-h-screen flex flex-col">
  <!-- Top nav -->
  <header class="bg-white border-b border-gray-200 sticky top-0 z-40">
    <div class="max-w-7xl mx-auto px-4 sm:px-6 lg:px-8 h-14 flex items-center justify-between gap-4">
      <!-- Wordmark -->
      <a href="/" class="text-lg font-bold text-brand-600 tracking-tight shrink-0">
        flaborful
      </a>

      <!-- Portal toggle (guests only) -->
      {#if !$isLoggedIn}
        <div class="flex items-center bg-gray-100 rounded-lg p-0.5 text-sm font-medium gap-0.5">
          <button
            class="px-3 py-1.5 rounded-md transition-colors {$session.portal === 'professional' ? 'bg-white shadow text-gray-900' : 'text-gray-500 hover:text-gray-700'}"
            on:click={() => switchPortal('professional')}>
            Agent Studio
          </button>
          <button
            class="px-3 py-1.5 rounded-md transition-colors {$session.portal === 'company' ? 'bg-white shadow text-gray-900' : 'text-gray-500 hover:text-gray-700'}"
            on:click={() => switchPortal('company')}>
            Hire Desk
          </button>
        </div>
      {/if}

      <!-- Nav links -->
      <nav class="hidden md:flex items-center gap-1 text-sm">
        <a href="/marketplace"
           class="px-3 py-1.5 rounded-md transition-colors {isMkt ? 'bg-brand-50 text-brand-700 font-medium' : 'text-gray-600 hover:bg-gray-100'}">
          Marketplace
        </a>
        {#if $isLoggedIn}
          {#if $session.role === 'company'}
            <a href="/hire"
               class="px-3 py-1.5 rounded-md transition-colors {isHire ? 'bg-brand-50 text-brand-700 font-medium' : 'text-gray-600 hover:bg-gray-100'}">
              Hire Desk
            </a>
          {:else}
            <a href="/studio"
               class="px-3 py-1.5 rounded-md transition-colors {isStudio ? 'bg-brand-50 text-brand-700 font-medium' : 'text-gray-600 hover:bg-gray-100'}">
              My Studio
            </a>
          {/if}
        {:else if $session.portal === 'professional'}
          <a href="/studio"
             class="px-3 py-1.5 rounded-md transition-colors {isStudio ? 'bg-brand-50 text-brand-700 font-medium' : 'text-gray-600 hover:bg-gray-100'}">
            My Studio
          </a>
        {:else}
          <a href="/hire"
             class="px-3 py-1.5 rounded-md transition-colors {isHire ? 'bg-brand-50 text-brand-700 font-medium' : 'text-gray-600 hover:bg-gray-100'}">
            Hire Desk
          </a>
        {/if}
      </nav>

      <div class="flex items-center gap-2">
        {#if $isLoggedIn}
          <span class="text-xs text-gray-500 capitalize">{$session.role}</span>
          <button on:click={signOut} class="btn-secondary text-xs">Sign out</button>
        {:else}
          <a href="/auth/login" class="btn-secondary text-xs">Sign in</a>
          <a href="/auth/register" class="btn-primary text-xs">Get started</a>
        {/if}
      </div>
    </div>
  </header>

  <!-- Page content -->
  <main class="flex-1">
    <slot />
  </main>
</div>
