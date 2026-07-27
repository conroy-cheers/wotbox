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
    type ClientDownload
  } from "../lib/api";
  import StatusPill from "../lib/StatusPill.svelte";

  let { client, infoHash }: { client: string; infoHash: string } = $props();
  const initialClient = untrack(() => client);
  const initialHash = untrack(() => infoHash);
  const download = createQuery({
    queryKey: ["download", initialClient, initialHash],
    queryFn: () =>
      api<ClientDownload>(
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

<svelte:head><title>{$download.data?.name ?? "Download"} · Wotbox</title></svelte:head>

<a class="back-link" href={appPath("/downloads")}><ArrowLeft size={16} /> Back to downloads</a>

{#if $download.isPending}
  <div class="release-hero skeleton-card"></div>
{:else if $download.isError}
  <div class="error-panel">{$download.error.message}</div>
{:else if $download.data}
  <header class="download-detail-heading">
    <div class="release-mark large">{$download.data.name.slice(0, 1).toUpperCase()}</div>
    <div>
      <p class="eyebrow">{$download.data.client} · {$download.data.clientState}</p>
      <h1>{$download.data.name}</h1>
      <div class="tag-list detail-tags">
        {#if $download.data.category}<span>{$download.data.category}</span>{/if}
        {#each $download.data.tags as tag}<span>{tag}</span>{/each}
      </div>
    </div>
    <StatusPill state={$download.data.state} />
  </header>

  <div class="progress-track detail-progress" aria-label={`${Math.round($download.data.progress * 100)}% complete`}>
    <span style={`width: ${Math.max(1, $download.data.progress * 100)}%`}></span>
  </div>

  <section class="download-detail-grid" aria-label="Download client information">
    <article><Download size={18} /><span>Downloaded</span><strong>{formatBytes($download.data.downloaded)}</strong><small>{formatSpeed($download.data.downloadSpeed)}</small></article>
    <article><Upload size={18} /><span>Uploaded</span><strong>{formatBytes($download.data.uploaded)}</strong><small>{formatSpeed($download.data.uploadSpeed)}</small></article>
    <article><Gauge size={18} /><span>Progress</span><strong>{Math.round($download.data.progress * 100)}%</strong><small>Ratio {$download.data.ratio.toFixed(2)}</small></article>
    <article><Clock3 size={18} /><span>ETA</span><strong>{eta($download.data.eta)}</strong><small>{$download.data.addedAt ? `Added ${relativeTime($download.data.addedAt)}` : "Add time unavailable"}</small></article>
    <article><HardDrive size={18} /><span>Total size</span><strong>{formatBytes($download.data.size)}</strong><small>{$download.data.completedAt ? `Completed ${relativeTime($download.data.completedAt)}` : "Not completed"}</small></article>
    <article><FolderOpen size={18} /><span>Save path</span><strong class="path-value">{$download.data.savePath}</strong><small>{$download.data.tracker ?? "Tracker unavailable"}</small></article>
  </section>

  <details class="source-panel">
    <summary>View download client identifiers</summary>
    <dl class="client-identifiers">
      <dt>Client</dt><dd>{$download.data.client}</dd>
      <dt>Info hash</dt><dd>{$download.data.infoHash}</dd>
      <dt>Native state</dt><dd>{$download.data.clientState}</dd>
    </dl>
  </details>
{/if}
