<script lang="ts">
  import { createQuery } from "@tanstack/svelte-query";
  import { derived, writable } from "svelte/store";
  import { Disc3, Search as SearchIcon, SlidersHorizontal } from "@lucide/svelte";
  import { api, appPath, type Envelope, type PublicConfig, type SearchGroup, type SearchPage, type SearchTorrent } from "../lib/api";
  import AddDownloadDialog from "../lib/AddDownloadDialog.svelte";
  import DeduplicationProgress from "../lib/DeduplicationProgress.svelte";
  import PreferredVariants from "../lib/PreferredVariants.svelte";
  import { releaseTypeColor } from "../lib/releasePresentation";
  import {
    browserViewPath,
    closeOverlay,
    integerSet,
    navigateView,
    optionalPositiveInteger,
    positiveInteger,
    replaceView,
    type ViewQuery
  } from "../lib/routing";
  import StaleNotice from "../lib/StaleNotice.svelte";

  const initial = new URLSearchParams(location.search);
  const initialPage = positiveInteger(initial, "page", 1);
  const initialValues = {
    query: initial.get("query") ?? "",
    artist: initial.get("artist") ?? "",
    year: initial.get("year") ?? "",
    format: initial.get("format") ?? "",
    media: initial.get("media") ?? "",
    tracker: initial.get("tracker") ?? "",
    page: initialPage,
    submitted: ["query", "artist", "year", "format", "media", "tracker", "page"]
      .some((key) => initial.has(key))
  };
  let query = $state(initialValues.query);
  let artist = $state(initialValues.artist);
  let year = $state(initialValues.year);
  let format = $state(initialValues.format);
  let media = $state(initialValues.media);
  let tracker = $state(initialValues.tracker);
  let submitted = $state(Boolean(initialValues.submitted));
  let selected = $state<{ group: SearchGroup; torrent: SearchTorrent } | null>(null);
  let showRedundantSingles = $state(initial.get("covered") === "1");
  let expandedGroups = $state(integerSet(initial, "expanded"));
  const requestedAddTorrent = optionalPositiveInteger(initial, "add");

  const config = createQuery({
    queryKey: ["config"],
    queryFn: () => api<PublicConfig>("/api/v1/config")
  });
  const searchValues = writable(initialValues);
  const resultOptions = derived(searchValues, (values) => {
    const params = new URLSearchParams();
    if (values.query) params.set("query", values.query);
    if (values.artist) params.set("artist", values.artist);
    if (values.year) params.set("year", values.year);
    if (values.format) params.set("format", values.format);
    if (values.media) params.set("media", values.media);
    if (values.tracker) params.set("tracker", values.tracker);
    if (values.page > 1) params.set("page", String(values.page));
    return {
      queryKey: ["search", values],
      queryFn: () => api<Envelope<SearchPage>>(`/api/v1/search?${params}`),
      enabled: Boolean(values.submitted),
      refetchInterval: 5_000
    };
  });
  const results = createQuery(resultOptions);

  function viewQuery(overrides: ViewQuery = {}): ViewQuery {
    return {
      query,
      artist,
      year,
      format,
      media,
      tracker,
      page: initialValues.page > 1 ? initialValues.page : undefined,
      covered: showRedundantSingles,
      expanded: [...expandedGroups],
      ...overrides
    };
  }

  function submit(event: SubmitEvent) {
    event.preventDefault();
    navigateView("/search", viewQuery({ page: undefined, covered: undefined, add: undefined }));
  }

  function visibleGroups(groups: SearchGroup[]): SearchGroup[] {
    return showRedundantSingles ? groups : groups.filter((group) => !group.albumCoverage);
  }

  function coveredCount(groups: SearchGroup[]): number {
    return groups.filter((group) => group.albumCoverage).length;
  }

  function coverageTitle(group: SearchGroup): string {
    return group.albumCoverage?.albums.map((album) => album.title).join(", ") ?? "";
  }

  function toggleCovered() {
    showRedundantSingles = !showRedundantSingles;
    replaceView("/search", viewQuery());
  }

  function toggleExpanded(groupId: number, expanded: boolean) {
    const next = new Set(expandedGroups);
    if (expanded) next.add(groupId);
    else next.delete(groupId);
    expandedGroups = next;
    replaceView("/search", viewQuery());
  }

  function choose(torrent: SearchTorrent) {
    navigateView("/search", viewQuery({ add: torrent.torrentId }));
  }

  function closeAddDialog() {
    closeOverlay("/search", viewQuery({ add: undefined }));
  }

  $effect(() => {
    if (!requestedAddTorrent || selected || !$results.data) return;
    for (const group of $results.data.data.groups) {
      const torrent = group.torrents.find((candidate) => candidate.torrentId === requestedAddTorrent);
      if (torrent) {
        selected = { group, torrent };
        break;
      }
    }
  });
</script>

<svelte:head><title>Search · Wotbox</title></svelte:head>

<header class="page-heading compact">
  <div>
    <p class="eyebrow">Tracker search</p>
    <h1>Find a release</h1>
    <p>Results come directly from the tracker and retain their source metadata.</p>
  </div>
</header>

<form class="search-panel" onsubmit={submit}>
  <label class="primary-search">
    <SearchIcon size={20} />
    <span class="sr-only">Release search</span>
    <input bind:value={query} placeholder="Album, label, catalogue number…" />
    <button type="submit">Search</button>
  </label>
  <div class="filter-grid">
    <label>
      <span>Tracker</span>
      <select bind:value={tracker}>
        {#each $config.data?.trackers ?? [] as trackerName}
          <option value={trackerName}>{trackerName.toUpperCase()}</option>
        {/each}
      </select>
    </label>
    <label><span>Artist</span><input bind:value={artist} placeholder="Any artist" /></label>
    <label><span>Year</span><input bind:value={year} inputmode="numeric" placeholder="Any year" /></label>
    <label>
      <span>Format</span>
      <select bind:value={format}>
        <option value="">Any format</option>
        <option>FLAC</option>
        <option>MP3</option>
        <option>AAC</option>
      </select>
    </label>
    <label>
      <span>Media</span>
      <select bind:value={media}>
        <option value="">Any media</option>
        <option>WEB</option>
        <option>CD</option>
        <option>Vinyl</option>
      </select>
    </label>
  </div>
</form>

<StaleNotice provenance={$results.data?.provenance} />

{#if $results.data?.data.deduplication}
  <DeduplicationProgress
    status={$results.data.data.deduplication}
    detail="Singles remain visible until their tracker track lists are confidently matched."
  />
{/if}

{#if !submitted}
  <div class="search-welcome">
    <SlidersHorizontal size={32} />
    <h2>Search with precision</h2>
    <p>Start broad, then use format and media filters to find the edition you want.</p>
  </div>
{:else if $results.isPending}
  <div class="result-list">
    {#each [1, 2, 3] as _}<div class="release-card skeleton-card"></div>{/each}
  </div>
{:else if $results.isError}
  <div class="error-panel">{$results.error.message}</div>
{:else if $results.data?.data.groups.length}
  <div class="result-summary">
    <span>{$results.data.data.totalResults?.toLocaleString() ?? $results.data.data.groups.length} results</span>
    <div class="result-summary-actions">
      {#if coveredCount($results.data.data.groups)}
        <button class="secondary-button compact-button" onclick={toggleCovered}>
          {showRedundantSingles ? "Hide" : "Show"} {coveredCount($results.data.data.groups)} album-covered {coveredCount($results.data.data.groups) === 1 ? "single" : "singles"}
        </button>
      {/if}
      <span>Page {$results.data.data.currentPage} of {$results.data.data.totalPages}</span>
    </div>
  </div>
  <div class="result-list">
    {#each visibleGroups($results.data.data.groups) as group}
      <article class="release-card release-type-coded" style={`--release-type-color: ${releaseTypeColor(group.releaseType)}`}>
        <div class="cover">
          <Disc3 size={36} />
          {#if group.image}
            <img
              src={group.image}
              alt=""
              referrerpolicy="no-referrer"
              loading="lazy"
              onerror={(event) => (event.currentTarget as HTMLImageElement).remove()}
            />
          {/if}
        </div>
        <div class="release-content">
          <div class="release-heading">
            <div>
              <p>{group.artist ?? "Various artists"}</p>
              <h2><a href={appPath(`/releases/${$results.data.provenance.tracker}/${group.groupId}?from=search`)}>{group.name}</a></h2>
              <span>
                {[group.year, group.releaseType].filter(Boolean).join(" · ")}
                {#if group.albumCoverage}
                  <span class="album-coverage-badge" title={`Covered by ${coverageTitle(group)}`}>
                    {group.albumCoverage.confidence === "fuzzy" ? "Likely included on albums" : "Included on albums"}
                  </span>
                  <span class="album-coverage-links">
                    {#each group.albumCoverage.albums as album, index}
                      {#if index}, {/if}<a href={appPath(`/releases/${album.tracker}/${album.groupId}?from=search`)}>{album.title}</a>
                    {/each}
                  </span>
                {/if}
              </span>
            </div>
            <div class="tag-list">
              {#each group.tags.slice(0, 3) as tag}<span>{tag}</span>{/each}
            </div>
          </div>
          <PreferredVariants
            variants={group.torrents}
            tracker={$results.data.provenance.tracker}
            groupId={group.groupId}
            title={group.name}
            expanded={expandedGroups.has(group.groupId)}
            onexpandedchange={(expanded) => toggleExpanded(group.groupId, expanded)}
            source="search"
            onadd={(torrent) => choose(torrent as SearchTorrent)}
          />
        </div>
      </article>
    {/each}
  </div>
  {#if $results.data.data.totalPages > 1}
    <nav class="pagination" aria-label="Search result pages">
      {#if $results.data.data.currentPage > 1}
        <a
          class="secondary-button compact-button"
          rel="prev"
          href={browserViewPath("/search", viewQuery({
            page: $results.data.data.currentPage - 1,
            add: undefined
          }))}
        >Previous</a>
      {/if}
      <span>Page {$results.data.data.currentPage} of {$results.data.data.totalPages}</span>
      {#if $results.data.data.currentPage < $results.data.data.totalPages}
        <a
          class="secondary-button compact-button"
          rel="next"
          href={browserViewPath("/search", viewQuery({
            page: $results.data.data.currentPage + 1,
            add: undefined
          }))}
        >Next</a>
      {/if}
    </nav>
  {/if}
{:else}
  <div class="search-welcome">
    <SearchIcon size={32} />
    <h2>No releases found</h2>
    <p>Try removing a filter or using a broader title.</p>
  </div>
{/if}

<AddDownloadDialog
  selection={selected ? {
    name: selected.group.name,
    artist: selected.group.artist,
    torrent: selected.torrent
  } : null}
  tracker={$results.data?.provenance.tracker ?? tracker}
  onclose={closeAddDialog}
/>
