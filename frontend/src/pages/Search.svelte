<script lang="ts">
  import { createQuery } from "@tanstack/svelte-query";
  import { derived, writable } from "svelte/store";
  import { Disc3, Search as SearchIcon, SlidersHorizontal } from "@lucide/svelte";
  import { api, appPath, type Envelope, type PublicConfig, type SearchGroup, type SearchPage, type SearchTorrent } from "../lib/api";
  import AddDownloadDialog from "../lib/AddDownloadDialog.svelte";
  import PreferredVariants from "../lib/PreferredVariants.svelte";
  import { releaseTypeColor } from "../lib/releasePresentation";
  import StaleNotice from "../lib/StaleNotice.svelte";

  const initial = new URLSearchParams(location.search);
  const initialValues = {
    query: initial.get("query") ?? "",
    artist: initial.get("artist") ?? "",
    year: initial.get("year") ?? "",
    format: initial.get("format") ?? "",
    media: initial.get("media") ?? "",
    tracker: initial.get("tracker") ?? "",
    submitted: initial.get("query") ?? initial.get("artist") ?? ""
  };
  let query = $state(initialValues.query);
  let artist = $state(initialValues.artist);
  let year = $state(initialValues.year);
  let format = $state(initialValues.format);
  let media = $state(initialValues.media);
  let tracker = $state(initialValues.tracker);
  let submitted = $state(initialValues.submitted);
  let selected = $state<{ group: SearchGroup; torrent: SearchTorrent } | null>(null);

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
    return {
      queryKey: ["search", values],
      queryFn: () => api<Envelope<SearchPage>>(`/api/v1/search?${params}`),
      enabled: Boolean(values.submitted)
    };
  });
  const results = createQuery(resultOptions);

  function submit(event: SubmitEvent) {
    event.preventDefault();
    submitted = query || artist || "*";
    const params = new URLSearchParams();
    if (query) params.set("query", query);
    if (artist) params.set("artist", artist);
    if (year) params.set("year", year);
    if (format) params.set("format", format);
    if (media) params.set("media", media);
    if (tracker) params.set("tracker", tracker);
    history.replaceState(null, "", `${location.pathname}?${params}`);
    searchValues.set({ submitted, query, artist, year, format, media, tracker });
  }
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
    <span>Page {$results.data.data.currentPage} of {$results.data.data.totalPages}</span>
  </div>
  <div class="result-list">
    {#each $results.data.data.groups as group}
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
              <h2><a href={appPath(`/releases/${$results.data.provenance.tracker}/${group.groupId}`)}>{group.name}</a></h2>
              <span>{[group.year, group.releaseType].filter(Boolean).join(" · ")}</span>
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
            onadd={(torrent) => selected = { group, torrent: torrent as SearchTorrent }}
          />
        </div>
      </article>
    {/each}
  </div>
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
  onclose={() => selected = null}
/>
