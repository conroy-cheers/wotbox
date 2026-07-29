<script lang="ts">
  import { createMutation, createQuery, useQueryClient } from "@tanstack/svelte-query";
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

  const queryClient = useQueryClient();
  const data = createQuery({
    queryKey: ["channels-overview"],
    queryFn: async () => {
      const channels = await api<ChannelOverview[]>("/api/v1/channels");
      const histories = Object.fromEntries(await Promise.all(channels.map(async ({ channel }) => [
        channel.id,
        await api<ChannelPackSummary[]>(`/api/v1/channels/${channel.id}/packs?limit=8`)
      ])));
      return { channels, histories } as {
        channels: ChannelOverview[];
        histories: Record<string, ChannelPackSummary[]>;
      };
    },
    refetchInterval: 5_000
  });
  const refresh = createMutation({
    mutationFn: (id: string) =>
      api<ChannelRun>(`/api/v1/channels/${id}/refresh`, { method: "POST" }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["channels-overview"] });
      queryClient.invalidateQueries({ queryKey: ["channels"] });
    }
  });

  function label(id: string): string {
    return id === "country_chart" ? "Country Top 100" : "Last.fm Discovery";
  }

  function schedule(channel: ChannelOverview["channel"]): string {
    const days = ["", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
    return `${days[channel.schedule.weekday]} ${channel.schedule.time} · ${channel.schedule.timezone}`;
  }
</script>

<svelte:head><title>Channels · Wotbox</title></svelte:head>

<header class="page-heading">
  <div>
    <p class="eyebrow">Recommendation channels</p>
    <h1>Fresh packs, on your schedule</h1>
    <p>Browse album-native recommendations and approve a complete download plan when it looks right.</p>
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
    {#each $data.data?.channels ?? [] as overview}
      <section class="channel-card">
        <header>
          <div class="channel-icon"><Radio size={22} /></div>
          <div>
            <p class="eyebrow">{overview.channel.enabled ? "Scheduled" : "Disabled"}</p>
            <h2>{label(overview.channel.id)}</h2>
          </div>
          <button
            class="secondary-button compact-button"
            disabled={Boolean(overview.activeRun) || $refresh.isPending}
            onclick={() => $refresh.mutate(overview.channel.id)}
          ><RefreshCw size={14} class={overview.activeRun ? "spinning" : ""} /> {overview.activeRun ? "Refreshing…" : "Refresh now"}</button>
        </header>
        <p class="channel-schedule">{schedule(overview.channel)}</p>
        {#if overview.channel.lastError}
          <div class="error-panel compact">{overview.channel.lastError}</div>
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
          <div class="empty-inline">No packs yet. Refresh the channel to build the first one.</div>
        {/if}
        {#if ($data.data?.histories[overview.channel.id]?.length ?? 0) > 1}
          <div class="pack-history">
            <h3><History size={15} /> History</h3>
            {#each $data.data?.histories[overview.channel.id]?.slice(1) ?? [] as pack}
              <a href={appPath(`/channels/${overview.channel.id}/packs/${pack.id}`)}>
                <span>{new Date(pack.createdAt).toLocaleDateString()}</span>
                <span>{pack.summary.executable} ready · {pack.decision}</span>
              </a>
            {/each}
          </div>
        {/if}
      </section>
    {/each}
  </div>
  {#if $refresh.isError}<div class="error-panel">{$refresh.error.message}</div>{/if}
{/if}
