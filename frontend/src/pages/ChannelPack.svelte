<script lang="ts">
  import { createMutation, createQuery, useQueryClient } from "@tanstack/svelte-query";
  import { ArrowLeft, Check, ExternalLink, RefreshCw, X } from "@lucide/svelte";
  import { untrack } from "svelte";
  import {
    api,
    ApiError,
    appPath,
    formatBytes,
    type ChannelBatchResult,
    type ChannelPack,
    type ClientDownloadState,
    type DownloadSelection
  } from "../lib/api";
  import AddDownloadDialog from "../lib/AddDownloadDialog.svelte";
  import { executableOrdinals, summarizeSelection } from "../lib/channelPack";
  import PreferredVariants from "../lib/PreferredVariants.svelte";
  import ReleaseCandidatePicker from "../lib/ReleaseCandidatePicker.svelte";
  import ReleaseCover from "../lib/ReleaseCover.svelte";
  import ReleaseDownloads from "../lib/ReleaseDownloads.svelte";
  import StatusPill from "../lib/StatusPill.svelte";
  import TrackerLinks from "../lib/TrackerLinks.svelte";

  let { id }: { id: string } = $props();
  const initialId = untrack(() => id);
  const queryClient = useQueryClient();
  let selection = $state<DownloadSelection | null>(null);
  let selectedTracker = $state("");
  let selectedOrdinals = $state<Set<number>>(new Set());
  let selectionVersion = $state(-1);
  type PackView = "actionable" | "waiting" | "cleanup" | "review" | "resolved";
  let activeView = $state<PackView>("actionable");

  const pack = createQuery({
    queryKey: ["channel-pack", initialId],
    queryFn: () => api<ChannelPack>(`/api/v1/channel-packs/${initialId}`),
    refetchInterval: 15_000
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
    },
    onError: (error) => {
      if (error instanceof ApiError && error.code === "pack_state_changed") {
        selectedOrdinals = new Set();
        queryClient.invalidateQueries({ queryKey: ["channel-pack", initialId] });
      }
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

  function itemView(item: ChannelPack["items"][number]): PackView {
    return item.disposition;
  }

  function selectable(item: ChannelPack["items"][number]): boolean {
    return item.selectable;
  }

  function visibleItems(current: ChannelPack): ChannelPack["items"] {
    return current.items.filter((item) => itemView(item) === activeView);
  }

  function viewCount(current: ChannelPack, view: PackView): number {
    return current.items.filter((item) => itemView(item) === view).length;
  }

  function selectAll(current: ChannelPack) {
    selectedOrdinals = executableOrdinals(current.items);
  }

  function downloadStates(downloads: ChannelPack["items"][number]["downloads"]): ClientDownloadState[] {
    return [...new Set(downloads.map((download) => download.live.state))];
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

  <div class="pack-toolbar">
    <div class="segmented-tabs" role="tablist" aria-label="Pack status">
      {#each [
        ["actionable", "Actionable"],
        ["waiting", "Waiting"],
        ["cleanup", "Cleanup"],
        ["review", "Needs review"],
        ["resolved", "Resolved"]
      ] as tab}
        <button
          role="tab"
          aria-selected={activeView === tab[0]}
          class:active={activeView === tab[0]}
          onclick={() => activeView = tab[0] as PackView}
        >{tab[1]} <span>{viewCount(current, tab[0] as PackView)}</span></button>
      {/each}
    </div>
    {#if current.decision === "open"}
      <div class="selection-actions">
        <button class="text-button" onclick={() => selectAll(current)}>Select all actions</button>
        <button class="text-button" onclick={() => selectedOrdinals = new Set()}>Select none</button>
      </div>
    {/if}
  </div>

  <div class="result-list pack-items">
    {#each visibleItems(current) as item}
      <article class="release-card pack-item">
        <div class="pack-rank">
          {#if current.decision === "open" && selectable(item)}
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
        <ReleaseCover image={item.source.artwork ?? item.release?.artwork} />
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
              <span>{item.source.year ?? ""}</span>
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
          {#if item.downloads.length}
            <details class="pack-source-downloads">
              <summary>
                {item.downloads.length} trumped {item.downloads.length === 1 ? "download" : "downloads"}
                {#each downloadStates(item.downloads) as state}<StatusPill {state} />{/each}
              </summary>
              <ReleaseDownloads downloads={item.downloads} />
            </details>
          {/if}
          {#if item.replacement}
            <section class="replacement-target" aria-label="Preferred replacement">
              <div>
                <p class="eyebrow">Preferred current replacement</p>
                <strong>{[item.replacement.format, item.replacement.encoding, item.replacement.media].filter(Boolean).join(" · ")}</strong>
                <span>{item.replacement.tracker.toUpperCase()} torrent #{item.replacement.torrentId} · {formatBytes(item.replacement.size)}</span>
              </div>
              <span class={`status-pill ${item.replacement.state === "complete" ? "complete" : item.replacement.state === "downloading" ? "downloading" : "queued"}`}>
                {item.replacement.state}
              </span>
            </section>
            {#if item.replacement.downloads.length}
              <ReleaseDownloads downloads={item.replacement.downloads} />
            {/if}
          {/if}
          {#if item.release}
            <PreferredVariants
              variants={item.variants}
              releaseId={item.release.id}
              tracker={item.release.tracker}
              groupId={item.release.groupId}
              title={item.release.title}
              source="channels"
              requestedTorrentId={item.replacement?.torrentId ?? item.plan?.torrentId}
              fulfillment={item.fulfillment}
              onadd={item.replacement ? undefined : (variant) => choose(item, variant)}
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
            {#if current.decision === "open" && item.release}
              <a href={`${searchPath(item.source.artist, item.source.title)}&pack=${current.id}&item=${item.ordinal}&version=${current.planVersion}`}>Change match</a>
            {/if}
          </div>
        </div>
      </article>
    {/each}
    {#if visibleItems(current).length === 0}
      <div class="pack-empty-view">
        <strong>Nothing in this view</strong>
        <span>Choose another status tab to review the rest of the pack.</span>
      </div>
    {/if}
  </div>

  <AddDownloadDialog
    selection={selection}
    tracker={selectedTracker}
    onclose={() => selection = null}
    oncomplete={() => $replan.mutate()}
  />
{/if}
