<script lang="ts">
  import { createQuery } from "@tanstack/svelte-query";
  import { ArrowLeft, Clock3, Disc3, Download, FolderOpen, Gauge, Upload } from "@lucide/svelte";
  import { untrack } from "svelte";
  import {
    api,
    appPath,
    formatBytes,
    formatSpeed,
    relativeTime,
    type Envelope,
    type ReleaseDetail
  } from "../lib/api";
  import StaleNotice from "../lib/StaleNotice.svelte";
  import StatusPill from "../lib/StatusPill.svelte";
  import PreferredVariants from "../lib/PreferredVariants.svelte";
  import DownloadDiagnostic from "../lib/DownloadDiagnostic.svelte";
  import { sanitizeReleaseDescription } from "../lib/releaseDescription";

  let { tracker, id }: { tracker: string; id: string } = $props();
  const initialTracker = untrack(() => tracker);
  const initialId = untrack(() => id);
  const pageParams = new URLSearchParams(location.search);
  const requestedTorrent = Number(pageParams.get("torrent")) || undefined;
  const fromLibrary = pageParams.get("from") === "library";
  const release = createQuery({
    queryKey: ["release", initialTracker, initialId],
    queryFn: () => api<Envelope<ReleaseDetail>>(`/api/v1/groups/${initialTracker}/${initialId}`),
    refetchInterval: 15_000
  });

  const selectedVariant = $derived(
    $release.data?.data.variants.find((variant) => variant.torrentId === requestedTorrent)
      ?? $release.data?.data.variants.find((variant) => variant.downloads.length > 0)
  );
  const live = $derived(selectedVariant?.downloads[0]);

  function eta(value?: number): string {
    if (value == null) return "Unknown";
    if (value < 60) return `${value}s`;
    if (value < 3600) return `${Math.round(value / 60)}m`;
    if (value < 86400) return `${Math.round(value / 3600)}h`;
    return `${Math.round(value / 86400)}d`;
  }
</script>

<svelte:head><title>{$release.data?.data.release.title ?? "Release"} · Wotbox</title></svelte:head>

<a class="back-link" href={appPath(fromLibrary ? "/library" : "/search")}>
  <ArrowLeft size={16} /> Back to {fromLibrary ? "Library" : "search"}
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
      <p class="eyebrow">{detail.release.tracker.toUpperCase()} release</p>
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
      tracker={detail.release.tracker}
      groupId={detail.release.groupId}
      title={detail.release.title}
      requestedTorrentId={requestedTorrent}
      fromLibrary={fromLibrary}
    />
  </section>

  {#if live}
    <section class="section">
      <div class="section-heading">
        <div><p class="eyebrow">Attached client state</p><h2>Live transfer</h2></div>
        <StatusPill state={live.state} />
      </div>
      {#if live.diagnostic}
        <DownloadDiagnostic
          diagnostic={live.diagnostic}
          href={appPath(`/downloads/${encodeURIComponent(live.client)}/${encodeURIComponent(live.infoHash)}`)}
        />
      {/if}
      <div class="progress-track detail-progress" aria-label={`${Math.round(live.progress * 100)}% complete`}>
        <span style={`width: ${Math.max(1, live.progress * 100)}%`}></span>
      </div>
      <div class="download-detail-grid" aria-label="Download client information">
        <article><Download size={18} /><span>Downloaded</span><strong>{formatBytes(live.downloaded)}</strong><small>{formatSpeed(live.downloadSpeed)}</small></article>
        <article><Upload size={18} /><span>Uploaded</span><strong>{formatBytes(live.uploaded)}</strong><small>{formatSpeed(live.uploadSpeed)}</small></article>
        <article><Gauge size={18} /><span>Progress</span><strong>{Math.round(live.progress * 100)}%</strong><small>Ratio {live.ratio.toFixed(2)}</small></article>
        <article><Clock3 size={18} /><span>ETA</span><strong>{eta(live.eta)}</strong><small>{live.addedAt ? `Added ${relativeTime(live.addedAt)}` : live.clientState}</small></article>
        <article><FolderOpen size={18} /><span>Client path</span><strong class="path-value">{live.savePath}</strong><small>{live.client}</small></article>
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
{/if}
