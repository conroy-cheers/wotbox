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
  import ReleaseCover from "../lib/ReleaseCover.svelte";
  import TrackerLinks from "../lib/TrackerLinks.svelte";
  import { releaseTypeColor } from "../lib/releasePresentation";
  import CachedImage from "../lib/CachedImage.svelte";
  import { liveDownloads, variantDownloads } from "../lib/liveState";
  import {
    api,
    appPath,
    type ArtistCatalogRelease,
    type ArtistCatalogRole,
    type DownloadSelection,
    type LibraryArtistPage
  } from "../lib/api";
  import {
    closeOverlay,
    integerSet,
    navigateView,
    oneOf,
    optionalPositiveInteger,
    replaceView,
    type ViewQuery
  } from "../lib/routing";

  let { id }: { id: string } = $props();
  const initialId = untrack(() => id);
  const routePath = `/library/artists/${encodeURIComponent(initialId)}`;
  const initial = new URLSearchParams(location.search);
  const search = writable(initial.get("q") ?? "");
  const format = writable(initial.get("format") ?? "");
  const ownership = writable(oneOf(
    initial,
    "ownership",
    ["all", "available", "library", "downloading", "missing"] as const,
    "all"
  ));
  const sort = writable(oneOf(
    initial,
    "sort",
    ["year_desc", "title", "added_desc"] as const,
    "year_desc"
  ));
  const showRedundantSingles = writable(initial.get("covered") === "1");
  let expandedGroups = $state(integerSet(initial, "expanded"));
  const requestedAddTorrent = optionalPositiveInteger(initial, "add");
  let selected = $state<DownloadSelection | null>(null);
  let urlSyncReady = false;

  const artist = createQuery({
    queryKey: ["library-artist", initialId],
    queryFn: () => api<LibraryArtistPage>(
      `/api/v1/library/artists/${encodeURIComponent(initialId)}?limit=5000`
    ),
    staleTime: 30_000
  });

  async function retryCatalog(): Promise<void> {
    await api<void>(
      `/api/v1/library/artists/${encodeURIComponent(initialId)}/refresh`,
      { method: "POST" }
    );
    await $artist.refetch();
  }

  const filteredGroups = derived(
    [artist, search, format, ownership, sort, liveDownloads],
    ([$artist, $search, $format, $ownership, $sort, $liveDownloads]) => {
      const libraryGroups: ArtistCatalogRelease[] = ($artist.data?.items ?? []).map((item) => {
        const credit = item.release.artists.find((candidate) => candidate.canonicalId === initialId);
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
      const byRelease = new Map(
        libraryGroups.map((group) => [group.release.id ?? `${group.release.tracker}:${group.release.groupId}`, group])
      );
      for (const group of $artist.data?.catalog.groups ?? []) {
        const key = group.release.id ?? `${group.release.tracker}:${group.release.groupId}`;
        const libraryGroup = byRelease.get(key);
        byRelease.set(key, libraryGroup
          ? {
              ...group,
              libraryAvailability: libraryGroup.libraryAvailability,
              libraryAddedAt: libraryGroup.libraryAddedAt
            }
          : group);
      }
      let items = [...byRelease.values()];
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
          variantDownloads(variant, $liveDownloads).some((download) =>
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

  function viewQuery(overrides: ViewQuery = {}): ViewQuery {
    return {
      q: $search.trim(),
      format: $format,
      ownership: $ownership === "all" ? undefined : $ownership,
      sort: $sort === "year_desc" ? undefined : $sort,
      covered: $showRedundantSingles,
      expanded: [...expandedGroups],
      ...overrides
    };
  }

  $effect(() => {
    const query = viewQuery();
    if (urlSyncReady) replaceView(routePath, query);
    else urlSyncReady = true;
  });

  function isAddable(variant: ArtistCatalogRelease["variants"][number]): boolean {
    return variantDownloads(variant, $liveDownloads).length === 0
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

  function choose(torrent: ArtistCatalogRelease["variants"][number]) {
    navigateView(routePath, viewQuery({ add: torrent.torrentId }));
  }

  function toggleExpanded(groupId: number, expanded: boolean) {
    const next = new Set(expandedGroups);
    if (expanded) next.add(groupId);
    else next.delete(groupId);
    expandedGroups = next;
  }

  function closeAddDialog() {
    closeOverlay(routePath, viewQuery({ add: undefined }));
  }

  $effect(() => {
    if (!requestedAddTorrent || selected) return;
    const candidates = $artist.data?.catalog.groups ?? $filteredGroups;
    for (const group of candidates) {
      const torrent = group.variants.find((candidate) => candidate.torrentId === requestedAddTorrent);
      if (torrent) {
        selected = {
          name: group.release.title,
          artist: displayArtist(group),
          torrent
        };
        break;
      }
    }
  });
</script>

<svelte:head><title>{$artist.data?.artist.name ?? "Artist"} · Library · Wotbox</title></svelte:head>

<a class="back-link" href={appPath("/library")}><ArrowLeft size={16} /> Back to Library</a>

{#if $artist.isPending}
  <div class="release-hero skeleton-card"></div>
{:else if $artist.isError}
  <div class="error-panel">{$artist.error.message}</div>
{:else if $artist.data}
  <header class="artist-hero">
    <div class="artist-mosaic artist-hero-mosaic" class:single={Boolean($artist.data.catalog.artist.artwork) || $artist.data.artist.artworks.length < 2}>
      {#if $artist.data.catalog.artist.artwork}
        <CachedImage src={$artist.data.catalog.artist.artwork} loading="eager" />
      {:else if $artist.data.artist.artworks.length}
        {#each $artist.data.artist.artworks as artwork}
          <CachedImage src={artwork} />
        {/each}
      {:else}
        <Disc3 size={48} />
      {/if}
    </div>
    <div>
      <p class="eyebrow">Canonical artist · {[...new Set($artist.data.artist.sources.map((source) => source.tracker.toUpperCase()))].join(" + ") || $artist.data.artist.tracker.toUpperCase()} sources</p>
      <h1>{$artist.data.catalog.artist.name ?? $artist.data.artist.name}</h1>
      <p>
        {$artist.data.artist.releaseCount} {$artist.data.artist.releaseCount === 1 ? "release" : "releases"} in your library
        · {$artist.data.catalog.groups.length} across configured catalogs
      </p>
      {#if $artist.data.artist.missingCount}
        <span class="availability-warning"><AlertTriangle size={13} /> {$artist.data.artist.missingCount} need attention</span>
      {/if}
      <button class="text-button" type="button" onclick={retryCatalog}>Refresh catalog</button>
    </div>
  </header>

  {#if !$artist.data.artist.sources.some((source) => source.artistId != null)}
    <div class="index-banner">
      <span class="index-pulse"></span>
      <p><strong>Tracker catalog pending</strong> This artist’s stable Gazelle identity is still being resolved. Your Library releases remain available below.</p>
    </div>
  {/if}

  {#if $artist.data.catalog.deduplication}
    <DeduplicationProgress
      status={$artist.data.catalog.deduplication}
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
              <ReleaseCover
                image={group.release.artwork}
                href={group.release.id ? appPath(`/releases/${group.release.id}?from=library`) : undefined}
                label={`${displayArtist(group)} — ${group.release.title}`}
              />
              <div class="release-content">
                <div class="release-heading">
                  <div>
                    <p>
                      {displayArtist(group)}
                      {#if appearances}<span class="role-badge">{roleLabel(group.roles)}</span>{/if}
                      {#if !group.listedOnTracker}<span class="availability-warning"><AlertTriangle size={12} /> Library only</span>{/if}
                    </p>
                    <h3><a href={group.release.id ? appPath(`/releases/${group.release.id}?from=library`) : undefined}>{group.release.title}</a></h3>
                    <span>
                      {[group.release.year, group.release.releaseType].filter(Boolean).join(" · ") || "Release details unavailable"}
                      {#if group.release.albumCoverage}
                        <span class="album-coverage-badge" title={`Covered by ${group.release.albumCoverage.albums.map((album) => album.title).join(", ")}`}>
                          {group.release.albumCoverage.confidence === "fuzzy" ? "Likely included on albums" : "Included on albums"}
                        </span>
                        <span class="album-coverage-links">
                          {#each group.release.albumCoverage.albums as album, index}
                            {#if index}, {/if}<span>{album.title}</span>
                          {/each}
                        </span>
                      {/if}
                    </span>
                    <TrackerLinks sources={group.release.sources} tracker={group.release.tracker} groupId={group.release.groupId} />
                  </div>
                  <div class="tag-list">
                    {#each group.tags.slice(0, 3) as tag}<span>{tag}</span>{/each}
                  </div>
                </div>
                <PreferredVariants
                  variants={group.variants}
                  releaseId={group.release.id}
                  tracker={group.release.tracker}
                  groupId={group.release.groupId}
                  title={group.release.title}
                  source="library"
                  expanded={expandedGroups.has(group.release.groupId)}
                  onexpandedchange={(expanded) => toggleExpanded(group.release.groupId, expanded)}
                  onadd={(torrent) => choose(torrent as ArtistCatalogRelease["variants"][number])}
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
  tracker={selected?.torrent.tracker ?? $artist.data?.artist.tracker ?? ""}
  onclose={closeAddDialog}
/>
