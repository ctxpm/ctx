<script lang="ts">
  import { formatBytes, formatDate, renderMarkdown } from '../lib/markdown';
  import {
    getPackage,
    getVersions,
    hueOf,
    session,
    setYanked,
    updateAccess,
    type AccessChange,
    type Package,
    type Version,
  } from '../lib/registry.svelte';

  let { name }: { name: string } = $props();

  let pkg = $state<Package | null>(null);
  let versions = $state<Version[]>([]);
  let loading = $state(true);
  let problem = $state('');
  let chosen = $state<string | null>(null);
  let copied = $state(false);
  let saving = $state(false);
  let grant = $state('');
  let trust = $state('');
  let dependencyTypes = $state<Record<string, string>>({});

  async function load() {
    loading = true;
    try {
      pkg = await getPackage(name);
      versions = pkg ? await getVersions(name) : [];
    } catch (error) {
      problem = `This package could not be loaded: ${(error as Error).message}`;
    }
    loading = false;
  }

  void load();

  const shown = $derived(
    versions.find((entry) => entry.version === chosen) ??
      versions.find((entry) => entry.version === pkg?.latest) ??
      versions[0] ??
      null,
  );
  const isLatest = $derived(shown !== null && shown.version === pkg?.latest);
  const command = $derived(`ctx install ${name}${shown && !isLatest ? `@${shown.version}` : ''}`);
  const readme = $derived(shown?.readme ? renderMarkdown(shown.readme) : '');
  const dependencies = $derived(Object.entries(shown?.dependencies ?? {}));
  const maintainer = $derived(session.email !== null && (pkg?.maintainers.includes(session.email) ?? false));

  // Colour each dependency layer by its own type, when the visitor may see that package.
  $effect(() => {
    for (const [dependency] of dependencies) {
      if (!(dependency in dependencyTypes)) {
        dependencyTypes[dependency] = '';
        void getPackage(dependency).then((found) => (dependencyTypes[dependency] = found?.type ?? '?'));
      }
    }
  });

  async function copy() {
    await navigator.clipboard.writeText(command);
    copied = true;
    setTimeout(() => (copied = false), 1800);
  }

  async function change(update: AccessChange) {
    if (!pkg) {
      return;
    }
    saving = true;
    problem = '';
    try {
      await updateAccess(pkg.name, update);
      pkg = { ...pkg, ...update };
    } catch (error) {
      problem = `The change was not saved: ${(error as Error).message}`;
    }
    saving = false;
  }

  function addReader(event: SubmitEvent) {
    event.preventDefault();
    if (!pkg) {
      return;
    }
    const entry = grant.trim().toLowerCase();
    if (entry.startsWith('@') && entry.length > 3) {
      void change({ readerDomains: [...new Set([...pkg.readerDomains, entry.slice(1)])] });
    } else if (/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(entry)) {
      void change({ readers: [...new Set([...pkg.readers, entry])] });
    } else {
      problem = 'Enter an email address, or a domain written as @example.org.';
      return;
    }
    grant = '';
  }

  function addPublisher(event: SubmitEvent) {
    event.preventDefault();
    if (!pkg) {
      return;
    }
    const repository = trust.trim().toLowerCase();
    if (!/^[a-z0-9_.-]+\/[a-z0-9_.-]+$/.test(repository)) {
      problem = 'Enter a GitHub repository as owner/repo.';
      return;
    }
    if (!pkg.trustedPublishers.some((entry) => entry.repository === repository)) {
      void change({ trustedPublishers: [...pkg.trustedPublishers, { provider: 'github', repository }] });
    }
    trust = '';
  }

  async function yank(version: Version) {
    saving = true;
    problem = '';
    try {
      await setYanked(name, version.version, !version.yanked);
      await load();
    } catch (error) {
      problem = `The version was not changed: ${(error as Error).message}`;
    }
    saving = false;
  }
</script>

{#if loading && !pkg}
  <p class="muted">Loading {name}…</p>
{:else if !pkg}
  <h1>{name} is not available</h1>
  <p class="notice">
    {#if session.email}
      Either this package does not exist, or it is not shared with {session.email}. The registry does not say which.
    {:else}
      Either this package does not exist, or you need to sign in to see it.
    {/if}
  </p>
  <p><a href="/">See all packages</a></p>
{:else}
  <header class="head" style:--hue={hueOf(pkg.type)}>
    <h1>{pkg.name}</h1>
    <p class="facts">
      {#if shown}<span class="version">{shown.version}</span>{/if}
      <span class="kind">{pkg.type}</span>
      <span class="muted">{pkg.visibility}</span>
      {#if shown?.yanked}<span class="yanked">yanked</span>{/if}
    </p>
    {#if pkg.description}<p class="lede">{shown?.description || pkg.description}</p>{/if}
  </header>

  {#if problem}
    <p class="notice problem" role="alert">{problem}</p>
  {/if}

  {#if shown}
    <div class="install">
      <code>{command}</code>
      <button class="button primary" onclick={copy} aria-live="polite">{copied ? 'Copied' : 'Copy command'}</button>
    </div>
  {:else}
    <p class="notice">
      Nothing is published under this name yet. Publish the first version with
      <code>ctx publish &lt;directory&gt;</code>.
    </p>
  {/if}

  <div class="columns">
    <article class="readme">
      {#if readme}
        <!-- Sanitised in renderMarkdown. -->
        {@html readme}
      {:else if shown}
        <p class="muted">This version has no README.md.</p>
      {/if}
    </article>

    <aside>
      {#if shown}
        <section aria-labelledby="stack-title">
          <h2 id="stack-title">What gets installed</h2>
          <!-- The package on top, what it needs underneath: layers, like the context itself. -->
          <ol class="stack">
            <li style:--hue={hueOf(pkg.type)}>
              <span class="layer-name">{pkg.name}</span>
              <span class="layer-range">{shown.version}</span>
            </li>
            {#each dependencies as [dependency, range] (dependency)}
              {@const known = dependencyTypes[dependency]}
              <li style:--hue={known && known !== '?' ? hueOf(known) : 'var(--line)'}>
                <a class="layer-name" href="/package/{dependency}">{dependency}</a>
                <span class="layer-range">{range}</span>
                {#if known === '?'}<span class="small muted hidden-note">not visible to you; left out on install</span>{/if}
              </li>
            {/each}
          </ol>
          <p class="small muted">
            {shown.fileCount} file{shown.fileCount === 1 ? '' : 's'}, {formatBytes(shown.size)} compressed
          </p>
        </section>
      {/if}

      {#if versions.length > 0}
        <section aria-labelledby="versions-title">
          <h2 id="versions-title">Versions</h2>
          <ul class="versions">
            {#each versions as entry (entry.version)}
              <li class:current={entry.version === shown?.version}>
                <button class="pick" onclick={() => (chosen = entry.version)} aria-pressed={entry.version === shown?.version}>
                  <span class="version">{entry.version}</span>
                  {#each Object.entries(pkg.distTags).filter(([, tagged]) => tagged === entry.version) as [tag] (tag)}
                    <span class="tag">{tag}</span>
                  {/each}
                  {#if entry.yanked}<span class="yanked">yanked</span>{/if}
                </button>
                <span class="small muted when">{formatDate(entry.publishedAt)}{entry.publishedBy ? `, ${entry.publishedBy}` : ''}</span>
                {#if maintainer}
                  <button class="button plain small" disabled={saving} onclick={() => yank(entry)}>
                    {entry.yanked ? 'Restore' : 'Yank'}
                  </button>
                {/if}
              </li>
            {/each}
          </ul>
        </section>
      {/if}

      {#if shown && shown.files.length > 0}
        <details>
          <summary>Files in {shown.version}</summary>
          <ul class="files small">
            {#each shown.files as file (file.path)}
              <li><span>{file.path}</span><span class="muted">{formatBytes(file.size)}</span></li>
            {/each}
          </ul>
        </details>
      {/if}

      {#if maintainer}
        <section aria-labelledby="access-title" class="access">
          <h2 id="access-title">Who can read this</h2>

          <div class="choice" role="group" aria-label="Visibility">
            <button class="button" class:primary={pkg.visibility === 'restricted'} disabled={saving} onclick={() => change({ visibility: 'restricted' })}>
              Only people listed here
            </button>
            <button class="button" class:primary={pkg.visibility === 'public'} disabled={saving} onclick={() => change({ visibility: 'public' })}>
              Anyone
            </button>
          </div>

          <ul class="people">
            {#each pkg.maintainers as person (person)}
              <li><span>{person}</span><span class="small muted">maintainer</span></li>
            {/each}
            {#each pkg.readers as person (person)}
              <li>
                <span>{person}</span>
                <button class="button plain small" disabled={saving} onclick={() => change({ readers: pkg!.readers.filter((entry) => entry !== person) })}>
                  Remove<span class="visually-hidden"> {person}</span>
                </button>
              </li>
            {/each}
            {#each pkg.readerDomains as domain (domain)}
              <li>
                <span>Everyone at {domain}</span>
                <button class="button plain small" disabled={saving} onclick={() => change({ readerDomains: pkg!.readerDomains.filter((entry) => entry !== domain) })}>
                  Remove<span class="visually-hidden"> {domain}</span>
                </button>
              </li>
            {/each}
          </ul>

          <form class="add" onsubmit={addReader}>
            <label class="visually-hidden" for="grant">Email address or @domain to give read access</label>
            <input id="grant" class="field" bind:value={grant} placeholder="ana@example.org or @example.org" autocomplete="off" />
            <button class="button" disabled={saving || grant.trim() === ''}>Give access</button>
          </form>

          <h3>Workflows that may publish</h3>
          {#if pkg.trustedPublishers.length === 0}
            <p class="small muted">None. Only maintainers can publish, from their own machine.</p>
          {/if}
          <ul class="people">
            {#each pkg.trustedPublishers as publisher (publisher.repository + (publisher.workflow ?? ''))}
              <li>
                <span>
                  {publisher.repository}
                  {#if publisher.workflow || publisher.ref}
                    <span class="small muted">{[publisher.workflow?.split('/').pop(), publisher.ref].filter(Boolean).join(', ')}</span>
                  {/if}
                </span>
                <button class="button plain small" disabled={saving} onclick={() => change({ trustedPublishers: pkg!.trustedPublishers.filter((entry) => entry !== publisher) })}>
                  Remove<span class="visually-hidden"> {publisher.repository}</span>
                </button>
              </li>
            {/each}
          </ul>
          <form class="add" onsubmit={addPublisher}>
            <label class="visually-hidden" for="trust">GitHub repository whose workflows may publish</label>
            <input id="trust" class="field" bind:value={trust} placeholder="owner/repo" autocomplete="off" />
            <button class="button" disabled={saving || trust.trim() === ''}>Trust repository</button>
          </form>
        </section>
      {/if}
    </aside>
  </div>
{/if}

<style>
  .head {
    display: grid;
    gap: 0.5rem;
    padding-left: 1rem;
    border-left: 6px solid var(--hue);
  }

  h1 {
    font-size: var(--step-3);
    overflow-wrap: anywhere;
  }

  h2 {
    font-size: var(--step-1);
    margin-bottom: 0.6rem;
  }

  h3 {
    font-size: var(--step-0);
    margin-top: 1.25rem;
    margin-bottom: 0.4rem;
  }

  .facts {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 0.25rem 1rem;
  }

  .version {
    font-family: var(--mono);
  }

  .kind {
    color: var(--hue);
    font-weight: 600;
  }

  .yanked {
    color: var(--danger);
    font-size: var(--step--1);
    font-weight: 600;
  }

  .lede {
    max-width: var(--measure);
    font-size: var(--step-1);
    color: var(--ink-soft);
  }

  .install {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 0.75rem;
    padding: 0.75rem 0.75rem 0.75rem 1.1rem;
    border: 1px solid var(--line);
    border-radius: 8px;
    background: var(--surface);
  }

  .install code {
    font-size: var(--step-1);
    overflow-wrap: anywhere;
  }

  .columns {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(17rem, 23rem);
    gap: clamp(1.5rem, 1rem + 3vw, 3.5rem);
    align-items: start;
  }

  aside {
    display: grid;
    gap: 2rem;
  }

  /* README: typography for content written by someone else. */
  .readme {
    max-width: var(--measure);
    min-width: 0;
  }

  .readme :global(h1) {
    font-size: var(--step-2);
    margin: 0 0 0.75rem;
  }

  .readme :global(h2) {
    font-size: var(--step-1);
    margin: 2rem 0 0.5rem;
  }

  .readme :global(h3) {
    font-size: var(--step-0);
    margin: 1.5rem 0 0.4rem;
  }

  .readme :global(p),
  .readme :global(ul),
  .readme :global(ol),
  .readme :global(blockquote),
  .readme :global(table) {
    margin: 0 0 1rem;
  }

  .readme :global(ul),
  .readme :global(ol) {
    padding-left: 1.4rem;
  }

  .readme :global(pre) {
    margin: 0 0 1rem;
    padding: 0.9rem 1rem;
    border: 1px solid var(--line);
    border-radius: 6px;
    background: var(--surface);
    overflow-x: auto;
  }

  .readme :global(:not(pre) > code) {
    padding: 0.1em 0.3em;
    border-radius: 4px;
    background: var(--surface);
  }

  .readme :global(blockquote) {
    padding-left: 1rem;
    border-left: 3px solid var(--line);
    color: var(--ink-soft);
  }

  .readme :global(img) {
    max-width: 100%;
  }

  .readme :global(table) {
    border-collapse: collapse;
    display: block;
    overflow-x: auto;
  }

  .readme :global(th),
  .readme :global(td) {
    padding: 0.35rem 0.7rem;
    border: 1px solid var(--line);
    text-align: left;
  }

  /* The dependency stack: solid bands, the package itself on top. */
  .stack {
    list-style: none;
    margin: 0 0 0.6rem;
    padding: 0;
    display: grid;
    gap: 3px;
  }

  .stack li {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    justify-content: space-between;
    gap: 0.1rem 0.75rem;
    padding: 0.55rem 0.8rem;
    border-radius: 4px;
    background: color-mix(in srgb, var(--hue) 16%, var(--surface));
    border-left: 6px solid var(--hue);
  }

  .stack li:first-child {
    background: color-mix(in srgb, var(--hue) 30%, var(--surface));
  }

  .layer-name {
    font-weight: 600;
    overflow-wrap: anywhere;
  }

  .layer-range {
    font-family: var(--mono);
    font-size: var(--step--1);
  }

  .hidden-note {
    flex-basis: 100%;
  }

  .versions,
  .people,
  .files {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  .versions li {
    display: grid;
    grid-template-columns: 1fr auto;
    align-items: baseline;
    gap: 0 0.5rem;
    padding: 0.4rem 0 0.4rem 0.6rem;
    border-left: 3px solid transparent;
    border-bottom: 1px solid var(--line);
  }

  .versions li.current {
    border-left-color: var(--ink);
  }

  .pick {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 0.5rem;
    padding: 0;
    border: 0;
    background: none;
    cursor: pointer;
    text-align: left;
  }

  .pick:hover .version {
    text-decoration: underline;
  }

  .tag {
    padding: 0 0.45rem;
    border: 1px solid var(--line);
    border-radius: 999px;
    font-size: var(--step--1);
  }

  .when {
    grid-column: 1;
  }

  .versions .button {
    grid-column: 2;
    grid-row: 1 / span 2;
    align-self: center;
  }

  details summary {
    cursor: pointer;
    font-weight: 500;
  }

  .files {
    margin-top: 0.5rem;
    font-family: var(--mono);
  }

  .files li,
  .people li {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 1rem;
    padding: 0.25rem 0;
    overflow-wrap: anywhere;
  }

  .people li {
    border-bottom: 1px solid var(--line);
  }

  .access {
    padding-top: 1.5rem;
    border-top: 1px solid var(--line);
  }

  .choice {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem;
    margin-bottom: 0.9rem;
  }

  .add {
    display: flex;
    gap: 0.4rem;
    margin-top: 0.7rem;
  }

  .add .field {
    flex: 1;
  }

  @media (max-width: 58rem) {
    .columns {
      grid-template-columns: minmax(0, 1fr);
    }
  }
</style>
