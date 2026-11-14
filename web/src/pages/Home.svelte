<script lang="ts">
  import { hueOf, listPackages, session, type Package } from '../lib/registry.svelte';
  import { navigate } from '../lib/router.svelte';

  let packages = $state<Package[] | null>(null);
  let problem = $state('');
  let search = $state('');
  let type = $state('');

  // Reload when the visitor signs in or out: what they may see changes with it.
  $effect(() => {
    void session.email;
    packages = null;
    problem = '';
    listPackages()
      .then((found) => (packages = found))
      .catch((error: Error) => (problem = `The package list could not be loaded: ${error.message}`));
  });

  const types = $derived([...new Set((packages ?? []).map((entry) => entry.type))].sort());
  const needle = $derived(search.trim().toLowerCase());
  const shown = $derived(
    (packages ?? []).filter(
      (entry) =>
        (type === '' || entry.type === type) &&
        (needle === '' || entry.name.includes(needle) || entry.description.toLowerCase().includes(needle)),
    ),
  );

  function open(event: SubmitEvent) {
    event.preventDefault();
    const exact = shown.find((entry) => entry.name === needle) ?? (shown.length === 1 ? shown[0] : undefined);
    if (exact) {
      navigate(`/package/${exact.name}`);
    }
  }
</script>

<!-- The search field is the command you would type: finding a package and installing it
     are the same sentence. -->
<form class="command" onsubmit={open} role="search">
  <label for="search"><span class="prompt" aria-hidden="true">$</span> ctx install</label>
  <input
    id="search"
    type="search"
    bind:value={search}
    placeholder="package"
    autocomplete="off"
    autocapitalize="off"
    spellcheck="false"
    aria-label="Search packages by name or description"
  />
</form>

{#if types.length > 1}
  <div class="types" role="group" aria-label="Filter by package type">
    <button class="chip" class:on={type === ''} onclick={() => (type = '')}>All types</button>
    {#each types as entry (entry)}
      <button class="chip" class:on={type === entry} style:--hue={hueOf(entry)} onclick={() => (type = entry)}>
        <span class="dot" aria-hidden="true"></span>{entry}
      </button>
    {/each}
  </div>
{/if}

{#if problem}
  <p class="notice problem" role="alert">{problem}</p>
{:else if packages === null}
  <p class="muted">Loading packages…</p>
{:else if packages.length === 0}
  <div class="notice">
    {#if session.email}
      <p>No package is shared with {session.email} yet, and this registry has no public packages.</p>
      <p class="muted small">Ask a maintainer to run <code>ctx access grant &lt;package&gt; {session.email}</code>.</p>
    {:else}
      <p>This registry has no public packages. Sign in to see the packages shared with you.</p>
    {/if}
  </div>
{:else if shown.length === 0}
  <p class="notice">No package matches “{search.trim()}”{type ? ` in type ${type}` : ''}.</p>
{:else}
  <ul class="list">
    {#each shown as entry (entry.name)}
      <li style:--hue={hueOf(entry.type)}>
        <a href="/package/{entry.name}">
          <span class="name">{entry.name}</span>
          <span class="version">{entry.latest ?? 'no release yet'}</span>
          <span class="description muted">{entry.description || 'No description'}</span>
          <span class="meta small">
            <span class="kind">{entry.type}</span>
            <span class="muted">{entry.visibility}</span>
          </span>
        </a>
      </li>
    {/each}
  </ul>
  <p class="muted small">
    {shown.length} of {packages.length} package{packages.length === 1 ? '' : 's'}
    {session.email ? `visible to ${session.email}` : 'public. Sign in to see the ones shared with you'}.
  </p>
{/if}

<style>
  .command {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 0.2em 0.5em;
    font-family: var(--mono);
    font-size: var(--step-3);
    font-weight: 500;
    line-height: 1.2;
  }

  .command label {
    white-space: nowrap;
  }

  .prompt {
    color: var(--ink-soft);
    user-select: none;
  }

  .command input {
    flex: 1 1 8ch;
    min-width: 0;
    padding: 0.05em 0;
    border: 0;
    border-bottom: 3px solid var(--ink);
    border-radius: 0;
    background: none;
    font-weight: 500;
    outline-offset: 6px;
  }

  .command input::placeholder {
    color: var(--ink-soft);
    opacity: 0.6;
  }

  .command input::-webkit-search-cancel-button {
    display: none;
  }

  .types {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem;
  }

  .chip {
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
    padding: 0.2rem 0.7rem;
    border: 1px solid var(--line);
    border-radius: 999px;
    background: none;
    cursor: pointer;
    font-size: var(--step--1);
  }

  .chip.on {
    border-color: var(--ink);
    background: var(--ink);
    color: var(--paper);
  }

  .dot {
    width: 0.6rem;
    height: 0.6rem;
    border-radius: 2px;
    background: var(--hue);
  }

  .list {
    list-style: none;
    margin: 0;
    padding: 0;
    border-top: 1px solid var(--line);
  }

  .list li {
    border-bottom: 1px solid var(--line);
  }

  /* One row per package; the coloured edge is the package's type. */
  .list a {
    display: grid;
    grid-template-columns: minmax(12ch, 22ch) 9ch 1fr auto;
    align-items: baseline;
    gap: 0.25rem 1.25rem;
    padding: 0.85rem 0.75rem 0.85rem 1rem;
    border-left: 5px solid var(--hue);
    text-decoration: none;
  }

  .list a:hover {
    background: var(--surface);
  }

  .name {
    font-family: var(--display);
    font-weight: 650;
    font-size: var(--step-1);
    overflow-wrap: anywhere;
  }

  .version {
    font-family: var(--mono);
    font-size: var(--step--1);
  }

  .meta {
    display: flex;
    gap: 0.9rem;
    justify-content: flex-end;
    white-space: nowrap;
  }

  .kind {
    color: var(--hue);
    font-weight: 600;
  }

  @media (max-width: 46rem) {
    .list a {
      grid-template-columns: 1fr auto;
    }

    .description {
      grid-column: 1 / -1;
      grid-row: 2;
    }

    .meta {
      grid-column: 1 / -1;
      justify-content: flex-start;
    }
  }
</style>
