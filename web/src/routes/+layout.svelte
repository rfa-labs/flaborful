<script lang="ts">
  import '../app.css';
  import { session, isLoggedIn, type Portal } from '$lib/stores/session';
  import { page } from '$app/stores';
  import { goto } from '$app/navigation';

  // Guest audience selector: only sets which side the marketing framing + primary CTA speak to.
  // It must NOT navigate into the gated /studio or /hire routes (guests can't enter those).
  function chooseAudience(p: Portal) {
    session.setPortal(p);
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
  <header class="bg-white/85 backdrop-blur border-b border-sand-200 sticky top-0 z-40">
    <div class="max-w-7xl mx-auto px-4 sm:px-6 lg:px-8 h-14 flex items-center justify-between gap-4">
      <!-- Wordmark: the gradient treatment, permanent (the landing page no longer needs to
           override the nav — the base nav already matches). -->
      <a href="/" class="font-display text-xl font-extrabold tracking-tight shrink-0 text-transparent bg-clip-text"
         style="background-image:linear-gradient(105deg,#7c4dff,#e0457b 60%,#f0863a)">
        flaborful
      </a>

      <!-- Audience selector (guests only). Steers the marketing framing + the primary CTA;
           it does not link into the gated /studio or /hire routes. Hidden on phones: at 390px
           the toggle + "Sign in" + "Get started" total 399px, which pushed "Get started" off
           the right edge on every page for logged-out mobile visitors. -->
      {#if !$isLoggedIn}
        <div class="hidden sm:flex items-center bg-sand-100 rounded-lg p-0.5 text-sm font-medium gap-0.5">
          <button
            class="px-3 py-1.5 rounded-md transition-colors {$session.portal === 'professional' ? 'bg-white shadow-sm text-sand-900' : 'text-sand-500 hover:text-sand-700'}"
            on:click={() => chooseAudience('professional')}>
            For professionals
          </button>
          <button
            class="px-3 py-1.5 rounded-md transition-colors {$session.portal === 'company' ? 'bg-white shadow-sm text-sand-900' : 'text-sand-500 hover:text-sand-700'}"
            on:click={() => chooseAudience('company')}>
            For companies
          </button>
        </div>
      {/if}

      <!-- Nav links -->
      <nav class="hidden md:flex items-center gap-1 text-sm">
        <a href="/marketplace"
           class="px-3 py-1.5 rounded-md transition-colors {isMkt ? 'bg-brand-50 text-brand-700 font-medium' : 'text-sand-600 hover:bg-sand-100'}">
          Marketplace
        </a>
        <!-- Gated destinations appear only for the matching signed-in role. Guests get no link
             into /studio or /hire — those routes require an account. -->
        {#if $isLoggedIn}
          {#if $session.role === 'company'}
            <a href="/hire"
               class="px-3 py-1.5 rounded-md transition-colors {isHire ? 'bg-brand-50 text-brand-700 font-medium' : 'text-sand-600 hover:bg-sand-100'}">
              Hire Desk
            </a>
          {:else}
            <a href="/studio"
               class="px-3 py-1.5 rounded-md transition-colors {isStudio ? 'bg-brand-50 text-brand-700 font-medium' : 'text-sand-600 hover:bg-sand-100'}">
              My Studio
            </a>
          {/if}
        {/if}
      </nav>

      <div class="flex items-center gap-2">
        {#if $isLoggedIn}
          <span class="text-xs text-sand-500 capitalize">{$session.role}</span>
          <button on:click={signOut} class="btn-secondary text-xs">Sign out</button>
        {:else}
          <a href="/auth/login" class="btn-secondary text-xs">Sign in</a>
          <a href="/auth/register?as={$session.portal}" class="btn-primary text-xs">Get started</a>
        {/if}
      </div>
    </div>
  </header>

  <!-- Page content -->
  <main class="flex-1">
    <slot />
  </main>
</div>
