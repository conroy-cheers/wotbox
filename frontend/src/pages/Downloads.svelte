<script lang="ts">
  import { createQuery } from "@tanstack/svelte-query";
  import { ArrowDownToLine, Clock3, RefreshCw } from "@lucide/svelte";
  import { derived, writable } from "svelte/store";
  import { api, appPath, formatBytes, formatSpeed, relativeTime, type DownloadsPage } from "../lib/api";
  import DownloadDiagnostic from "../lib/DownloadDiagnostic.svelte";
  import StatusPill from "../lib/StatusPill.svelte";
  import { releaseTypeColor } from "../lib/releasePresentation";

  const limit = writable(100);
  const queryOptions = derived(limit, ($limit) => ({
    queryKey: ["downloads", $limit] as const,
    queryFn: () => api<DownloadsPage>(`/api/v1/downloads?limit=${$limit}`),
    refetchInterval: 15_000
  }));
  const downloads = createQuery(queryOptions);

  function eta(value?: number): string {
    if (value == null) return "—";
    if (value < 60) return `${value}s`;
    if (value < 3600) return `${Math.round(value / 60)}m`;
    return `${Math.round(value / 3600)}h`;
  }
</script>

<svelte:head><title>Downloads · Wotbox</title></svelte:head>

<header class="page-heading">
  <div>
    <p class="eyebrow">Tracker releases</p>
    <h1>Downloads</h1>
    <p>Canonical tracker releases with live transfer state attached.</p>
  </div>
  <button class="icon-button" aria-label="Refresh downloads" onclick={() => $downloads.refetch()}>
    <span class:spin={$downloads.isFetching}><RefreshCw size={18} /></span>
  </button>
</header>

{#if $downloads.isError}
  <div class="error-panel">{$downloads.error.message}</div>
{:else if $downloads.isPending}
  <div class="download-grid">{#each [1, 2, 3] as _}<div class="download-card skeleton-card"></div>{/each}</div>
{:else if $downloads.data?.items.length}
  {#if $downloads.data.index.pending + $downloads.data.index.resolving > 0}
    <div class="stale-notice">
      Indexing tracker releases… {$downloads.data.index.linked} linked,
      {$downloads.data.index.pending + $downloads.data.index.resolving} remaining.
    </div>
  {/if}
  <div class="download-grid">
    {#each $downloads.data.items as item}
      {@const download = item.download}
      <article class="download-card release-type-coded" style={`--release-type-color: ${releaseTypeColor(item.release.releaseType)}`}>
        <div class="download-card-heading">
          <div class="release-mark large">{item.release.title.slice(0, 1).toUpperCase()}</div>
          <div>
            <h2>
              <a href={appPath(`/releases/${item.release.tracker}/${item.release.groupId}?torrent=${item.variant.torrentId}`)}>
                {item.release.title}
              </a>
            </h2>
            <p>{item.release.artist ?? "Various artists"} · {[item.release.year, item.release.releaseType, item.variant.format, item.variant.encoding].filter(Boolean).join(" · ")} · {formatBytes(download.size)}</p>
          </div>
          <StatusPill state={download.state} />
        </div>
        <div class="progress-track" aria-label={`${Math.round(download.progress * 100)}% complete`}>
          <span style={`width: ${Math.max(1, download.progress * 100)}%`}></span>
        </div>
        <div class="progress-copy">
          <strong>{Math.round(download.progress * 100)}%</strong>
          <span>{formatSpeed(download.downloadSpeed)}</span>
          <span><Clock3 size={14} /> {eta(download.eta)}</span>
        </div>
        {#if download.diagnostic}
          <DownloadDiagnostic
            diagnostic={download.diagnostic}
            compact
            href={appPath(`/downloads/${encodeURIComponent(download.client)}/${encodeURIComponent(download.infoHash)}`)}
          />
        {/if}
        <footer>
          <span>{item.provenance.stale ? "Tracker metadata stale · refreshing" : download.addedAt ? `Added ${relativeTime(download.addedAt)}` : download.clientState}</span>
          <span>Ratio {download.ratio.toFixed(2)} · ↑ {formatSpeed(download.uploadSpeed)}</span>
        </footer>
      </article>
    {/each}
  </div>
  {#if $downloads.data.items.length >= $limit && $limit < 500}
    <div class="load-more">
      <button class="secondary-button" onclick={() => $limit = Math.min($limit + 100, 500)}>
        Load 100 more
      </button>
      <span>Showing the {$limit} most recently added torrents</span>
    </div>
  {/if}
{:else}
  <div class="search-welcome">
    <ArrowDownToLine size={32} />
    <h2>No linked releases yet</h2>
    {#if $downloads.data && $downloads.data.index.pending + $downloads.data.index.resolving > 0}
      <p>Wotbox is indexing {$downloads.data.index.pending + $downloads.data.index.resolving} configured-tracker torrents. Resolved releases will appear progressively.</p>
    {:else}
      <p>No configured-tracker torrents have been linked to canonical releases.</p>
    {/if}
  </div>
{/if}
