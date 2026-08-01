<script lang="ts">
  import { createQuery } from "@tanstack/svelte-query";
  import { ArrowDownToLine, ArrowRight, Clock3, RefreshCw } from "@lucide/svelte";
  import { derived, writable } from "svelte/store";
  import { api, formatBytes, formatSpeed, relativeTime, type DownloadsPage } from "../lib/api";
  import DownloadDiagnostic from "../lib/DownloadDiagnostic.svelte";
  import StatusPill from "../lib/StatusPill.svelte";
  import { releaseTypeColor } from "../lib/releasePresentation";
  import { positiveInteger, releaseViewPath, replaceView } from "../lib/routing";

  const initial = new URLSearchParams(location.search);
  const limit = writable(Math.min(positiveInteger(initial, "limit", 100), 500));
  let urlSyncReady = false;
  const queryOptions = derived(limit, ($limit) => ({
    queryKey: ["downloads", $limit] as const,
    queryFn: () => api<DownloadsPage>(`/api/v1/downloads?limit=${$limit}`),
    refetchInterval: 15_000
  }));
  const downloads = createQuery(queryOptions);

  $effect(() => {
    const value = $limit;
    if (urlSyncReady) {
      replaceView("/downloads", { limit: value === 100 ? undefined : value });
    } else {
      urlSyncReady = true;
    }
  });

  function eta(value?: number): string {
    if (value == null) return "—";
    if (value < 60) return `${value}s`;
    if (value < 3600) return `${Math.round(value / 60)}m`;
    return `${Math.round(value / 3600)}h`;
  }

  function releasePath(item: DownloadsPage["items"][number], showClientDetails = false): string {
    return releaseViewPath(
      item.release.id,
      item.variant.torrentId,
      "downloads",
      { client: item.download.client, infoHash: item.download.infoHash },
      false,
      showClientDetails
    );
  }
</script>

<svelte:head><title>Downloads · Wotbox</title></svelte:head>

<header class="page-heading">
  <div>
    <p class="eyebrow">Tracker releases</p>
    <h1>Downloads</h1>
    <p>Canonical releases with source provenance and live transfer state attached.</p>
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
              <a href={releasePath(item)}>
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
            href={releasePath(item, true)}
          />
        {/if}
        <footer>
          <div class="download-card-meta">
            <span>{item.liveStale ? `Live status cached${item.liveObservedAt ? ` · ${relativeTime(item.liveObservedAt)}` : ""}` : item.provenance.stale ? "Tracker metadata stale · refreshing" : download.addedAt ? `Added ${relativeTime(download.addedAt)}` : download.clientState}</span>
            <span>Ratio {download.ratio.toFixed(2)} · ↑ {formatSpeed(download.uploadSpeed)}</span>
          </div>
          <a class="download-release-link" href={releasePath(item)}>
            View release <ArrowRight size={13} />
          </a>
        </footer>
      </article>
    {/each}
  </div>
  {#if $downloads.data.items.length < $downloads.data.total && $limit < 500}
    <div class="load-more">
      <button class="secondary-button" onclick={() => $limit = Math.min($limit + 100, 500)}>
        Load 100 more
      </button>
      <span>Showing {$downloads.data.items.length} of {$downloads.data.total} linked torrents</span>
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
