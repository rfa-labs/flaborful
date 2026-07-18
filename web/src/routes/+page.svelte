<script lang="ts">
  import { goto } from '$app/navigation';
  import { onMount } from 'svelte';
  import { session } from '$lib/stores/session';

  // The landing page speaks to both sides of the market. The header audience selector sets
  // session.portal; this page swaps its entire copy between the professional and company
  // variant off that value, reusing the exact same sections and styling.
  $: audience = $session.portal; // 'professional' | 'company'
  $: isCompany = audience === 'company';

  // The signature: the hero cycles through professions, each with the work that profession's
  // agent actually does. Breadth is demonstrated in the first three seconds instead of being
  // claimed in a footnote. Flaborful is for any profession an agent can do the work of —
  // engineers are just the vertical we seeded first.
  const professions = [
    { name: 'designer',   work: 'critiques the layout in your voice',        hue: 'var(--violet)' },
    { name: 'accountant', work: 'runs the month-end close the way you do',   hue: 'var(--teal)' },
    { name: 'engineer',   work: 'ships the change and opens the review',     hue: 'var(--amber)' },
    { name: 'marketer',   work: 'audits the funnel against your playbook',   hue: 'var(--rose)' },
    { name: 'lawyer',     work: 'redlines the contract to your standards',   hue: 'var(--violet)' },
    { name: 'analyst',    work: 'builds the model the way you would',        hue: 'var(--teal)' },
    { name: 'writer',     work: 'drafts it in your register, not a robot’s', hue: 'var(--amber)' },
  ];

  let i = 0;
  let paused = false;

  onMount(() => {
    // Respect reduced motion: settle on one profession rather than cycling.
    const rm = window.matchMedia('(prefers-reduced-motion: reduce)');
    if (rm.matches) return;
    const t = setInterval(() => {
      if (!paused) i = (i + 1) % professions.length;
    }, 2400);
    return () => clearInterval(t);
  });

  $: current = professions[i];

  const steps = [
    {
      k: 'It reads your work',
      d: 'Point it at your repos, docs, decks, and case files. It writes up how you actually work — the patterns you reach for, the shortcuts you refuse.',
    },
    {
      k: 'It learns your taste',
      d: 'Then it trains on that: a fine-tuned adapter that encodes your judgment and belongs to you. Export or delete it anytime.',
    },
    {
      k: 'It does the job',
      d: 'A company hires it. It works end to end and brings you the result. Nothing leaves without your say-so — and you get paid.',
    },
  ];

  // For professionals & freelancers — the concrete "what you get" grid.
  const pros = [
    {
      k: 'One agent, many clients',
      d: 'Put it to work for a dozen companies at once. Each pays modestly; it stacks into a living.',
    },
    {
      k: 'Bring your own book',
      d: 'Point the clients you already have here. They onboard and pay through the platform — you run it.',
    },
    {
      k: 'You stay the operator',
      d: 'It does the work and brings you the result. You approve it and put your name on it. Nothing ships without you.',
    },
  ];

  const terms = [
    { t: 'Your agent is yours.',          d: 'Trained on your work, held in your account. Take the weights and walk any day.' },
    { t: 'The work product is theirs.',   d: 'Like any contractor: whoever hires you owns what your agent delivers for them.' },
    { t: 'The experience comes home.',    d: 'The judgment your agent sharpens on a job comes back to you. The client’s secrets don’t.' },
    { t: 'Companies never bleed together.', d: 'One company’s data is walled off from every other company — and from every other agent.' },
    { t: 'Proprietary info doesn’t linger.', d: 'When a job ends, that company’s proprietary data leaves the knowledge base. The skill stays, not the secrets.' },
    { t: 'You are the gate.',             d: 'Nothing ships, sends, or files without your approval. The agent proposes; you decide.' },
  ];

  // ── Company variant ─────────────────────────────────────────────────────────
  // How hiring works from the company's seat.
  const companySteps = [
    {
      k: 'Find your contractor',
      d: 'Browse by profession and pick the expert whose agentic workforce fits your need — an accountant, an engineer, a designer, each a real human fronting their agents.',
    },
    {
      k: 'Hand over the work',
      d: 'Attach the context — a composable knowledge vault scoped to this job — and the agent gets to work. Frontier-quality output, a fraction of the cost of the human alone.',
    },
    {
      k: 'A real human gates it',
      d: 'The expert who trained the agent reviews, gates, and discusses the work with you. You’re not talking to a bot — you’re working with a professional who stands behind it.',
    },
  ];

  // How a company judges which agent to hire.
  const companyChoose = [
    {
      k: 'The real résumé',
      d: 'See the professional behind the agent — their actual background and credentials, not a marketing bio.',
    },
    {
      k: 'The knowledge graph',
      d: 'Browse the agent’s knowledge vault as a graph. Nodes may be locked out — labels hidden at the professional’s choice — so you can gauge depth without exposing their secrets.',
    },
    {
      k: 'The track record',
      d: 'Jobs completed, outputs approved, work delivered. Hire on a record of getting it done, not a promise.',
    },
  ];

  const companyTerms = [
    { t: 'A human stands behind it.',     d: 'Every agent is operated and gated by the real expert who trained it. You hire the human and their workforce, together.' },
    { t: 'Frontier quality, contractor price.', d: 'Expert-grade work on every task, for a fraction of what the professional alone would cost.' },
    { t: 'Vaults attach, then detach.',   d: 'Give the agent a knowledge vault scoped to your job. When the work is done, it detaches — your proprietary data doesn’t linger.' },
    { t: 'Your data never bleeds.',       d: 'What you share is walled off from every other company and every other agent on the platform.' },
    { t: 'You own the work product.',     d: 'Like any contractor engagement: whatever the agent delivers for you is yours to keep.' },
    { t: 'Real people, real conversation.', d: 'Gating and discussing the work happens with the expert, not a chatbot. Human judgment sits on top of the agentic labor.' },
  ];

  // Active copy for the current audience.
  $: activeSteps  = isCompany ? companySteps  : steps;
  $: activeGrid   = isCompany ? companyChoose : pros;
  $: activeTerms  = isCompany ? companyTerms  : terms;
</script>

<svelte:head>
  <title>Flaborful — Own the agent they built from your work</title>
  <meta
    name="description"
    content="They trained a model on your work, then made the team smaller. Train your own instead: an agent that works in your style, that you own, and that earns for you."
  />
  <!-- Fonts (Bricolage Grotesque + Public Sans) are loaded app-wide in app.css now; just warm
       up the connection here. -->
  <link rel="preconnect" href="https://fonts.googleapis.com" />
  <link rel="preconnect" href="https://fonts.gstatic.com" crossorigin="anonymous" />
</svelte:head>

<div class="mkt">
  <!-- ambient warmth: two soft light sources, not a flat background -->
  <div class="glow glow-a" aria-hidden="true"></div>
  <div class="glow glow-b" aria-hidden="true"></div>

  <section class="hero">
    <p class="pill">
      <span class="spark" aria-hidden="true"></span>
      {isCompany ? 'Experts, backed by an agentic workforce' : 'A worker-owned agent platform'}
    </p>

    {#if isCompany}
      <h1>
        Hire the expert.<br />
        Their agents <em>do the work.</em>
      </h1>
    {:else}
      <h1>
        Own the agent<br />
        they built <em>from you.</em>
      </h1>
    {/if}

    <!-- Signature: the profession cycler. -->
    <div
      class="cycler"
      on:mouseenter={() => (paused = true)}
      on:mouseleave={() => (paused = false)}
      role="presentation"
    >
      <span class="c-lead">{isCompany ? 'An' : 'Your'}</span>
      <span class="c-slot">
        {#key current.name}
          <span class="c-word" style="--hue:{current.hue}">{current.name}</span>
        {/key}
      </span>
      <span class="c-lead">agent</span>
      <span class="c-slot c-slot-wide">
        {#key current.name}
          <span class="c-work">{current.work}.</span>
        {/key}
      </span>
    </div>

    {#if isCompany}
      <p class="lede">
        Hire an accounting agent and you’re hiring an accountant — a real expert who operates and
        gates an AI workforce. Frontier-quality work on every task, for a fraction of what the
        professional alone would cost.
      </p>
    {:else}
      <p class="lede">
        They trained a model on your work, then made the team smaller.
        Train your own instead: an agentic workforce that works in your style, that you own, and that earns for you.
      </p>
    {/if}

    <div class="cta">
      {#if isCompany}
        <button class="b-main" on:click={() => goto('/marketplace')}>
          Find your contractor
          <span class="arr" aria-hidden="true">→</span>
        </button>
      {:else}
        <button class="b-main" on:click={() => goto('/auth/register?as=professional')}>
          Claim your agent
          <span class="arr" aria-hidden="true">→</span>
        </button>
      {/if}
      <a class="b-alt" href="#how">See how it works</a>
    </div>
  </section>

  <!-- the argument -->
  <section class="argue">
    <div class="argue-card">
      {#if isCompany}
        <h2>Find the contractor for the job.</h2>
        <p>
          The marketplace is where you find the right expert for a specific need — where the
          “contractor” is an agentic workforce, trained and gated by a real human professional in
          their field. You get their expertise at machine scale, without giving up the human.
        </p>
        <p>
          Because a person stands behind every agent, there’s real conversation when it counts:
          the expert gates the output, talks through the task, and answers for the work. Not a
          tool you have to babysit — a professional you can actually hire.
        </p>
      {:else}
        <h2>Your work is the pitch.</h2>
        <p>
          You don’t sell a business on a résumé anymore — you sell them on an agent that already
          works the way you do. It applies, it delivers, and the delivery is the proof.
        </p>
        <p>
          Freelancing already? Point the clients you have at Flaborful — they onboard, hand your
          agent the work, and pay right here while you run it. One professional, many companies at
          once: each job modest, and it stacks. We’re building the supply side in the open — so
          grow the marketplace from the inside.
        </p>
      {/if}
    </div>
  </section>

  <!-- how -->
  <section class="how" id="how">
    <h2 class="sec">{isCompany ? 'How hiring works' : 'How your agent gets built'}</h2>
    <div class="steps">
      {#each activeSteps as s, n}
        <article class="step" style="--n:{n}">
          <span class="num">{n + 1}</span>
          <h3>{s.k}</h3>
          <p>{s.d}</p>
        </article>
      {/each}
    </div>
  </section>

  <!-- audience-specific grid: pros' economics OR how a company chooses -->
  <section class="pros">
    {#if isCompany}
      <h2 class="sec">How to choose an agent</h2>
      <p class="sec-sub">
        Judge on what’s real: the person, their proof, and their record.
      </p>
    {:else}
      <h2 class="sec">Built for the way you already work</h2>
      <p class="sec-sub">
        An agentic workforce changes the math of freelancing.
      </p>
    {/if}
    <div class="steps">
      {#each activeGrid as p, n}
        <article class="step" style="--n:{n}">
          <h3>{p.k}</h3>
          <p>{p.d}</p>
        </article>
      {/each}
    </div>
  </section>

  <!-- marketplace / composable vaults -->
  <section class="market">
    <div class="market-card">
      {#if isCompany}
        <h2>Context that attaches, then lets go.</h2>
        <p>
          Every agent carries a knowledge vault — the professional’s expertise, as a graph you can
          browse before you hire. For your job, hand it a composable vault scoped to the work:
          your context, temporarily attached for exactly as long as it’s needed.
        </p>
        <p>
          When the job ends, that vault detaches. The skill the agent gained stays with the
          expert; your proprietary information leaves. You get the depth of a specialist without
          your data living anywhere it shouldn’t.
        </p>
      {:else}
        <h2>A marketplace of professions, not prompts.</h2>
        <p>
          A directory where every profession is a real expert fronting an agent trained on how they
          work. A company or local business shows up with a problem and finds not a tool to figure
          out, but a professional whose agent just does the job.
        </p>
        <p>
          Breadth is the pull: the more professions on the board, the more businesses come looking.
          Early on it grows fastest from the inside — professionals bringing the clients they
          already have.
        </p>
      {/if}
    </div>
  </section>

  <!-- terms -->
  <section class="terms">
    {#if isCompany}
      <h2 class="sec">What you’re actually hiring</h2>
      <p class="sec-sub">
        An expert and their agentic workforce — with the guarantees a contractor engagement
        should have. Nothing buried in a terms page.
      </p>
    {:else}
      <h2 class="sec">Data trust, structured like a contractor</h2>
      <p class="sec-sub">
        Your agent works for a company the way you would — the work product is theirs, the
        experience is yours. The whole boundary, nothing buried in a terms page.
      </p>
    {/if}
    <div class="tgrid">
      {#each activeTerms as t, n}
        <div class="term" style="--n:{n}">
          <span class="tmark" aria-hidden="true"></span>
          <h3>{t.t}</h3>
          <p>{t.d}</p>
        </div>
      {/each}
    </div>
  </section>

  <section class="end">
    {#if isCompany}
      <h2>Real expertise. Agentic scale.</h2>
      <p class="end-sub">Find your contractor.</p>
      <button class="b-main lg" on:click={() => goto('/marketplace')}>
        Browse the marketplace
        <span class="arr" aria-hidden="true">→</span>
      </button>
      <p class="fine">See the experts and their agents. No account needed to look.</p>
    {:else}
      <h2>Your work already trained somebody’s model.</h2>
      <p class="end-sub">Make the next one yours.</p>
      <button class="b-main lg" on:click={() => goto('/auth/register?as=professional')}>
        Claim your agent
        <span class="arr" aria-hidden="true">→</span>
      </button>
      <p class="fine">Free while we build the professional side. No card.</p>
    {/if}
  </section>
</div>

<style>
  /* Scoped to this page. The app's shared design system (brand-*, .btn, .card) is used by 25
     other files across the studio/hire dashboards and is deliberately left alone — this is a
     marketing surface with its own identity, not a restyle of the product. */
  .mkt {
    /* Warm and luminous. Deliberately not Tailwind's stock indigo (#6366f1), which is what made
       the original read as a template. */
    --ink: #1c1524;
    --ink-2: #5b5167;
    --cream: #fdf8f3;
    --shell: #f6ecf9;

    --amber: #f0863a;
    --rose: #e0457b;
    --violet: #7c4dff;
    --teal: #12a594;

    --display: 'Bricolage Grotesque', ui-sans-serif, system-ui, sans-serif;
    --body: 'Public Sans', ui-sans-serif, system-ui, sans-serif;

    position: relative;
    isolation: isolate;
    overflow: clip; /* contains the glows without clipping page scroll */
    background:
      radial-gradient(120% 80% at 50% 0%, var(--shell) 0%, var(--cream) 55%, var(--cream) 100%);
    color: var(--ink);
    font-family: var(--body);
  }

  /* Two soft light sources give the page depth instead of a flat fill. */
  .glow {
    position: absolute;
    z-index: -1;
    border-radius: 50%;
    filter: blur(80px);
    pointer-events: none;
  }
  .glow-a {
    top: -14rem; left: -6rem;
    width: 42rem; height: 32rem;
    background: radial-gradient(circle, rgba(124, 77, 255, 0.3), transparent 68%);
  }
  .glow-b {
    top: -8rem; right: -10rem;
    width: 40rem; height: 30rem;
    background: radial-gradient(circle, rgba(240, 134, 58, 0.32), transparent 68%);
  }

  section { max-width: 70rem; margin: 0 auto; padding-left: 1.5rem; padding-right: 1.5rem; }

  /* ── hero ─────────────────────────────────────────────── */
  .hero { padding-top: 6rem; padding-bottom: 5rem; max-width: 60rem; }

  .pill {
    display: inline-flex; align-items: center; gap: 0.5rem;
    background: rgba(255, 255, 255, 0.75);
    border: 1px solid rgba(124, 77, 255, 0.18);
    box-shadow: 0 2px 14px rgba(124, 77, 255, 0.09);
    backdrop-filter: blur(8px);
    border-radius: 999px;
    padding: 0.42rem 0.95rem 0.42rem 0.6rem;
    font-size: 0.78rem; font-weight: 500; color: var(--ink-2);
    margin: 0 0 2rem;
  }
  .spark {
    width: 0.5rem; height: 0.5rem; border-radius: 50%;
    background: linear-gradient(135deg, var(--amber), var(--rose));
    box-shadow: 0 0 0 3px rgba(224, 69, 123, 0.14);
  }

  h1 {
    font-family: var(--display);
    font-weight: 800;
    font-size: clamp(2.7rem, 6.6vw, 5.1rem);
    line-height: 1.02;
    letter-spacing: -0.033em;
    margin: 0 0 1.6rem;
  }
  h1 em {
    font-style: normal;
    background: linear-gradient(105deg, var(--violet) 0%, var(--rose) 48%, var(--amber) 100%);
    -webkit-background-clip: text;
    background-clip: text;
    color: transparent;
  }

  /* the cycler */
  .cycler {
    display: flex; flex-wrap: wrap; align-items: center; gap: 0.5rem;
    font-family: var(--display);
    font-size: clamp(1.05rem, 2.1vw, 1.45rem);
    font-weight: 600;
    color: var(--ink-2);
    margin: 0 0 2rem;
    min-height: 2.6rem;
  }
  .c-lead { color: var(--ink-2); }
  .c-slot { position: relative; display: inline-flex; align-items: center; }

  .c-word {
    display: inline-block;
    color: #fff;
    background: var(--hue);
    border-radius: 0.6rem;
    padding: 0.16em 0.55em;
    box-shadow: 0 6px 20px -6px var(--hue);
    animation: swap 380ms cubic-bezier(0.2, 0.9, 0.25, 1);
  }
  .c-work {
    display: inline-block;
    color: var(--ink-2);
    font-weight: 500;
    animation: swap 380ms cubic-bezier(0.2, 0.9, 0.25, 1) 60ms both;
  }
  @keyframes swap {
    from { opacity: 0; transform: translateY(0.5em) rotate(-1.5deg); filter: blur(3px); }
    to   { opacity: 1; transform: none; filter: none; }
  }

  .lede {
    font-size: 1.13rem; line-height: 1.68; color: var(--ink-2);
    max-width: 38rem; margin: 0 0 2.4rem;
  }

  .cta { display: flex; flex-wrap: wrap; align-items: center; gap: 1.1rem; }

  .b-main {
    display: inline-flex; align-items: center; gap: 0.55rem;
    font-family: var(--body); font-size: 0.98rem; font-weight: 600;
    color: #fff; border: 0; cursor: pointer;
    padding: 0.95rem 1.6rem;
    border-radius: 0.85rem;
    background: linear-gradient(120deg, var(--violet), var(--rose) 60%, var(--amber));
    background-size: 160% 100%;
    box-shadow: 0 10px 30px -10px rgba(124, 77, 255, 0.65),
                inset 0 1px 0 rgba(255, 255, 255, 0.28);
    transition: background-position 400ms ease, transform 160ms ease, box-shadow 200ms ease;
  }
  .b-main:hover {
    background-position: 100% 0;
    transform: translateY(-2px);
    box-shadow: 0 16px 38px -12px rgba(224, 69, 123, 0.6),
                inset 0 1px 0 rgba(255, 255, 255, 0.35);
  }
  .b-main .arr { transition: transform 200ms ease; }
  .b-main:hover .arr { transform: translateX(3px); }
  .b-main.lg { padding: 1.05rem 2rem; font-size: 1.03rem; }

  .b-alt {
    font-size: 0.95rem; font-weight: 500; color: var(--ink);
    text-decoration: none;
    padding: 0.9rem 1.2rem; border-radius: 0.85rem;
    background: rgba(255, 255, 255, 0.7);
    border: 1px solid rgba(28, 21, 36, 0.1);
    transition: background 160ms ease, transform 160ms ease;
  }
  .b-alt:hover { background: #fff; transform: translateY(-1px); }

  /* ── argument ─────────────────────────────────────────── */
  .argue { padding-bottom: 5rem; }
  .argue-card {
    background: linear-gradient(135deg, #2a1f38 0%, #3d2547 55%, #4a2740 100%);
    color: #f7f0fa;
    border-radius: 1.5rem;
    padding: 3.2rem 3rem;
    box-shadow: 0 30px 60px -30px rgba(42, 31, 56, 0.7);
    max-width: 52rem;
  }
  .argue-card h2 {
    font-family: var(--display); font-weight: 700;
    font-size: clamp(1.6rem, 3.2vw, 2.3rem);
    letter-spacing: -0.02em;
    line-height: 1.15;
    margin: 0 0 1.2rem;
  }
  .argue-card p {
    font-size: 1rem; line-height: 1.72;
    color: rgba(247, 240, 250, 0.76);
    margin: 0 0 1rem; max-width: 40rem;
  }
  .argue-card p:last-child { margin-bottom: 0; }

  /* ── how ──────────────────────────────────────────────── */
  .sec {
    font-family: var(--display); font-weight: 700;
    font-size: clamp(1.7rem, 3.4vw, 2.4rem);
    letter-spacing: -0.022em;
    margin: 0 0 2.2rem;
  }
  .how, .pros { padding-bottom: 5rem; }
  .steps { display: grid; grid-template-columns: repeat(3, 1fr); gap: 1.4rem; }

  /* ── marketplace ──────────────────────────────────────── */
  /* A warm, light counterpart to the dark argue card, so the two narrative bands read as
     different beats rather than a repeat. */
  .market { padding-bottom: 5rem; }
  .market-card {
    background:
      radial-gradient(120% 130% at 100% 0%, rgba(240, 134, 58, 0.16), transparent 60%),
      linear-gradient(135deg, #fffaf4, #fdf1f6 60%, #f6eefc);
    border: 1px solid rgba(124, 77, 255, 0.14);
    border-radius: 1.5rem;
    padding: 3.2rem 3rem;
    box-shadow: 0 30px 60px -34px rgba(124, 77, 255, 0.4);
    max-width: 52rem;
  }
  .market-card h2 {
    font-family: var(--display); font-weight: 700;
    font-size: clamp(1.6rem, 3.2vw, 2.3rem);
    letter-spacing: -0.02em; line-height: 1.15;
    margin: 0 0 1.2rem;
    background: linear-gradient(105deg, var(--violet), var(--rose) 55%, var(--amber));
    -webkit-background-clip: text; background-clip: text; color: transparent;
  }
  .market-card p {
    font-size: 1rem; line-height: 1.72; color: var(--ink-2);
    margin: 0 0 1rem; max-width: 42rem;
  }
  .market-card p:last-child { margin-bottom: 0; }

  .step {
    background: rgba(255, 255, 255, 0.78);
    backdrop-filter: blur(6px);
    border: 1px solid rgba(28, 21, 36, 0.07);
    border-radius: 1.2rem;
    padding: 1.9rem 1.6rem;
    box-shadow: 0 14px 36px -22px rgba(28, 21, 36, 0.4);
    transition: transform 220ms ease, box-shadow 220ms ease;
  }
  .step:hover {
    transform: translateY(-4px);
    box-shadow: 0 24px 50px -24px rgba(124, 77, 255, 0.45);
  }
  .num {
    display: inline-grid; place-items: center;
    width: 2.1rem; height: 2.1rem; border-radius: 0.7rem;
    font-family: var(--display); font-weight: 700; font-size: 0.95rem;
    color: #fff; margin-bottom: 1rem;
    background: linear-gradient(135deg, var(--violet), var(--rose));
    box-shadow: 0 6px 18px -6px rgba(124, 77, 255, 0.7);
  }
  .step:nth-child(2) .num { background: linear-gradient(135deg, var(--rose), var(--amber)); box-shadow: 0 6px 18px -6px rgba(224, 69, 123, 0.7); }
  .step:nth-child(3) .num { background: linear-gradient(135deg, var(--amber), var(--teal)); box-shadow: 0 6px 18px -6px rgba(240, 134, 58, 0.7); }

  .step h3 {
    font-family: var(--display); font-weight: 700; font-size: 1.16rem;
    letter-spacing: -0.012em; margin: 0 0 0.55rem;
  }
  .step p { font-size: 0.93rem; line-height: 1.66; color: var(--ink-2); margin: 0; }

  /* ── terms ────────────────────────────────────────────── */
  .terms { padding-bottom: 5rem; }
  .sec-sub {
    font-size: 1rem; line-height: 1.65; color: var(--ink-2);
    max-width: 34rem; margin: -1.4rem 0 2.2rem;
  }
  .tgrid { display: grid; grid-template-columns: 1fr 1fr; gap: 1.4rem; }

  /* These were bare text on the background while every neighbouring section was a card, which
     made the most-asked question read as an afterthought. Give them the same soft depth, with a
     colour-coded edge so the boundaries are visually distinct. */
  .term {
    position: relative;
    background: rgba(255, 255, 255, 0.78);
    backdrop-filter: blur(6px);
    border: 1px solid rgba(28, 21, 36, 0.07);
    border-radius: 1.1rem;
    padding: 1.6rem 1.5rem 1.6rem 1.8rem;
    box-shadow: 0 14px 36px -24px rgba(28, 21, 36, 0.38);
    overflow: hidden;
  }
  .tmark {
    position: absolute; left: 0; top: 0; bottom: 0; width: 4px;
    background: linear-gradient(180deg, var(--violet), var(--rose));
  }
  .term:nth-child(2) .tmark { background: linear-gradient(180deg, var(--rose), var(--amber)); }
  .term:nth-child(3) .tmark { background: linear-gradient(180deg, var(--amber), var(--teal)); }
  .term:nth-child(4) .tmark { background: linear-gradient(180deg, var(--teal), var(--violet)); }
  .term:nth-child(5) .tmark { background: linear-gradient(180deg, var(--violet), var(--amber)); }
  .term:nth-child(6) .tmark { background: linear-gradient(180deg, var(--amber), var(--rose)); }

  .term h3 {
    font-family: var(--display); font-weight: 700; font-size: 1.12rem;
    margin: 0 0 0.45rem; letter-spacing: -0.012em;
  }
  .term p { font-size: 0.94rem; line-height: 1.68; color: var(--ink-2); margin: 0; }

  /* ── end ──────────────────────────────────────────────── */
  .end { text-align: center; padding-top: 2rem; padding-bottom: 7rem; }
  .end h2 {
    font-family: var(--display); font-weight: 800;
    font-size: clamp(1.9rem, 4.4vw, 3.1rem);
    letter-spacing: -0.028em; line-height: 1.1;
    margin: 0 0 0.6rem;
  }
  .end-sub {
    font-family: var(--display); font-weight: 700;
    font-size: clamp(1.4rem, 3.2vw, 2.2rem);
    letter-spacing: -0.025em;
    margin: 0 0 2rem;
    background: linear-gradient(105deg, var(--violet), var(--rose) 55%, var(--amber));
    -webkit-background-clip: text; background-clip: text; color: transparent;
  }
  .fine { font-size: 0.82rem; color: var(--ink-2); margin: 1.2rem 0 0; }

  /* ── responsive ───────────────────────────────────────── */
  @media (max-width: 880px) {
    .steps { grid-template-columns: 1fr; }
    .tgrid { grid-template-columns: 1fr; }
    .argue-card, .market-card { padding: 2.2rem 1.5rem; border-radius: 1.2rem; }
    .hero { padding-top: 4rem; }
    .cycler { font-size: 1.02rem; }
  }

  @media (prefers-reduced-motion: reduce) {
    .c-word, .c-work { animation: none; }
    .b-main, .b-alt, .step { transition: none; }
    .b-main:hover, .b-alt:hover, .step:hover { transform: none; }
  }

  :global(.mkt button:focus-visible),
  :global(.mkt a:focus-visible) {
    outline: 2px solid var(--violet);
    outline-offset: 3px;
  }
</style>
