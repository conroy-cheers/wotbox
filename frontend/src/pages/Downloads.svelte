<script lang="ts">
  import { createQuery } from "@tanstack/svelte-query";
  import { ArrowDownToLine, Clock3, RefreshCw } from "@lucide/svelte";
  import { derived, writable } from "svelte/store";
  import { api, appPath, formatBytes, formatSpeed, relativeTime, type ClientDownload } from "../lib/api";
  import StatusPill from "../lib/StatusPill.svelte";

  const limit = writable(100);
  const queryOptions = derived(limit, ($limit) => ({
    queryKey: ["downloads", $limit] as const,
    queryFn: () => api<ClientDownload[]>(`/api/v1/downloads?limit=${$limit}`),
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
    <p class="eyebrow">qBittorrent</p>
    <h1>Downloads</h1>
    <p>Live torrents reported directly by your download client.</p>
  </div>
  <button class="icon-button" aria-label="Refresh downloads" onclick={() => $downloads.refetch()}>
    <span class:spin={$downloads.isFetching}><RefreshCw size={18} /></span>
  </button>
</header>

{#if $downloads.isError}
  <div class="error-panel">{$downloads.error.message}</div>
{:else if $downloads.isPending}
  <div class="download-grid">{#each [1, 2, 3] as _}<div class="download-card skeleton-card"></div>{/each}</div>
{:else if $downloads.data?.length}
  <div class="download-grid">
    {#each $downloads.data as download}
      <article class="download-card">
        <div class="download-card-heading">
          <div class="release-mark large">{download.name.slice(0, 1).toUpperCase()}</div>
          <div>
            <h2>
              <a href={appPath(`/downloads/${encodeURIComponent(download.client)}/${download.infoHash}`)}>
                {download.name}
              </a>
            </h2>
            <p>{download.client} · {download.category || "Uncategorised"} · {formatBytes(download.size)}</p>
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
        <footer>
          <span>{download.addedAt ? `Added ${relativeTime(download.addedAt)}` : download.clientState}</span>
          <span>Ratio {download.ratio.toFixed(2)} · ↑ {formatSpeed(download.uploadSpeed)}</span>
        </footer>
      </article>
    {/each}
  </div>
  {#if $downloads.data.length >= $limit && $limit < 500}
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
    <h2>Your queue is empty</h2>
    <p>qBittorrent is not currently reporting any torrents.</p>
  </div>
{/if}
