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
  import { executableOrdinals, summarizeSelection } from "../lib/channelPack";
  import PreferredVariants from "../lib/PreferredVariants.svelte";
  import ReleaseCandidatePicker from "../lib/ReleaseCandidatePicker.svelte";
  import ReleaseDownloads from "../lib/ReleaseDownloads.svelte";
  import TrackerLinks from "../lib/TrackerLinks.svelte";

  let { id }: { id: string } = $props();
  const initialId = untrack(() => id);
  const queryClient = useQueryClient();
  let selection = $state<DownloadSelection | null>(null);
  let selectedTracker = $state("");
  let selectedOrdinals = $state<Set<number>>(new Set());
  let selectionVersion = $state(-1);

  const pack = createQuery({
    queryKey: ["channel-pack", initialId],
    queryFn: () => api<ChannelPack>(`/api/v1/channel-packs/${initialId}`),
    refetchInterval: 3_000
  });
  const replan = createMutation({
    mutationFn: () => api<ChannelPack>(`/api/v1/channel-packs/${initialId}/replan`, { method: "POST" }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["channel-pack", initialId] })
  });
  const attach = createMutation({
    mutationFn: ({ ordinal, releaseId, version }: { ordinal: number; releaseId: string; version: number }) =>
      api<ChannelPack>(`/api/v1/channel-packs/${initialId}/items/${ordinal}/attach`, {
        method: "POST",
        body: JSON.stringify({ planVersion: version, releaseId })
      }),
    onSuccess: (updated) => {
      queryClient.setQueryData(["channel-pack", initialId], updated);
      queryClient.invalidateQueries({ queryKey: ["channel-pack", initialId] });
    }
  });
  const decide = createMutation({
    mutationFn: ({
      action,
      version,
      ordinals
    }: {
      action: "accept" | "reject";
      version: number;
      ordinals?: number[];
    }) =>
      api<ChannelBatchResult | ChannelPack>(`/api/v1/channel-packs/${initialId}/${action}`, {
        method: "POST",
        body: JSON.stringify({ planVersion: version, ordinals })
      }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["channel-pack", initialId] });
      queryClient.invalidateQueries({ queryKey: ["channels-overview"] });
      queryClient.invalidateQueries({ queryKey: ["downloads"] });
    }
  });
  const selectedSummary = $derived(
    summarizeSelection($pack.data?.items ?? [], selectedOrdinals)
  );

  function searchPath(artist: string, title: string): string {
    const params = new URLSearchParams({ artist, query: title });
    return appPath(`/search?${params}`);
  }

  function choose(item: ChannelPack["items"][number], torrent: DownloadSelection["torrent"]) {
    selection = { name: item.source.title, artist: item.source.artist, torrent };
    selectedTracker = torrent.tracker ?? item.release?.tracker ?? "";
  }

  function accept(pack: ChannelPack) {
    if (window.confirm(`Submit ${selectedOrdinals.size} selected releases to the download client?`)) {
      $decide.mutate({
        action: "accept",
        version: pack.planVersion,
        ordinals: [...selectedOrdinals]
      });
    }
  }

  function toggleItem(ordinal: number, checked: boolean) {
    const next = new Set(selectedOrdinals);
    if (checked) next.add(ordinal);
    else next.delete(ordinal);
    selectedOrdinals = next;
  }

  $effect(() => {
    const current = $pack.data;
    if (!current || current.planVersion === selectionVersion) return;
    selectionVersion = current.planVersion;
    selectedOrdinals = executableOrdinals(current.items);
  });
</script>

<svelte:head><title>Recommendation pack · Wotbox</title></svelte:head>

<a class="back-link" href={appPath("/channels")}><ArrowLeft size={15} /> Channels</a>

{#if $pack.isPending}
  <div class="release-card skeleton-card"></div>
{:else if $pack.isError}
  <div class="error-panel">{$pack.error.message}</div>
{:else if $pack.data}
  {@const current = $pack.data}
  {@const visibleSummary = current.decision === "open" ? selectedSummary : current.summary}
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
        <button class="primary-button" disabled={$decide.isPending || current.planStale || selectedOrdinals.size === 0} onclick={() => accept(current)}>
          <Check size={16} /> Accept {selectedOrdinals.size} selected
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
  {#if $attach.isError}<div class="error-panel">{$attach.error.message}</div>{/if}

  <section class="plan-summary">
    <div><strong>{visibleSummary.executable}</strong><span>{current.decision === "open" ? "Selected" : "Ready"}</span></div>
    <div><strong>{formatBytes(visibleSummary.totalSize)}</strong><span>Download size</span></div>
    <div><strong>{visibleSummary.tokenUses}</strong><span>Tokens</span></div>
    <div><strong>{visibleSummary.skipped}</strong><span>Skipped</span></div>
  </section>

  <div class="result-list pack-items">
    {#each current.items as item}
      <article class="release-card pack-item">
        <div class="pack-rank">
          {#if current.decision === "open" && item.planState === "executable"}
            <input
              type="checkbox"
              aria-label={`Include ${item.source.title}`}
              checked={selectedOrdinals.has(item.ordinal)}
              onchange={(event) => toggleItem(item.ordinal, event.currentTarget.checked)}
            />
          {:else}
            {item.source.rank}
          {/if}
        </div>
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
              {#if item.release}
                <TrackerLinks sources={item.release.sources} tracker={item.release.tracker} groupId={item.release.groupId} />
              {/if}
            </div>
            {#if item.source.url}<a class="icon-link" href={item.source.url} target="_blank" rel="noreferrer" aria-label="Open source"><ExternalLink size={15} /></a>{/if}
          </div>
          {#if item.source.substitutedFrom}
            <div class="substitution-note">
              Recommended from single
              {#if item.source.substitutedFrom.url}
                <a href={item.source.substitutedFrom.url} target="_blank" rel="noreferrer">{item.source.substitutedFrom.title}</a>
              {:else}
                <strong>{item.source.substitutedFrom.title}</strong>
              {/if}
              · mapped to this containing release
            </div>
          {/if}
          <ReleaseDownloads downloads={item.downloads} />
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
          {:else if item.candidates.length}
            <ReleaseCandidatePicker
              candidates={item.candidates}
              pending={$attach.isPending}
              onselect={(candidate) => candidate.id && $attach.mutate({
                ordinal: item.ordinal,
                releaseId: candidate.id,
                version: current.planVersion
              })}
            />
          {/if}
          <div class="pack-item-state">
            {#if item.job}
              <span class={`status-pill ${item.job.state}`}>{item.job.state.replaceAll("_", " ")}</span>
              <span>{item.job.errorMessage ?? `Submitted through ${item.job.profile}`}</span>
            {:else if item.planState === "executable" && item.plan}
              <span class="status-pill queued">Planned</span>
              <span>
                {[item.plan.format, item.plan.encoding, item.plan.media].filter(Boolean).join(" · ")}
                · {formatBytes(item.plan.size)} · {item.plan.profile}
                {item.plan.useToken
                  ? ` · ${item.plan.tokenCost} ${item.plan.tokenCost === 1 ? "token" : "tokens"}`
                  : ""}
              </span>
            {:else}
              <span class="status-pill unknown">{item.planState.replaceAll("_", " ")}</span>
              <span>{item.reason}</span>
              {#if !item.release}
                <a href={`${searchPath(item.source.artist, item.source.title)}&pack=${current.id}&item=${item.ordinal}&version=${current.planVersion}`}>Search and attach</a>
              {/if}
            {/if}
          </div>
        </div>
      </article>
    {/each}
  </div>

  <AddDownloadDialog
    selection={selection}
    tracker={selectedTracker}
    onclose={() => selection = null}
    oncomplete={() => $replan.mutate()}
  />
{/if}
