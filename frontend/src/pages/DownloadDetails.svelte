<script lang="ts">
  import { createQuery } from "@tanstack/svelte-query";
  import { ArrowLeft, Clock3, Download, FolderOpen, Gauge, HardDrive, Upload } from "@lucide/svelte";
  import { untrack } from "svelte";
  import {
    api,
    appPath,
    formatBytes,
    formatSpeed,
    relativeTime,
    type CanonicalDownload
  } from "../lib/api";
  import DownloadDiagnostic from "../lib/DownloadDiagnostic.svelte";
  import StatusPill from "../lib/StatusPill.svelte";

  let { client, infoHash }: { client: string; infoHash: string } = $props();
  const initialClient = untrack(() => client);
  const initialHash = untrack(() => infoHash);
  const download = createQuery({
    queryKey: ["download", initialClient, initialHash],
    queryFn: () =>
      api<CanonicalDownload>(
        `/api/v1/downloads/${encodeURIComponent(initialClient)}/${encodeURIComponent(initialHash)}`
      ),
    refetchInterval: 15_000
  });

  function eta(value?: number): string {
    if (value == null) return "Unknown";
    if (value < 60) return `${value}s`;
    if (value < 3600) return `${Math.round(value / 60)}m`;
    if (value < 86400) return `${Math.round(value / 3600)}h`;
    return `${Math.round(value / 86400)}d`;
  }
</script>

<svelte:head><title>{$download.data?.release.title ?? "Download"} · Wotbox</title></svelte:head>

<a class="back-link" href={appPath("/downloads")}><ArrowLeft size={16} /> Back to downloads</a>

{#if $download.isPending}
  <div class="release-hero skeleton-card"></div>
{:else if $download.isError}
  <div class="error-panel">{$download.error.message}</div>
{:else if $download.data}
  {@const live = $download.data.download}
  <header class="download-detail-heading">
    <div class="release-mark large">{$download.data.release.title.slice(0, 1).toUpperCase()}</div>
    <div>
      <p class="eyebrow">{$download.data.release.tracker.toUpperCase()} · {live.clientState}</p>
      <h1>{$download.data.release.title}</h1>
      <div class="tag-list detail-tags">
        {#if $download.data.release.artist}<span>{$download.data.release.artist}</span>{/if}
        {#if $download.data.variant.format}<span>{$download.data.variant.format}</span>{/if}
        {#if $download.data.variant.encoding}<span>{$download.data.variant.encoding}</span>{/if}
      </div>
    </div>
    <StatusPill state={live.state} />
  </header>

  {#if live.diagnostic}
    <DownloadDiagnostic diagnostic={live.diagnostic} />
  {/if}

  <div class="progress-track detail-progress" aria-label={`${Math.round(live.progress * 100)}% complete`}>
    <span style={`width: ${Math.max(1, live.progress * 100)}%`}></span>
  </div>

  <section class="download-detail-grid" aria-label="Download client information">
    <article><Download size={18} /><span>Downloaded</span><strong>{formatBytes(live.downloaded)}</strong><small>{formatSpeed(live.downloadSpeed)}</small></article>
    <article><Upload size={18} /><span>Uploaded</span><strong>{formatBytes(live.uploaded)}</strong><small>{formatSpeed(live.uploadSpeed)}</small></article>
    <article><Gauge size={18} /><span>Progress</span><strong>{Math.round(live.progress * 100)}%</strong><small>Ratio {live.ratio.toFixed(2)}</small></article>
    <article><Clock3 size={18} /><span>ETA</span><strong>{eta(live.eta)}</strong><small>{live.addedAt ? `Added ${relativeTime(live.addedAt)}` : "Add time unavailable"}</small></article>
    <article><HardDrive size={18} /><span>Total size</span><strong>{formatBytes(live.size)}</strong><small>{live.completedAt ? `Completed ${relativeTime(live.completedAt)}` : "Not completed"}</small></article>
    <article><FolderOpen size={18} /><span>Save path</span><strong class="path-value">{live.savePath}</strong><small>{live.client}</small></article>
  </section>

  <details class="source-panel">
    <summary>View download client identifiers</summary>
    <dl class="client-identifiers">
      <dt>Client</dt><dd>{live.client}</dd>
      <dt>Info hash</dt><dd>{live.infoHash}</dd>
      <dt>Native state</dt><dd>{live.clientState}</dd>
    </dl>
  </details>
{/if}
