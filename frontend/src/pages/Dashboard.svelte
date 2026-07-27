<script lang="ts">
  import { createQuery } from "@tanstack/svelte-query";
  import { ArrowDownToLine, Database, Gauge, HardDrive, RefreshCw } from "@lucide/svelte";
  import { api, formatBytes, relativeTime, type Account, type ClientDownload, type Envelope } from "../lib/api";
  import StatusPill from "../lib/StatusPill.svelte";
  import StaleNotice from "../lib/StaleNotice.svelte";

  const account = createQuery({
    queryKey: ["account"],
    queryFn: () => api<Envelope<Account>>("/api/v1/account")
  });
  const downloads = createQuery({
    queryKey: ["downloads"],
    queryFn: () => api<ClientDownload[]>("/api/v1/downloads?limit=6"),
    refetchInterval: 15_000
  });
  const health = createQuery({
    queryKey: ["health"],
    queryFn: () => api<{ status: string; qbittorrent?: string }>("/health/ready"),
    retry: 1
  });
</script>

<svelte:head><title>Dashboard · Wotbox</title></svelte:head>

<header class="page-heading">
  <div>
    <p class="eyebrow">Overview</p>
    <h1>Good evening{$account.data ? `, ${$account.data.data.username}` : ""}.</h1>
    <p>Tracker truth and download state, in one quiet place.</p>
  </div>
  <button class="icon-button" aria-label="Refresh dashboard" onclick={() => {
    $account.refetch();
    $downloads.refetch();
    $health.refetch();
  }}><RefreshCw size={18} /></button>
</header>

{#if $account.isError}
  <div class="error-panel">{$account.error.message}</div>
{:else}
  <StaleNotice provenance={$account.data?.provenance} />
  <section class="metric-grid" aria-label="Account statistics">
    <article class="metric-card">
      <span class="metric-icon"><Gauge size={19} /></span>
      <p>Ratio</p>
      <strong>{$account.data?.data.ratio?.toFixed(2) ?? "—"}</strong>
      <small>Required {$account.data?.data.requiredRatio?.toFixed(2) ?? "—"}</small>
    </article>
    <article class="metric-card">
      <span class="metric-icon"><Database size={19} /></span>
      <p>Uploaded</p>
      <strong>{formatBytes($account.data?.data.uploaded)}</strong>
      <small>{$account.data?.data.userClass ?? "Tracker class"}</small>
    </article>
    <article class="metric-card">
      <span class="metric-icon"><ArrowDownToLine size={19} /></span>
      <p>Downloaded</p>
      <strong>{formatBytes($account.data?.data.downloaded)}</strong>
      <small>{$account.data?.data.bonusPoints?.toLocaleString() ?? "—"} bonus</small>
    </article>
    <article class="metric-card">
      <span class="metric-icon"><HardDrive size={19} /></span>
      <p>qBittorrent</p>
      <strong class:healthy={$health.data?.status === "ok"}>
        {$health.isPending ? "Checking" : $health.isError ? "Offline" : "Connected"}
      </strong>
      <small>{$health.data?.qbittorrent ?? "Music client"}</small>
    </article>
  </section>
{/if}

<section class="section">
  <div class="section-heading">
    <div>
      <p class="eyebrow">Recent activity</p>
      <h2>Downloads</h2>
    </div>
    <a class="text-link" href="downloads">View all</a>
  </div>
  <div class="panel-list">
    {#if $downloads.isPending}
      <div class="skeleton-row"></div>
      <div class="skeleton-row"></div>
    {:else if $downloads.data?.length}
      {#each $downloads.data.slice(0, 6) as download}
        <article class="activity-row">
          <div class="release-mark">{download.name.slice(0, 1).toUpperCase()}</div>
          <div class="activity-copy">
            <strong>{download.name}</strong>
            <span>{download.client} · {download.addedAt ? relativeTime(download.addedAt) : download.clientState}</span>
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
