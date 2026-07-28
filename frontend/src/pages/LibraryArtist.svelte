<script lang="ts">
  import { createQuery } from "@tanstack/svelte-query";
  import {
    AlertTriangle,
    ArrowLeft,
    Disc3,
    Search
  } from "@lucide/svelte";
  import { derived, writable } from "svelte/store";
  import { untrack } from "svelte";
  import AddDownloadDialog from "../lib/AddDownloadDialog.svelte";
  import DeduplicationProgress from "../lib/DeduplicationProgress.svelte";
  import PreferredVariants from "../lib/PreferredVariants.svelte";
  import { releaseTypeColor } from "../lib/releasePresentation";
  import StaleNotice from "../lib/StaleNotice.svelte";
  import {
    api,
    appPath,
    type ArtistCatalogPage,
    type ArtistCatalogRelease,
    type ArtistCatalogRole,
    type DownloadSelection,
    type Envelope,
    type LibraryArtistPage
  } from "../lib/api";

  let { tracker, artistKey }: { tracker: string; artistKey: string } = $props();
  const initialTracker = untrack(() => tracker);
  const initialArtistKey = untrack(() => artistKey);
  const search = writable("");
  const format = writable("");
  const ownership = writable("all");
  const sort = writable("year_desc");
  const showRedundantSingles = writable(false);
  let selected = $state<DownloadSelection | null>(null);

  const artist = createQuery({
    queryKey: ["library-artist", initialTracker, initialArtistKey],
    queryFn: () => api<LibraryArtistPage>(
      `/api/v1/library/artists/${encodeURIComponent(initialTracker)}/${encodeURIComponent(initialArtistKey)}?limit=5000`
    ),
    staleTime: 30_000
  });

  const catalogOptions = derived(artist, ($artist) => {
    const artistId = $artist.data?.artist.artistId;
    return {
      queryKey: ["artist-catalog", initialTracker, artistId] as const,
      queryFn: () => api<Envelope<ArtistCatalogPage>>(
        `/api/v1/artists/${encodeURIComponent(initialTracker)}/${artistId}/releases`
      ),
      enabled: artistId != null,
      staleTime: 60_000,
      refetchInterval: 5_000
    };
  });
  const catalog = createQuery(catalogOptions);

  const filteredGroups = derived(
    [artist, catalog, search, format, ownership, sort],
    ([$artist, $catalog, $search, $format, $ownership, $sort]) => {
      const catalogGroups = $catalog.data?.data.groups;
      let items: ArtistCatalogRelease[] = catalogGroups
        ? [...catalogGroups]
        : ($artist.data?.items ?? []).map((item) => {
            const credit = item.release.artists.find((candidate) => candidate.key === initialArtistKey);
            return {
              release: item.release,
              tags: [],
              variants: item.variants,
              roles: [credit?.role === "primary" ? "primary" : "guest"],
              listedOnTracker: true,
              libraryAvailability: item.availability,
              libraryAddedAt: item.addedAt
            };
          });
      const needle = $search.trim().toLowerCase();
      items = items.filter((item) => {
        if (needle && !item.release.title.toLowerCase().includes(needle)) return false;
        if ($format && !item.variants.some((variant) => variant.format?.toLowerCase() === $format.toLowerCase())) {
          return false;
        }
        if ($ownership === "library" && item.libraryAvailability == null) return false;
        if ($ownership === "missing" && !item.variants.some((variant) => variant.library?.availability === "missing")) {
          return false;
        }
        if ($ownership === "downloading" && !item.variants.some((variant) =>
          variant.downloads.some((download) =>
            ["downloading", "queued", "checking", "stalled"].includes(download.state)
          )
        )) {
          return false;
        }
        if ($ownership === "available" && !item.variants.some(isAddable)) return false;
        return true;
      });
      if ($sort === "title") {
        items.sort((left, right) => left.release.title.localeCompare(right.release.title));
      } else if ($sort === "added_desc") {
        items.sort((left, right) =>
          (right.libraryAddedAt ?? "").localeCompare(left.libraryAddedAt ?? "")
        );
      } else {
        items.sort((left, right) =>
          (right.release.year ?? 0) - (left.release.year ?? 0)
          || left.release.title.localeCompare(right.release.title)
        );
      }
      return items;
    }
  );
  const hiddenSingles = derived(filteredGroups, ($groups) =>
    $groups.filter((group) => group.release.albumCoverage).length
  );
  const groups = derived(
    [filteredGroups, showRedundantSingles],
    ([$groups, $showRedundantSingles]) =>
      $showRedundantSingles
        ? $groups
        : $groups.filter((group) => !group.release.albumCoverage)
  );
  const primaryGroups = derived(groups, ($groups) =>
    $groups.filter((group) => group.roles.includes("primary"))
  );
  const appearanceGroups = derived(groups, ($groups) =>
    $groups.filter((group) => !group.roles.includes("primary"))
  );

  function isAddable(variant: ArtistCatalogRelease["variants"][number]): boolean {
    return variant.downloads.length === 0
      && variant.library?.availability !== "present";
  }

  function roleLabel(roles: ArtistCatalogRole[]): string {
    return roles
      .filter((role) => role !== "primary")
      .map((role) => role === "dj" ? "DJ" : role.charAt(0).toUpperCase() + role.slice(1))
      .join(", ") || "Appearance";
  }

  function displayArtist(group: ArtistCatalogRelease): string {
    const primaryCount = group.release.artists.filter((artist) => artist.role === "primary").length;
    return primaryCount > 3 ? "Various artists" : group.release.artist ?? "Various artists";
  }

  function choose(group: ArtistCatalogRelease, torrent: ArtistCatalogRelease["variants"][number]) {
    selected = {
      name: group.release.title,
      artist: displayArtist(group),
      torrent
    };
  }
</script>

<svelte:head><title>{$artist.data?.artist.name ?? "Artist"} · Library · Wotbox</title></svelte:head>

<a class="back-link" href={appPath("/library")}><ArrowLeft size={16} /> Back to Library</a>

{#if $artist.isPending}
  <div class="release-hero skeleton-card"></div>
{:else if $artist.isError}
  <div class="error-panel">{$artist.error.message}</div>
{:else if $artist.data}
  <header class="artist-hero">
    <div class="artist-mosaic artist-hero-mosaic" class:single={Boolean($catalog.data?.data.artist.artwork) || $artist.data.artist.artworks.length < 2}>
      {#if $catalog.data?.data.artist.artwork}
        <img src={$catalog.data.data.artist.artwork} alt="" referrerpolicy="no-referrer" />
      {:else if $artist.data.artist.artworks.length}
        {#each $artist.data.artist.artworks as artwork}
          <img src={artwork} alt="" referrerpolicy="no-referrer" onerror={(event) => ((event.currentTarget as HTMLImageElement).style.display = "none")} />
        {/each}
      {:else}
        <Disc3 size={48} />
      {/if}
    </div>
    <div>
      <p class="eyebrow">{$artist.data.artist.tracker.toUpperCase()} artist</p>
      <h1>{$catalog.data?.data.artist.name ?? $artist.data.artist.name}</h1>
      <p>
        {$artist.data.artist.releaseCount} {$artist.data.artist.releaseCount === 1 ? "release" : "releases"} in your library
        {#if $catalog.data} · {$catalog.data.data.groups.length} on the tracker{/if}
      </p>
      {#if $artist.data.artist.missingCount}
        <span class="availability-warning"><AlertTriangle size={13} /> {$artist.data.artist.missingCount} need attention</span>
      {/if}
    </div>
  </header>

  {#if $artist.data.artist.artistId == null}
    <div class="index-banner">
      <span class="index-pulse"></span>
      <p><strong>Tracker catalog pending</strong> This artist’s stable Gazelle identity is still being resolved. Your Library releases remain available below.</p>
    </div>
  {:else if $catalog.isPending}
    <div class="index-banner">
      <span class="index-pulse"></span>
      <p><strong>Loading tracker discography</strong> Your Library releases are already visible while Gazelle responds.</p>
    </div>
  {:else if $catalog.isError}
    <div class="error-panel compact">The tracker catalog is temporarily unavailable. Showing your Library releases instead.</div>
  {/if}

  <StaleNotice provenance={$catalog.data?.provenance} />

  {#if $catalog.data?.data.deduplication}
    <DeduplicationProgress
      status={$catalog.data.data.deduplication}
      detail="Singles remain visible until their tracker track lists are confidently matched."
    />
  {/if}

  <section class="library-controls artist-controls" aria-label="Artist release filters">
    <label class="library-search">
      <Search size={17} />
      <span class="sr-only">Search release titles</span>
      <input bind:value={$search} placeholder="Search this artist’s releases" />
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
      <span>Ownership</span>
      <select bind:value={$ownership}>
        <option value="all">Everything</option>
        <option value="available">Available to add</option>
        <option value="library">In Library</option>
        <option value="downloading">Downloading</option>
        <option value="missing">Missing</option>
      </select>
    </label>
    <label>
      <span>Sort</span>
      <select bind:value={$sort}>
        <option value="year_desc">Newest release year</option>
        <option value="title">Title A–Z</option>
        <option value="added_desc">Recently added</option>
      </select>
    </label>
  </section>

  {#snippet releaseSection(title: string, items: ArtistCatalogRelease[], appearances = false)}
    {#if items.length || (!appearances && $hiddenSingles)}
      <section class="artist-discography">
        <div class="discography-heading">
          <div>
            <p class="eyebrow">{appearances ? "Collaborations and credits" : "Discography"}</p>
            <h2>{title}</h2>
          </div>
          <div class="discography-actions">
            {#if !appearances && $hiddenSingles}
              <button class="secondary-button compact-button" onclick={() => $showRedundantSingles = !$showRedundantSingles}>
                {$showRedundantSingles ? "Hide" : "Show"} {$hiddenSingles} album-covered {$hiddenSingles === 1 ? "single" : "singles"}
              </button>
            {/if}
            <span>{items.length} {items.length === 1 ? "release" : "releases"}</span>
          </div>
        </div>
        <div class="result-list artist-catalog-list">
          {#each items as group}
            <article class="release-card catalog-release-card release-type-coded" style={`--release-type-color: ${releaseTypeColor(group.release.releaseType)}`}>
              <a class="cover" href={appPath(`/releases/${group.release.tracker}/${group.release.groupId}?from=library`)}>
                <Disc3 size={36} />
                {#if group.release.artwork}
                  <img src={group.release.artwork} alt="" referrerpolicy="no-referrer" loading="lazy" onerror={(event) => (event.currentTarget as HTMLImageElement).remove()} />
                {/if}
              </a>
              <div class="release-content">
                <div class="release-heading">
                  <div>
                    <p>
                      {displayArtist(group)}
                      {#if appearances}<span class="role-badge">{roleLabel(group.roles)}</span>{/if}
                      {#if !group.listedOnTracker}<span class="availability-warning"><AlertTriangle size={12} /> Library only</span>{/if}
                    </p>
                    <h3><a href={appPath(`/releases/${group.release.tracker}/${group.release.groupId}?from=library`)}>{group.release.title}</a></h3>
                    <span>
                      {[group.release.year, group.release.releaseType].filter(Boolean).join(" · ") || "Release details unavailable"}
                      {#if group.release.albumCoverage}
                        <span class="album-coverage-badge" title={`Covered by ${group.release.albumCoverage.albums.map((album) => album.title).join(", ")}`}>
                          {group.release.albumCoverage.confidence === "fuzzy" ? "Likely included on albums" : "Included on albums"}
                        </span>
                        <span class="album-coverage-links">
                          {#each group.release.albumCoverage.albums as album, index}
                            {#if index}, {/if}<a href={appPath(`/releases/${album.tracker}/${album.groupId}?from=library`)}>{album.title}</a>
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
                  variants={group.variants}
                  tracker={group.release.tracker}
                  groupId={group.release.groupId}
                  title={group.release.title}
                  fromLibrary={true}
                  onadd={(torrent) => choose(group, torrent as ArtistCatalogRelease["variants"][number])}
                />
              </div>
            </article>
          {/each}
        </div>
      </section>
    {/if}
  {/snippet}

  {@render releaseSection("Primary releases", $primaryGroups)}
  {@render releaseSection("Appearances", $appearanceGroups, true)}

  {#if !$groups.length && !$hiddenSingles}
    <div class="search-welcome">
      <Disc3 size={34} />
      <h2>No releases match these filters</h2>
      <p>Try clearing the title, format, or ownership filter.</p>
    </div>
  {/if}
{/if}

<AddDownloadDialog
  selection={selected}
  tracker={initialTracker}
  onclose={() => selected = null}
/>
