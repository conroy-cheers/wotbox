<script lang="ts">
  import { createQuery } from "@tanstack/svelte-query";
  import { ArrowDownToLine, Database, Gauge, HardDrive, RefreshCw } from "@lucide/svelte";
  import { api, appPath, formatBytes, relativeTime, type DownloadsPage, type ProviderStatus, type TrackerAccount } from "../lib/api";
  import StatusPill from "../lib/StatusPill.svelte";
  import StaleNotice from "../lib/StaleNotice.svelte";
  import TrackerLinks from "../lib/TrackerLinks.svelte";
  import { releaseTypeColor } from "../lib/releasePresentation";
  import { releaseViewPath } from "../lib/routing";

  const accounts = createQuery({
    queryKey: ["accounts"],
    queryFn: () => api<TrackerAccount[]>("/api/v1/accounts")
  });
  const downloads = createQuery({
    queryKey: ["downloads"],
    queryFn: () => api<DownloadsPage>("/api/v1/downloads?limit=6")
  });
  const providers = createQuery({
    queryKey: ["providers"],
    queryFn: () => api<ProviderStatus[]>("/api/v1/providers")
  });
  const qbit = $derived(($providers.data ?? []).find((provider) => provider.kind === "download_client"));
</script>

<svelte:head><title>Dashboard · Wotbox</title></svelte:head>

<header class="page-heading">
  <div>
    <p class="eyebrow">Overview</p>
    <h1>Good evening{$accounts.data?.[0] ? `, ${$accounts.data[0].account.username}` : ""}.</h1>
    <p>Tracker truth and download state, in one quiet place.</p>
  </div>
  <button class="icon-button" aria-label="Refresh dashboard" onclick={() => {
    $accounts.refetch();
    $downloads.refetch();
    $providers.refetch();
  }}><RefreshCw size={18} /></button>
</header>

{#if $accounts.isError}
  <div class="error-panel">{$accounts.error.message}</div>
{:else}
  {#each $accounts.data ?? [] as trackerAccount}
    <StaleNotice provenance={trackerAccount.provenance} />
  {/each}
  <section class="metric-grid" aria-label="Account statistics">
    {#each $accounts.data ?? [] as trackerAccount}
      <article class="metric-card">
        <span class="metric-icon"><Gauge size={19} /></span>
        <p>{trackerAccount.tracker.toUpperCase()} ratio</p>
        <strong>{trackerAccount.account.ratio?.toFixed(2) ?? "—"}</strong>
        <small>Required {trackerAccount.account.requiredRatio?.toFixed(2) ?? "—"}</small>
      </article>
      <article class="metric-card">
        <span class="metric-icon"><Database size={19} /></span>
        <p>{trackerAccount.tracker.toUpperCase()} uploaded</p>
        <strong>{formatBytes(trackerAccount.account.uploaded)}</strong>
        <small>{trackerAccount.account.userClass ?? "Tracker class"}</small>
      </article>
      <article class="metric-card">
        <span class="metric-icon"><ArrowDownToLine size={19} /></span>
        <p>{trackerAccount.tracker.toUpperCase()} downloaded</p>
        <strong>{formatBytes(trackerAccount.account.downloaded)}</strong>
        <small>{trackerAccount.account.bonusPoints?.toLocaleString() ?? "—"} bonus</small>
      </article>
    {/each}
    <article class="metric-card">
      <span class="metric-icon"><HardDrive size={19} /></span>
      <p>qBittorrent</p>
      <strong class:healthy={qbit?.state === "available"}>
        {$providers.isPending ? "Checking" : $providers.isError ? "Unknown" : qbit?.state === "available" ? "Connected" : qbit?.state ?? "Unconfigured"}
      </strong>
      <small>{qbit?.message ?? qbit?.displayName ?? "Music client"}</small>
    </article>
  </section>
{/if}

<section class="section">
  <div class="section-heading">
    <div>
      <p class="eyebrow">Recent activity</p>
      <h2>Downloads</h2>
    </div>
    <a class="text-link" href={appPath("/downloads")}>View all</a>
  </div>
  <div class="panel-list">
    {#if $downloads.isPending}
      <div class="skeleton-row"></div>
      <div class="skeleton-row"></div>
    {:else if $downloads.data?.items.length}
      {#each $downloads.data.items.slice(0, 6) as item}
        {@const download = item.download}
        <article class="activity-row release-type-coded" style={`--release-type-color: ${releaseTypeColor(item.release.releaseType)}`}>
          <div class="release-mark">{item.release.title.slice(0, 1).toUpperCase()}</div>
          <div class="activity-copy">
            <strong>
              <a href={releaseViewPath(
                item.release.id,
                item.variant?.torrentId,
                "downloads",
                { client: download.client, infoHash: download.infoHash }
              )}>
                {item.release.title}
              </a>
            </strong>
            <span>
              {item.release.artist ?? "Various artists"} · {item.variant?.format ?? "Release matched from torrent name"} ·
              {download.diagnostic?.summary ?? (download.addedAt ? relativeTime(download.addedAt) : download.clientState)}
            </span>
            <TrackerLinks sources={item.release.sources} tracker={item.release.tracker} groupId={item.release.groupId} />
          </div>
          <StatusPill state={download.state} />
        </article>
      {/each}
    {:else}
      <div class="empty-state">
        <ArrowDownToLine size={28} />
        <strong>No downloads yet</strong>
        <span>Search the tracker to add your first release.</span>
      </div>
    {/if}
  </div>
</section>
