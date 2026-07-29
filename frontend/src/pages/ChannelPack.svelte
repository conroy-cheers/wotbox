<script lang="ts">
  import { createMutation, createQuery, useQueryClient } from "@tanstack/svelte-query";
  import { ArrowLeft, Check, ExternalLink, RefreshCw, X } from "@lucide/svelte";
  import { untrack } from "svelte";
  import {
    api,
    appPath,
    formatBytes,
    type ChannelBatchResult,
    type ChannelPack,
    type DownloadSelection
  } from "../lib/api";
  import AddDownloadDialog from "../lib/AddDownloadDialog.svelte";
  import PreferredVariants from "../lib/PreferredVariants.svelte";

  let { id }: { id: string } = $props();
  const initialId = untrack(() => id);
  const queryClient = useQueryClient();
  let selection = $state<DownloadSelection | null>(null);
  let selectedTracker = $state("");

  const pack = createQuery({
    queryKey: ["channel-pack", initialId],
    queryFn: () => api<ChannelPack>(`/api/v1/channel-packs/${initialId}`),
    refetchInterval: 3_000
  });
  const replan = createMutation({
    mutationFn: () => api<ChannelPack>(`/api/v1/channel-packs/${initialId}/replan`, { method: "POST" }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["channel-pack", initialId] })
  });
  const decide = createMutation({
    mutationFn: ({ action, version }: { action: "accept" | "reject"; version: number }) =>
      api<ChannelBatchResult | ChannelPack>(`/api/v1/channel-packs/${initialId}/${action}`, {
        method: "POST",
        body: JSON.stringify({ planVersion: version })
      }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["channel-pack", initialId] });
      queryClient.invalidateQueries({ queryKey: ["channels-overview"] });
      queryClient.invalidateQueries({ queryKey: ["downloads"] });
    }
  });

  function searchPath(artist: string, title: string): string {
    const params = new URLSearchParams({ artist, query: title });
    return appPath(`/search?${params}`);
  }

  function choose(item: ChannelPack["items"][number], torrent: DownloadSelection["torrent"]) {
    selection = { name: item.source.title, artist: item.source.artist, torrent };
    selectedTracker = torrent.tracker ?? item.release?.tracker ?? "";
  }

  function accept(pack: ChannelPack) {
    if (window.confirm(`Submit ${pack.summary.executable} planned releases to the download client?`)) {
      $decide.mutate({ action: "accept", version: pack.planVersion });
    }
  }
</script>

<svelte:head><title>Recommendation pack · Wotbox</title></svelte:head>

<a class="back-link" href={appPath("/channels")}><ArrowLeft size={15} /> Channels</a>

{#if $pack.isPending}
  <div class="release-card skeleton-card"></div>
{:else if $pack.isError}
  <div class="error-panel">{$pack.error.message}</div>
{:else if $pack.data}
  {@const current = $pack.data}
  <header class="page-heading pack-heading">
    <div>
      <p class="eyebrow">{new Date(current.createdAt).toLocaleString()} · {current.decision}</p>
      <h1>{current.sourceTitle}</h1>
      <p>{current.items.length} recommendations · {current.partial ? "Partial source refresh" : "Complete source refresh"}</p>
    </div>
    {#if current.decision === "open"}
      <div class="heading-actions">
        <button class="secondary-button" disabled={$decide.isPending} onclick={() => $decide.mutate({ action: "reject", version: current.planVersion })}>
          <X size={16} /> Reject plan
        </button>
        <button class="primary-button" disabled={$decide.isPending || current.planStale || current.summary.executable === 0} onclick={() => accept(current)}>
          <Check size={16} /> Accept {current.summary.executable} downloads
        </button>
      </div>
    {/if}
  </header>

  {#if current.planStale}
    <div class="notice-banner">
      <span><strong>This plan is stale.</strong> Release preferences or download profiles changed after it was generated.</span>
      <button class="secondary-button compact-button" disabled={$replan.isPending} onclick={() => $replan.mutate()}>
        <RefreshCw size={14} /> {$replan.isPending ? "Replanning…" : "Replan"}
      </button>
    </div>
  {/if}
  {#if $decide.isError}<div class="error-panel">{$decide.error.message}</div>{/if}
  {#if $replan.isError}<div class="error-panel">{$replan.error.message}</div>{/if}

  <section class="plan-summary">
    <div><strong>{current.summary.executable}</strong><span>Ready</span></div>
    <div><strong>{formatBytes(current.summary.totalSize)}</strong><span>Download size</span></div>
    <div><strong>{current.summary.tokenUses}</strong><span>Tokens</span></div>
    <div><strong>{current.summary.skipped}</strong><span>Skipped</span></div>
  </section>

  <div class="result-list pack-items">
    {#each current.items as item}
      <article class="release-card pack-item">
        <div class="pack-rank">{item.source.rank}</div>
        <div class="cover">
          {#if item.source.artwork}<img src={item.source.artwork} alt="" loading="lazy" referrerpolicy="no-referrer" />{/if}
        </div>
        <div class="release-content">
          <div class="release-heading">
            <div>
              <p>{item.source.artist}</p>
              <h2>
                {#if item.release?.id}
                  <a href={appPath(`/releases/${item.release.id}?from=channels`)}>{item.source.title}</a>
                {:else}
                  {item.source.title}
                {/if}
              </h2>
              <span>
                {item.source.year ?? ""}
                {#if item.plan}<span class="source-badges"><span>{item.plan.tracker.toUpperCase()}</span></span>{/if}
              </span>
            </div>
            {#if item.source.url}<a class="icon-link" href={item.source.url} target="_blank" rel="noreferrer" aria-label="Open source"><ExternalLink size={15} /></a>{/if}
          </div>
          {#if item.release}
            <PreferredVariants
              variants={item.variants}
              releaseId={item.release.id}
              tracker={item.release.tracker}
              groupId={item.release.groupId}
              title={item.release.title}
              source="channels"
              onadd={(variant) => choose(item, variant)}
            />
          {/if}
          <div class="pack-item-state">
            {#if item.job}
              <span class={`status-pill ${item.job.state}`}>{item.job.state.replaceAll("_", " ")}</span>
              <span>{item.job.errorMessage ?? `Submitted through ${item.job.profile}`}</span>
            {:else if item.planState === "executable" && item.plan}
              <span class="status-pill queued">Planned</span>
              <span>{[item.plan.format, item.plan.encoding, item.plan.media].filter(Boolean).join(" · ")} · {formatBytes(item.plan.size)} · {item.plan.profile}{item.plan.useToken ? " · token" : ""}</span>
            {:else}
              <span class="status-pill unknown">{item.planState.replaceAll("_", " ")}</span>
              <span>{item.reason}</span>
              {#if !item.release}<a href={searchPath(item.source.artist, item.source.title)}>Search trackers</a>{/if}
            {/if}
          </div>
        </div>
      </article>
    {/each}
  </div>

  <AddDownloadDialog selection={selection} tracker={selectedTracker} onclose={() => selection = null} />
{/if}
