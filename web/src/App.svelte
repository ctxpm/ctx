<script lang="ts">
  import { session, signIn, signOut } from './lib/registry.svelte';
  import { interceptLinks, router } from './lib/router.svelte';
  import Home from './pages/Home.svelte';
  import Package from './pages/Package.svelte';

  let signInProblem = $state('');

  async function startSignIn() {
    signInProblem = '';
    try {
      await signIn();
    } catch (error) {
      const code = (error as { code?: string }).code ?? '';
      // Closing the pop-up is a decision, not a failure.
      if (!code.includes('popup-closed') && !code.includes('cancelled')) {
        signInProblem = `Sign-in did not work: ${(error as Error).message}`;
      }
    }
  }
</script>

<svelte:document onclick={interceptLinks} />

<header class="bar">
  <a class="mark" href="/" aria-label="{session.registryName}, all packages">
    <span class="strata" aria-hidden="true"><i></i><i></i><i></i></span>
    <span>{session.registryName}</span>
  </a>
  <div class="who">
    {#if !session.ready}
      <span class="muted small">Connecting…</span>
    {:else if session.email}
      <span class="muted small email">{session.email}</span>
      <button class="button plain" onclick={signOut}>Sign out</button>
    {:else if !session.failure}
      <button class="button" onclick={startSignIn}>Sign in with Google</button>
    {/if}
  </div>
</header>

<main>
  {#if signInProblem}
    <p class="notice problem" role="alert">{signInProblem}</p>
  {/if}

  {#if session.failure}
    <p class="notice problem" role="alert">{session.failure}</p>
  {:else if !session.ready}
    <p class="muted">Connecting to the registry…</p>
  {:else if router.route.page === 'home'}
    <Home />
  {:else if router.route.page === 'package'}
    {#key `${router.route.name}:${session.email}`}
      <Package name={router.route.name} />
    {/key}
  {:else}
    <h1>There is no page here</h1>
    <p><a href="/">See all packages</a></p>
  {/if}
</main>

<footer class="muted small">
  <span>Install packages with the <code>ctx</code> command-line tool.</span>
  <a href="https://github.com/ctxpm/ctx">Source and documentation</a>
</footer>

<style>
  .bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
    padding: 0.9rem var(--gutter);
    border-bottom: 1px solid var(--line);
  }

  .mark {
    display: inline-flex;
    align-items: center;
    gap: 0.6rem;
    font-family: var(--display);
    font-weight: 650;
    font-size: var(--step-1);
    text-decoration: none;
  }

  /* Three stacked layers: the same picture a package's dependencies are drawn with. */
  .strata {
    display: grid;
    gap: 2px;
    width: 1.35rem;
  }

  .strata i {
    display: block;
    height: 0.3rem;
    border-radius: 1px;
  }

  .strata i:nth-child(1) {
    background: var(--stratum-1);
  }

  .strata i:nth-child(2) {
    background: var(--stratum-2);
  }

  .strata i:nth-child(3) {
    background: var(--stratum-3);
  }

  .who {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    min-width: 0;
  }

  .email {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  main {
    max-width: 72rem;
    margin: 0 auto;
    padding: clamp(1.5rem, 1rem + 3vw, 3.5rem) var(--gutter) 4rem;
    display: grid;
    gap: 1.5rem;
  }

  footer {
    display: flex;
    flex-wrap: wrap;
    justify-content: space-between;
    gap: 0.5rem 2rem;
    max-width: 72rem;
    margin: 0 auto;
    padding: 1.25rem var(--gutter) 2.5rem;
    border-top: 1px solid var(--line);
  }

  @media (max-width: 32rem) {
    .email {
      display: none;
    }
  }
</style>
