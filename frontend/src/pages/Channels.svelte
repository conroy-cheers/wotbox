<script lang="ts">
  import { createMutation, createQuery } from "@tanstack/svelte-query";
  import { History, Radio, RefreshCw, Settings2 } from "@lucide/svelte";
  import {
    api,
    appPath,
    formatBytes,
    relativeTime,
    type ChannelOverview,
    type ChannelPackSummary,
    type ChannelRun
  } from "../lib/api";

  let expandedHistory = $state<Set<string>>(new Set());
  let fullHistories = $state<Record<string, ChannelPackSummary[]>>({});
  const data = createQuery({
    queryKey: ["channels-overview"],
    queryFn: () => api<ChannelOverview[]>("/api/v1/channels")
  });
  const refresh = createMutation({
    mutationFn: (id: string) =>
      api<ChannelRun>(`/api/v1/channels/${id}/refresh`, { method: "POST" })
  });

  function label(channel: ChannelOverview["channel"]): string {
    switch (channel.kind) {
      case "country_chart": return `Country Top ${channel.countryChart?.albumCount ?? 100}`;
      case "lastfm": return "Last.fm Discovery";
      case "trumped_downloads": return "Trumped downloads";
    }
  }

  function schedule(channel: ChannelOverview["channel"]): string {
    const days = ["", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
    return `${days[channel.schedule.weekday]} ${channel.schedule.time} · ${channel.schedule.timezone}`;
  }

  function refreshIssue(channel: ChannelOverview["channel"]): string | undefined {
    if (channel.kind !== "lastfm") return undefined;
    if (!channel.credentialConfigured) return "The Last.fm API key is not configured.";
    if (!channel.lastfm?.username.trim()) return "Set a Last.fm username before refreshing.";
    return undefined;
  }

  async function toggleHistory(overview: ChannelOverview) {
    const channelId = overview.channel.id;
    const next = new Set(expandedHistory);
    if (next.has(channelId)) {
      next.delete(channelId);
    } else {
      if (!fullHistories[channelId]) {
        fullHistories = {
          ...fullHistories,
          [channelId]: await api<ChannelPackSummary[]>(`/api/v1/channels/${channelId}/packs?limit=100`)
        };
      }
      next.add(channelId);
    }
    expandedHistory = next;
  }

  function phaseLabel(run: ChannelRun): string {
    switch (run.phase) {
      case "discovering": return "Discovering recommendations";
      case "matching": return "Matching tracker releases";
      case "waiting_provider": return "Waiting for tracker";
      case "planning": return "Building download plan";
      case "saving": return "Saving pack";
      default: return "Starting refresh";
    }
  }

  function progressPercent(run: ChannelRun): number | undefined {
    if (!run.progressTotal) return undefined;
    return Math.min(100, Math.round(run.progressCompleted / run.progressTotal * 100));
  }

  function elapsed(run: ChannelRun): string {
    const seconds = Math.max(0, Math.floor((Date.now() - new Date(run.startedAt).getTime()) / 1000));
    if (seconds < 60) return `${seconds}s`;
    return `${Math.floor(seconds / 60)}m ${seconds % 60}s`;
  }
</script>

<svelte:head><title>Channels · Wotbox</title></svelte:head>

<header class="page-heading">
  <div>
    <p class="eyebrow">Recommendation channels</p>
    <h1>Fresh packs, on your schedule</h1>
    <p>Browse recommendations and replacement candidates, then approve a complete download plan when it looks right.</p>
  </div>
  <a class="secondary-button" href={appPath("/preferences#channels")}><Settings2 size={16} /> Configure</a>
</header>

{#if $data.isPending}
  <div class="result-list">
    {#each [1, 2] as _}<div class="release-card skeleton-card"></div>{/each}
  </div>
{:else if $data.isError}
  <div class="error-panel">{$data.error.message}</div>
{:else}
  <div class="channel-grid">
    {#each $data.data ?? [] as overview}
      <section class="channel-card">
        <header>
          <div class="channel-icon"><Radio size={22} /></div>
          <div>
            <p class="eyebrow">{overview.channel.enabled ? "Scheduled" : "Disabled"}</p>
            <h2>{label(overview.channel)}</h2>
          </div>
          <button
            class="secondary-button compact-button"
            disabled={Boolean(overview.activeRun) || $refresh.isPending || Boolean(refreshIssue(overview.channel))}
            title={refreshIssue(overview.channel)}
            onclick={() => $refresh.mutate(overview.channel.id)}
          ><RefreshCw size={14} class={overview.activeRun ? "spinning" : ""} /> {overview.activeRun ? "Refreshing…" : "Refresh now"}</button>
        </header>
        <p class="channel-schedule">
          {schedule(overview.channel)}
          {#if overview.channel.enabled && overview.channel.nextRefreshAt}
            · next {relativeTime(overview.channel.nextRefreshAt)}
          {/if}
        </p>
        {#if refreshIssue(overview.channel)}
          <div class="notice-banner compact">
            {refreshIssue(overview.channel)}
            <a href={appPath("/preferences#channels")}>Configure channel</a>
          </div>
        {/if}
        {#if overview.channel.lastError}
          <div class="error-panel compact">
            {overview.channel.lastError}
            {#if overview.channel.failureCount}
              <small>Retry {overview.channel.nextRefreshAt ? relativeTime(overview.channel.nextRefreshAt) : "pending"} · attempt {overview.channel.failureCount + 1}</small>
            {/if}
          </div>
        {/if}
        {#if overview.activeRun}
          {@const percent = progressPercent(overview.activeRun)}
          <div class="channel-progress" aria-live="polite">
            <div class="channel-progress-heading">
              <span>
                <strong>{phaseLabel(overview.activeRun)}</strong>
                <small>{overview.activeRun.progressMessage ?? "Preparing…"}</small>
                {#if overview.activeRun.retryAt}
                  <small>Retrying at {new Date(overview.activeRun.retryAt).toLocaleString()}</small>
                {/if}
              </span>
              <span>
                {#if overview.activeRun.progressTotal}
                  {overview.activeRun.progressCompleted}/{overview.activeRun.progressTotal}
                {/if}
                <small>{elapsed(overview.activeRun)}</small>
              </span>
            </div>
            <div class="channel-progress-track" class:indeterminate={percent === undefined}>
              <span style:width={percent === undefined ? "32%" : `${percent}%`}></span>
            </div>
          </div>
        {/if}
        {#if overview.latestPack}
          <a class="latest-pack" href={appPath(`/channels/${overview.channel.id}/packs/${overview.latestPack.id}`)}>
            <span>
              <strong>{overview.latestPack.sourceTitle}</strong>
              <small>{relativeTime(overview.latestPack.createdAt)} · {overview.latestPack.decision}</small>
            </span>
            <span class="pack-metrics">
              <strong>{overview.latestPack.summary.executable}</strong> ready
              <small>{formatBytes(overview.latestPack.summary.totalSize)}</small>
            </span>
          </a>
        {:else}
          <div class="empty-inline">
            {overview.channel.kind === "trumped_downloads"
              ? "No snapshot yet. Refresh after download inspection to list unregistered OPS torrents."
              : "No packs yet. Refresh the channel to build the first one."}
          </div>
        {/if}
        {#if overview.packCount > 1}
          <div class="pack-history">
            <h3><History size={15} /> History</h3>
            {#each (expandedHistory.has(overview.channel.id)
              ? fullHistories[overview.channel.id] ?? overview.recentPacks
              : overview.recentPacks).slice(1) as pack}
              <a href={appPath(`/channels/${overview.channel.id}/packs/${pack.id}`)}>
                <span>{new Date(pack.createdAt).toLocaleDateString()}</span>
                <span>{pack.summary.executable} ready · {pack.decision}</span>
              </a>
            {/each}
            {#if overview.packCount > 8}
              <button class="secondary-button compact-button" onclick={() => toggleHistory(overview)}>
                {expandedHistory.has(overview.channel.id) ? "Show recent" : `Show all ${overview.packCount} packs`}
              </button>
            {/if}
          </div>
        {/if}
      </section>
    {/each}
  </div>
  {#if $refresh.isError}<div class="error-panel">{$refresh.error.message}</div>{/if}
{/if}
