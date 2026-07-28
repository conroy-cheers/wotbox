<script lang="ts">
  import { createQuery } from "@tanstack/svelte-query";
  import { AlertTriangle, BookOpen, Disc3, Search } from "@lucide/svelte";
  import { derived, writable } from "svelte/store";
  import {
    api,
    appPath,
    relativeTime,
    type LibraryArtistSummary,
    type LibraryArtistsPage,
    type LibraryAvailability,
    type PublicConfig
  } from "../lib/api";
  import { releaseTypeColor } from "../lib/releasePresentation";

  const search = writable("");
  const tracker = writable("");
  const format = writable("");
  const availability = writable("all");
  const limit = writable(1000);

  const config = createQuery({
    queryKey: ["config"],
    queryFn: () => api<PublicConfig>("/api/v1/config")
  });
  const options = derived(
    [search, tracker, format, availability, limit],
    ([$search, $tracker, $format, $availability, $limit]) => {
      const params = new URLSearchParams({ limit: String($limit) });
      if ($search.trim()) params.set("q", $search.trim());
      if ($tracker) params.set("tracker", $tracker);
      if ($format) params.set("format", $format);
      if ($availability !== "all") params.set("availability", $availability);
      return {
        queryKey: ["library", $search, $tracker, $format, $availability, $limit] as const,
        queryFn: () => api<LibraryArtistsPage>(`/api/v1/library/artists?${params}`),
        staleTime: 30_000
      };
    }
  );
  const library = createQuery(options);

  function initial(name: string): string {
    const sortable = name.trim().replace(/^the\s+/i, "");
    const letter = sortable.charAt(0).toUpperCase();
    return /^[A-Z]$/.test(letter) ? letter : "#";
  }

  function grouped(artists: LibraryArtistSummary[]) {
    const groups = new Map<string, LibraryArtistSummary[]>();
    for (const artist of artists) {
      const letter = initial(artist.name);
      groups.set(letter, [...(groups.get(letter) ?? []), artist]);
    }
    return [...groups].map(([letter, items]) => ({ letter, items }));
  }

  function availabilityLabel(value: LibraryAvailability): string {
    if (value === "partial") return "Partially missing";
    if (value === "missing") return "Missing";
    return "Available";
  }
</script>

<svelte:head><title>Library · Wotbox</title></svelte:head>

<header class="page-heading">
  <div>
    <p class="eyebrow">Completed releases</p>
    <h1>Library</h1>
    <p>Your permanent collection, arranged by tracker artist credits.</p>
  </div>
  <BookOpen size={28} />
</header>

<section class="library-controls" aria-label="Library filters">
  <label class="library-search">
    <Search size={17} />
    <span class="sr-only">Search artists and releases</span>
    <input bind:value={$search} placeholder="Search artists and release titles" />
  </label>
  <label>
    <span>Tracker</span>
    <select bind:value={$tracker}>
      <option value="">All trackers</option>
      {#each $config.data?.trackers ?? [] as name}
        <option value={name}>{name.toUpperCase()}</option>
      {/each}
    </select>
  </label>
  <label>
    <span>Format</span>
    <select bind:value={$format}>
      <option value="">All formats</option>
      <option value="FLAC">FLAC</option>
      <option value="MP3">MP3</option>
      <option value="AAC">AAC</option>
    </select>
  </label>
  <label>
    <span>Availability</span>
    <select bind:value={$availability}>
      <option value="all">All releases</option>
      <option value="present">Present</option>
      <option value="partial">Partial</option>
      <option value="missing">Missing</option>
    </select>
  </label>
</section>

{#if $library.data?.index.lastSuccessfulScanAt}
  <p class="library-scan">
    Availability checked {relativeTime($library.data.index.lastSuccessfulScanAt)}
    {#if $library.data.index.unresolvedCredits}
      · enriching {$library.data.index.unresolvedCredits}
      {$library.data.index.unresolvedCredits === 1 ? "release" : "releases"}
    {/if}
  </p>
{/if}

{#if $library.isError}
  <div class="error-panel">{$library.error.message}</div>
{:else if $library.isPending}
  <div class="artist-grid">
    {#each [1, 2, 3, 4, 5, 6] as _}<div class="artist-card skeleton-card"></div>{/each}
  </div>
{:else if $library.data}
  {#if $search.trim()}
    <section class="library-results">
      <div class="section-heading">
        <div><p class="eyebrow">Artist matches</p><h2>{$library.data.artistTotal} {$library.data.artistTotal === 1 ? "artist" : "artists"}</h2></div>
      </div>
      {#if !$library.data.artists.length}
        <p class="library-empty-copy">No artist names match “{$search}”.</p>
      {/if}
    </section>
  {/if}

  {#if $library.data.artists.length}
    <nav class="alphabet-nav" aria-label="Jump to artist initial">
      {#each grouped($library.data.artists) as group}
        <a href={`#artists-${group.letter}`}>{group.letter}</a>
      {/each}
    </nav>
    {#each grouped($library.data.artists) as group}
      <section class="artist-letter-section" id={`artists-${group.letter}`}>
        <h2>{group.letter}</h2>
        <div class="artist-grid">
          {#each group.items as artist}
            <a
              class="artist-card"
              href={appPath(`/library/artists/${encodeURIComponent(artist.tracker)}/${encodeURIComponent(artist.key)}`)}
            >
              <div class="artist-mosaic" class:single={artist.artworks.length < 2}>
                {#if artist.artworks.length}
                  {#each artist.artworks as artwork}
                    <img src={artwork} alt="" referrerpolicy="no-referrer" onerror={(event) => ((event.currentTarget as HTMLImageElement).style.display = "none")} />
                  {/each}
                {:else}
                  <Disc3 size={38} />
                {/if}
              </div>
              <div class="artist-card-copy">
                <div>
                  <h3>{artist.name}</h3>
                  <span>{artist.tracker.toUpperCase()}</span>
                </div>
                <p>{artist.releaseCount} {artist.releaseCount === 1 ? "release" : "releases"}</p>
                {#if artist.missingCount}
                  <small class="availability-warning"><AlertTriangle size={12} /> {artist.missingCount} need attention</small>
                {/if}
              </div>
            </a>
          {/each}
        </div>
      </section>
    {/each}
  {:else if !$search.trim()}
    <div class="search-welcome">
      <BookOpen size={34} />
      <h2>No completed releases yet</h2>
      <p>Releases appear here permanently after a linked torrent reaches 100%.</p>
    </div>
  {/if}

  {#if $search.trim()}
    <section class="section">
      <div class="section-heading">
        <div><p class="eyebrow">Release matches</p><h2>{$library.data.releaseTotal} {$library.data.releaseTotal === 1 ? "release" : "releases"}</h2></div>
      </div>
      {#if $library.data.releases.length}
        <div class="library-release-grid">
          {#each $library.data.releases as item}
            <article class="library-release-card release-type-coded" style={`--release-type-color: ${releaseTypeColor(item.release.releaseType)}`}>
              <a class="cover" href={appPath(`/releases/${item.release.tracker}/${item.release.groupId}?from=library`)}>
                <Disc3 size={28} />
                {#if item.release.artwork}
                  <img src={item.release.artwork} alt="" referrerpolicy="no-referrer" onerror={(event) => ((event.currentTarget as HTMLImageElement).style.display = "none")} />
                {/if}
              </a>
              <div>
                <h3><a href={appPath(`/releases/${item.release.tracker}/${item.release.groupId}?from=library`)}>{item.release.title}</a></h3>
                <p>{item.release.artist ?? "Unknown artist"} · {[item.release.year, item.release.releaseType].filter(Boolean).join(" · ")}</p>
                <div class="variant-chips">
                  {#each item.variants as variant}
                    <a href={appPath(`/releases/${item.release.tracker}/${item.release.groupId}?torrent=${variant.torrentId}&from=library`)}>
                      {[variant.format, variant.encoding].filter(Boolean).join(" · ") || "Unknown format"}
                    </a>
                  {/each}
                </div>
              </div>
              <span class:warning={item.availability !== "present"} class="availability-label">
                {availabilityLabel(item.availability)}
              </span>
            </article>
          {/each}
        </div>
      {:else}
        <p class="library-empty-copy">No release titles match “{$search}”.</p>
      {/if}
    </section>
  {/if}

  {#if $library.data.artistTotal > $limit && $limit < 5000}
    <div class="load-more">
      <button class="secondary-button" onclick={() => $limit = Math.min($limit + 1000, 5000)}>
        Load more artists
      </button>
    </div>
  {/if}
{/if}
