<script lang="ts">
  import { createQuery } from "@tanstack/svelte-query";
  import { ArrowLeft, Clock3, Disc3, Download, FolderOpen, Gauge, HardDrive, Upload } from "@lucide/svelte";
  import { untrack } from "svelte";
  import {
    api,
    appPath,
    formatBytes,
    formatSpeed,
    relativeTime,
    type CrossSeedPlan,
    type Envelope,
    type ReleaseDetail,
    type TorrentVariant
  } from "../lib/api";
  import AddDownloadDialog from "../lib/AddDownloadDialog.svelte";
  import StaleNotice from "../lib/StaleNotice.svelte";
  import StatusPill from "../lib/StatusPill.svelte";
  import PreferredVariants from "../lib/PreferredVariants.svelte";
  import DownloadDiagnostic from "../lib/DownloadDiagnostic.svelte";
  import { sanitizeReleaseDescription } from "../lib/releaseDescription";
  import {
    closeOverlay,
    navigateView,
    oneOf,
    optionalPositiveInteger,
    replaceView,
    selectReleaseAttachment
  } from "../lib/routing";

  let { id }: { id: string } = $props();
  const initialId = untrack(() => id);
  const routePath = `/releases/${encodeURIComponent(initialId)}`;
  const pageParams = new URLSearchParams(location.search);
  const requestedTorrent = Number(pageParams.get("torrent")) || undefined;
  const requestedClient = pageParams.get("client") || undefined;
  const requestedHash = pageParams.get("hash") || undefined;
  const requestedAddTorrent = optionalPositiveInteger(pageParams, "add");
  const attachmentRequested = requestedClient != null || requestedHash != null;
  const source = oneOf(
    pageParams,
    "from",
    ["search", "library", "downloads", "channels"] as const,
    "search"
  );
  let variantsExpanded = $state(pageParams.get("expanded") === "1");
  let identifiersOpen = $state(pageParams.get("details") === "client");
  let selected = $state<TorrentVariant | null>(null);
  const release = createQuery({
    queryKey: ["release", initialId, requestedTorrent],
    queryFn: () => api<Envelope<ReleaseDetail>>(
      `/api/v1/releases/${encodeURIComponent(initialId)}`
      + (requestedTorrent ? `?torrent=${requestedTorrent}` : "")
    ),
    refetchInterval: 15_000
  });
  const crossSeedPlans = createQuery({
    queryKey: ["cross-seed-plans", initialId],
    queryFn: () => api<CrossSeedPlan[]>(
      `/api/v1/releases/${encodeURIComponent(initialId)}/cross-seed-plans`
    ),
    staleTime: 300_000,
    retry: 0
  });

  const selectedVariant = $derived(
    $release.data?.data.variants.find((variant) => variant.torrentId === requestedTorrent)
      ?? $release.data?.data.variants.find((variant) => variant.downloads.length > 0)
  );
  const live = $derived(
    selectReleaseAttachment(selectedVariant?.downloads ?? [], requestedClient, requestedHash)
  );
  const attachmentUnavailable = $derived(
    attachmentRequested && $release.data != null && live == null
  );

  function eta(value?: number): string {
    if (value == null) return "Unknown";
    if (value < 60) return `${value}s`;
    if (value < 3600) return `${Math.round(value / 60)}m`;
    if (value < 86400) return `${Math.round(value / 3600)}h`;
    return `${Math.round(value / 86400)}d`;
  }

  function updateRoute(expanded = variantsExpanded, details = identifiersOpen) {
    replaceView(routePath, {
      torrent: requestedTorrent,
      client: requestedClient,
      hash: requestedHash,
      from: source,
      expanded: expanded ? 1 : undefined,
      details: details ? "client" : undefined,
      add: requestedAddTorrent
    });
  }

  function toggleExpanded(expanded: boolean) {
    variantsExpanded = expanded;
    updateRoute();
  }

  function toggleIdentifiers(event: Event) {
    identifiersOpen = (event.currentTarget as HTMLDetailsElement).open;
    updateRoute();
  }

  function choose(variant: TorrentVariant) {
    navigateView(routePath, {
      torrent: requestedTorrent,
      client: requestedClient,
      hash: requestedHash,
      from: source,
      expanded: variantsExpanded ? Number(initialId) : undefined,
      details: identifiersOpen ? "client" : undefined,
      add: variant.torrentId
    });
  }

  function closeAddDialog() {
    closeOverlay(routePath, {
      torrent: requestedTorrent,
      client: requestedClient,
      hash: requestedHash,
      from: source,
      expanded: variantsExpanded ? Number(initialId) : undefined,
      details: identifiersOpen ? "client" : undefined
    });
  }

  $effect(() => {
    if (!requestedAddTorrent || selected || !$release.data) return;
    selected = $release.data.data.variants.find(
      (variant) => variant.torrentId === requestedAddTorrent
    ) ?? null;
  });
</script>

<svelte:head><title>{$release.data?.data.release.title ?? "Release"} · Wotbox</title></svelte:head>

<a class="back-link" href={appPath(source === "library" ? "/library" : source === "downloads" ? "/downloads" : source === "channels" ? "/channels" : "/search")}>
  <ArrowLeft size={16} /> Back to {source === "library" ? "Library" : source === "downloads" ? "downloads" : source === "channels" ? "channels" : "search"}
</a>
<StaleNotice provenance={$release.data?.provenance} />

{#if $release.isPending}
  <div class="release-hero skeleton-card"></div>
{:else if $release.isError}
  <div class="error-panel">{$release.error.message}</div>
{:else if $release.data}
  {@const detail = $release.data.data}
  <section class="release-hero">
    <div class="cover hero-cover">
      <Disc3 size={48} />
      {#if detail.release.artwork}
        <img
          src={detail.release.artwork}
          alt=""
          referrerpolicy="no-referrer"
          onerror={(event) => (event.currentTarget as HTMLImageElement).remove()}
        />
      {/if}
    </div>
    <div>
      <p class="eyebrow">
        {detail.release.sources.map((source) => source.tracker.toUpperCase()).join(" + ")} release
      </p>
      <h1>{detail.release.title}</h1>
      <p class="release-byline">
        {[detail.release.artist, detail.release.year, detail.release.releaseType, detail.recordLabel].filter(Boolean).join(" · ")}
      </p>
      <div class="tag-list">
        {#each detail.tags.slice(0, 8) as tag}<span>{tag}</span>{/each}
      </div>
    </div>
  </section>

  <section class="section">
    <div class="section-heading">
      <div><p class="eyebrow">Editions</p><h2>Torrent variants</h2></div>
    </div>
    <PreferredVariants
      variants={detail.variants}
      releaseId={detail.release.id}
      tracker={detail.release.tracker}
      groupId={detail.release.groupId}
      title={detail.release.title}
      requestedTorrentId={requestedTorrent}
      source={source}
      expanded={variantsExpanded}
      onexpandedchange={toggleExpanded}
      onadd={(variant) => choose(variant as TorrentVariant)}
    />
  </section>

  {#if live}
    <section class="section">
      <div class="section-heading">
        <div><p class="eyebrow">Attached client state</p><h2>Live transfer</h2></div>
        <StatusPill state={live.state} />
      </div>
      {#if live.diagnostic}
        <DownloadDiagnostic diagnostic={live.diagnostic} />
      {/if}
      <div class="progress-track detail-progress" aria-label={`${Math.round(live.progress * 100)}% complete`}>
        <span style={`width: ${Math.max(1, live.progress * 100)}%`}></span>
      </div>
      <div class="download-detail-grid" aria-label="Download client information">
        <article><Download size={18} /><span>Downloaded</span><strong>{formatBytes(live.downloaded)}</strong><small>{formatSpeed(live.downloadSpeed)}</small></article>
        <article><Upload size={18} /><span>Uploaded</span><strong>{formatBytes(live.uploaded)}</strong><small>{formatSpeed(live.uploadSpeed)}</small></article>
        <article><Gauge size={18} /><span>Progress</span><strong>{Math.round(live.progress * 100)}%</strong><small>Ratio {live.ratio.toFixed(2)}</small></article>
        <article><Clock3 size={18} /><span>ETA</span><strong>{eta(live.eta)}</strong><small>{live.addedAt ? `Added ${relativeTime(live.addedAt)}` : live.clientState}</small></article>
        <article><HardDrive size={18} /><span>Total size</span><strong>{formatBytes(live.size)}</strong><small>{live.completedAt ? `Completed ${relativeTime(live.completedAt)}` : "Not completed"}</small></article>
        <article><FolderOpen size={18} /><span>Save path</span><strong class="path-value">{live.savePath}</strong><small>{live.client}</small></article>
      </div>
      <details class="source-panel" open={identifiersOpen} ontoggle={toggleIdentifiers}>
        <summary>View download client identifiers</summary>
        <dl class="client-identifiers">
          <dt>Client</dt><dd>{live.client}</dd>
          <dt>Info hash</dt><dd>{live.infoHash}</dd>
          <dt>Native state</dt><dd>{live.clientState}</dd>
        </dl>
      </details>
    </section>
  {:else if attachmentUnavailable}
    <section class="section">
      <div class="stale-notice" role="status">
        This download attachment is no longer available from the requested client. The canonical release metadata remains available.
      </div>
    </section>
  {/if}

  {#if detail.description}
    <section class="section prose-panel">
      <p class="eyebrow">About this release</p>
      <div class="release-description">
        {@html sanitizeReleaseDescription(detail.description)}
      </div>
    </section>
  {/if}

  {#if $crossSeedPlans.data?.length}
    <section class="section">
      <div class="section-heading">
        <div><p class="eyebrow">Read-only analysis</p><h2>Cross-seed plans</h2></div>
      </div>
      <div class="panel-list">
        {#each $crossSeedPlans.data as plan}
          <article class="activity-row">
            <div class="release-mark">{plan.targetTracker.slice(0, 1).toUpperCase()}</div>
            <div class="activity-copy">
              <strong>{plan.sourceTracker.toUpperCase()} → {plan.targetTracker.toUpperCase()}</strong>
              <span>
                {plan.matchedFiles}/{plan.targetFiles} files match ·
                {plan.compatible ? "100% compatible" : "incomplete match"} ·
                {plan.policyEligible ? "policy eligible" : "blocked by current policy"}
              </span>
              <small>{plan.summary} Nothing has been added to or changed in qBittorrent.</small>
            </div>
          </article>
        {/each}
      </div>
    </section>
  {/if}
{/if}

<AddDownloadDialog
  selection={selected && $release.data ? {
    name: $release.data.data.release.title,
    artist: $release.data.data.release.artist,
    torrent: selected
  } : null}
  tracker={selected?.tracker ?? $release.data?.data.release.tracker ?? ""}
  onclose={closeAddDialog}
/>
