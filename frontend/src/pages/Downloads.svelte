<script lang="ts">
  import { createMutation, createQuery, useQueryClient } from "@tanstack/svelte-query";
  import { ArrowDownToLine, ArrowRight, Clock3, RefreshCw } from "@lucide/svelte";
  import { derived, writable } from "svelte/store";
  import { api, formatBytes, formatSpeed, relativeTime, type DownloadsPage, type ImportsPage, type ImportTask } from "../lib/api";
  import DownloadStatusRow from "../lib/DownloadStatusRow.svelte";
  import DownloadDiagnostic from "../lib/DownloadDiagnostic.svelte";
  import StatusPill from "../lib/StatusPill.svelte";
  import TrackerLinks from "../lib/TrackerLinks.svelte";
  import { releaseTypeColor } from "../lib/releasePresentation";
  import { positiveInteger, releaseViewPath, replaceView } from "../lib/routing";

  const initial = new URLSearchParams(location.search);
  const queryClient = useQueryClient();
  let view = $state<"imports" | "transfers">(initial.get("view") === "transfers" ? "transfers" : "imports");
  let importFilter = $state<"active" | "review" | "history">("active");
  const limit = writable(Math.min(positiveInteger(initial, "limit", 100), 500));
  let urlSyncReady = false;
  const queryOptions = derived(limit, ($limit) => ({
    queryKey: ["downloads", $limit] as const,
    queryFn: () => api<DownloadsPage>(`/api/v1/downloads?limit=${$limit}`),
    refetchInterval: 15_000
  }));
  const downloads = createQuery(queryOptions);
  const imports = createQuery({
    queryKey: ["imports"],
    queryFn: () => api<ImportsPage>("/api/v1/imports?limit=500"),
    refetchInterval: 10_000
  });
  const importAction = createMutation({
    mutationFn: ({ id, action }: { id: string; action: "retry" | "dismiss" }) =>
      api<void>(`/api/v1/imports/${id}/${action}`, { method: "POST" }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["imports"] })
  });

  $effect(() => {
    const value = $limit;
    if (urlSyncReady) {
      replaceView("/downloads", {
        view: view === "imports" ? undefined : view,
        limit: value === 100 ? undefined : value
      });
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
      item.variant?.torrentId,
      "downloads",
      { client: item.download.client, infoHash: item.download.infoHash },
      false,
      showClientDetails
    );
  }

  function importBucket(item: ImportTask): "active" | "review" | "history" {
    if (["needs_review", "blocked", "failed"].includes(item.state)) return "review";
    if (["complete", "dismissed"].includes(item.state)) return "history";
    return "active";
  }

  function visibleImports(): ImportTask[] {
    return ($imports.data?.items ?? []).filter((item) => importBucket(item) === importFilter);
  }
</script>

<svelte:head><title>Downloads · Wotbox</title></svelte:head>

<header class="page-heading">
  <div>
    <p class="eyebrow">Tracker releases</p>
    <h1>Downloads</h1>
    <p>Follow downloads from transfer through release matching, library import, and guarded replacement cleanup.</p>
  </div>
  <button class="icon-button" aria-label="Refresh downloads" onclick={() => $downloads.refetch()}>
    <span class:spin={$downloads.isFetching}><RefreshCw size={18} /></span>
  </button>
</header>

<div class="page-tabs" role="tablist" aria-label="Downloads view">
  <button role="tab" class:active={view === "imports"} aria-selected={view === "imports"} onclick={() => view = "imports"}>Import queue</button>
  <button role="tab" class:active={view === "transfers"} aria-selected={view === "transfers"} onclick={() => view = "transfers"}>All transfers</button>
</div>

{#if view === "imports"}
  {#if $imports.isError}
    <div class="error-panel">{$imports.error.message}</div>
  {:else if $imports.isPending}
    <div class="download-grid">{#each [1, 2, 3] as _}<div class="download-card skeleton-card"></div>{/each}</div>
  {:else if $imports.data}
    <div class="import-overview">
      <div><strong>{$imports.data.counts.active}</strong><span>Active</span></div>
      <div><strong>{$imports.data.counts.review}</strong><span>Needs review</span></div>
      <div><strong>{$imports.data.counts.complete}</strong><span>History</span></div>
    </div>
    <div class="segmented-tabs import-filter-tabs" role="tablist" aria-label="Import status">
      {#each [["active", "Active"], ["review", "Needs review"], ["history", "History"]] as tab}
        <button role="tab" class:active={importFilter === tab[0]} aria-selected={importFilter === tab[0]} onclick={() => importFilter = tab[0] as typeof importFilter}>{tab[1]}</button>
      {/each}
    </div>
    {#if visibleImports().length}
      <div class="import-task-list">
        {#each visibleImports() as item}
          <article class="import-task-card">
            <header>
              <div>
                <p class="eyebrow">{item.tracker?.toUpperCase() ?? "Unlinked"}{item.torrentId ? ` · torrent #${item.torrentId}` : ""}</p>
                <h2>{item.release?.title ?? item.displayName}</h2>
                <span>{item.release?.artist ?? item.client ?? "Download import"}</span>
                {#if item.release}<TrackerLinks sources={item.release.sources} tracker={item.release.tracker} groupId={item.release.groupId} />{/if}
              </div>
              <span class={`status-pill ${item.state}`}>{item.state.replaceAll("_", " ")}</span>
            </header>
            {#if item.download}
              <DownloadStatusRow name={item.displayName} download={item.download} eyebrow="Current torrent" />
            {:else if item.state === "downloading"}
              <div class="import-pending-line">Waiting for the download client to report the replacement torrent.</div>
            {/if}
            {#if item.supersessions.length}
              <details class="import-supersessions" open={item.state === "blocked" || item.state === "needs_review"}>
                <summary>{item.supersessions.length} superseded {item.supersessions.length === 1 ? "torrent" : "torrents"}</summary>
                {#each item.supersessions as source}
                  {#if source.download}
                    <DownloadStatusRow
                      name={source.sourceName}
                      download={source.download}
                      eyebrow={`${source.tracker.toUpperCase()} · ${source.cleanupState.replaceAll("_", " ")}`}
                      note={source.reason}
                      compact
                    />
                  {:else}
                    <div class="import-source-line"><strong>{source.sourceName}</strong><span>{source.cleanupState.replaceAll("_", " ")}</span></div>
                  {/if}
                {/each}
              </details>
            {/if}
            {#if item.reason || item.error}<p class="import-task-reason">{item.error ?? item.reason}</p>{/if}
            <footer>
              <span>{item.baseline ? "Existing library baseline" : `Updated ${relativeTime(item.updatedAt)}`}</span>
              {#if ["blocked", "failed", "needs_review"].includes(item.state)}
                <div>
                  <button class="secondary-button compact-button" disabled={$importAction.isPending} onclick={() => $importAction.mutate({ id: item.id, action: "retry" })}>Retry checks</button>
                  <button class="text-button" disabled={$importAction.isPending} onclick={() => $importAction.mutate({ id: item.id, action: "dismiss" })}>Dismiss</button>
                </div>
              {/if}
            </footer>
          </article>
        {/each}
      </div>
    {:else}
      <div class="search-welcome"><ArrowDownToLine size={32} /><h2>No {importFilter.replaceAll("_", " ")} imports</h2><p>Tasks move here automatically as downloads and replacement cleanup progress.</p></div>
    {/if}
  {/if}
{:else}

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
            <p>{item.release.artist ?? "Various artists"} · {[item.release.year, item.release.releaseType, item.variant?.format, item.variant?.encoding].filter(Boolean).join(" · ")} · {formatBytes(download.size)}</p>
            <TrackerLinks sources={item.release.sources} tracker={item.release.tracker} groupId={item.release.groupId} />
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
{/if}
