<script lang="ts">
  import { createMutation, createQuery, useQueryClient } from "@tanstack/svelte-query";
  import { Dialog } from "bits-ui";
  import { derived, writable } from "svelte/store";
  import { ArrowDownToLine, Check, Disc3, Search as SearchIcon, SlidersHorizontal, Users } from "@lucide/svelte";
  import { api, appPath, formatBytes, type CreateDownload, type DownloadJob, type DownloadProfile, type Envelope, type PublicConfig, type SearchGroup, type SearchPage, type SearchTorrent } from "../lib/api";
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
  let useToken = $state(false);
  let selectedProfile = $state("ops");
  const queryClient = useQueryClient();

  const config = createQuery({
    queryKey: ["config"],
    queryFn: () => api<PublicConfig>("/api/v1/config")
  });
  const profiles = createQuery({
    queryKey: ["download-profiles"],
    queryFn: () => api<DownloadProfile[]>("/api/v1/download-profiles")
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

  const addDownload = createMutation({
    mutationFn: (request: CreateDownload) =>
      api<DownloadJob>("/api/v1/downloads", {
        method: "POST",
        headers: { "Idempotency-Key": crypto.randomUUID() },
        body: JSON.stringify(request)
      }),
    onSuccess: async () => {
      selected = null;
      useToken = false;
      await queryClient.invalidateQueries({ queryKey: ["downloads"] });
    }
  });

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
      <article class="release-card">
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
          <div class="torrent-list">
            {#each group.torrents as torrent}
              <div class="torrent-row">
                <div class="torrent-format">
                  <strong>{torrent.format ?? "Unknown"}</strong>
                  <span>{torrent.encoding ?? "Unknown"} · {torrent.media ?? "Unknown"}</span>
                  {#if torrent.remasterTitle}<small>{torrent.remasterTitle}</small>{/if}
                </div>
                <span class="torrent-size">{formatBytes(torrent.size)}</span>
                <span class="peer-count" title="Seeders"><Users size={14} /> {torrent.seeders ?? 0}</span>
                <div class="torrent-flags">
                  {#if torrent.freeleech}<span class="free-badge">Free</span>{/if}
                  {#if torrent.canUseToken}<span class="token-badge">Token</span>{/if}
                </div>
                <button class="download-button" aria-label={`Download ${group.name}`} onclick={() => {
                  selected = { group, torrent };
                  selectedProfile = $profiles.data?.[0]?.name ?? "ops";
                }}><ArrowDownToLine size={17} /></button>
              </div>
            {/each}
          </div>
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

<Dialog.Root open={selected !== null} onOpenChange={(open) => { if (!open) selected = null; }}>
  <Dialog.Portal>
    <Dialog.Overlay class="dialog-overlay" />
    <Dialog.Content class="dialog-content">
      <Dialog.Title class="dialog-title">Add to qBittorrent</Dialog.Title>
      <Dialog.Description class="dialog-description">
        Confirm the release and download profile. Tracker metadata will be checked again before submission.
      </Dialog.Description>
      {#if selected}
        <div class="dialog-release">
          <div class="release-mark">{selected.group.name.slice(0, 1)}</div>
          <div>
            <strong>{selected.group.name}</strong>
            <span>{selected.group.artist} · {selected.torrent.format} {selected.torrent.encoding}</span>
          </div>
        </div>
        <label class="dialog-field">
          <span>Download profile</span>
          <select bind:value={selectedProfile}>
            {#each $profiles.data ?? [] as profile}
              <option value={profile.name}>{profile.name} · {profile.savePath}</option>
            {/each}
          </select>
        </label>
        <label class:disabled={!selected.torrent.canUseToken} class="token-toggle">
          <input type="checkbox" bind:checked={useToken} disabled={!selected.torrent.canUseToken} />
          <span class="toggle-box">{#if useToken}<Check size={15} />{/if}</span>
          <span>
            <strong>Use a freeleech token</strong>
            <small>{selected.torrent.canUseToken ? "This action consumes one tracker token." : "This torrent is not token eligible."}</small>
          </span>
        </label>
        {#if $addDownload.isError}<div class="error-panel compact">{$addDownload.error.message}</div>{/if}
        <div class="dialog-actions">
          <button class="secondary-button" onclick={() => selected = null}>Cancel</button>
          <button class="primary-button" disabled={$addDownload.isPending} onclick={() => $addDownload.mutate({
            tracker: $results.data?.provenance.tracker ?? tracker,
            torrentId: selected!.torrent.torrentId,
            profile: selectedProfile,
            useToken
          })}>{$addDownload.isPending ? "Adding…" : "Add download"}</button>
        </div>
      {/if}
    </Dialog.Content>
  </Dialog.Portal>
</Dialog.Root>
