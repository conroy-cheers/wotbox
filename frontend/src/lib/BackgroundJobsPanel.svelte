<script lang="ts">
  import { createQuery, useQueryClient } from "@tanstack/svelte-query";
  import { CirclePause, RefreshCw, RotateCcw } from "@lucide/svelte";
  import {
    api,
    type BackgroundJobState,
    type BackgroundJobsOverview,
    type BackgroundJobStatus
  } from "./api";

  const queryClient = useQueryClient();
  const jobs = createQuery({
    queryKey: ["background-jobs"],
    queryFn: () => api<BackgroundJobsOverview>("/api/v1/background-jobs?limit=50"),
    refetchInterval: 3_000
  });
  let actionJob = $state("");
  let actionError = $state("");

  const labels: Record<string, string> = {
    resolve_download_hash: "Match library download",
    index_tracklist: "Index release tracklist",
    compute_single_coverage: "Match Single to Album",
    scan_download_client: "Scan download client",
    canonical_backfill: "Update library identities",
    enrich_library_artists: "Enrich library artists",
    notify_plex: "Notify Plex"
  };
  const stateOrder: BackgroundJobState[] = [
    "running",
    "pending",
    "retrying",
    "failed",
    "completed",
    "cancelled"
  ];
  const visible = $derived($jobs.data?.jobs ?? []);

  function displayName(job: BackgroundJobStatus): string {
    return labels[job.kind] ?? job.kind.replaceAll("_", " ");
  }

  function context(job: BackgroundJobStatus): string {
    const parts = job.deduplicationKey.split(":");
    if (job.kind === "index_tracklist") {
      return `${parts[1]?.toUpperCase()} group #${parts[2]}`;
    }
    if (job.kind === "compute_single_coverage") {
      return `${parts[1]?.toUpperCase()} Single group #${parts[2]}`;
    }
    if (job.kind === "resolve_download_hash") {
      return `${parts[1]?.toUpperCase()} torrent ${parts[2]?.slice(0, 8)}…`;
    }
    if (job.kind === "scan_download_client") {
      return parts.slice(1).join(":");
    }
    if (job.kind === "notify_plex") {
      return "Partial music library scan";
    }
    return "";
  }

  function when(job: BackgroundJobStatus): string {
    if (job.state === "retrying" && job.nextRunAt) {
      return `Retry ${new Date(job.nextRunAt).toLocaleString()}`;
    }
    if (job.state === "running" && job.leaseUntil) {
      return `Lease healthy until ${new Date(job.leaseUntil).toLocaleTimeString()}`;
    }
    return `Updated ${new Date(job.updatedAt).toLocaleString()}`;
  }

  async function control(job: BackgroundJobStatus, action: "cancel" | "retry") {
    actionJob = job.id;
    actionError = "";
    try {
      await api<unknown>(`/api/v1/background-jobs/${job.id}/${action}`, { method: "POST" });
      await queryClient.invalidateQueries({ queryKey: ["background-jobs"] });
    } catch (cause) {
      actionError = cause instanceof Error ? cause.message : "Unable to update background job";
    } finally {
      actionJob = "";
    }
  }
</script>

<section class="preferences-panel background-work-panel" id="background-work">
  <div class="section-heading">
    <div><p class="eyebrow">Durable task queue</p><h2>Background work</h2></div>
    <button
      class="secondary-button compact-button"
      disabled={$jobs.isFetching}
      onclick={() => $jobs.refetch()}
    ><RefreshCw size={14} class={$jobs.isFetching ? "spin" : ""} /> Refresh</button>
  </div>
  <p class="settings-help">
    Library matching, tracklist indexing, and client scans survive restarts. Work waiting on a
    provider is retried automatically without bypassing its API safety limits.
  </p>
  {#if $jobs.isPending}
    <div class="skeleton-card"></div>
  {:else if $jobs.isError}
    <div class="error-panel compact">{$jobs.error.message}</div>
  {:else}
    <div class="job-counts" aria-label="Background job counts">
      {#each stateOrder as state}
        <div class:attention={state === "failed" && ($jobs.data?.counts[state] ?? 0) > 0}>
          <strong>{$jobs.data?.counts[state] ?? 0}</strong>
          <span>{state}</span>
        </div>
      {/each}
    </div>
    <div class="background-job-list">
      {#each visible as job}
        <article class="background-job-row">
          <span class={`job-state-dot ${job.state}`} title={job.state}></span>
          <div class="background-job-copy">
            <header>
              <strong>{displayName(job)}</strong>
              <span class={`job-state ${job.state}`}>{job.state}</span>
            </header>
            <span>{[context(job), job.progressMessage ?? when(job)].filter(Boolean).join(" · ")}</span>
            {#if job.lastErrorMessage && ["failed", "retrying"].includes(job.state)}
              <small>{job.lastErrorMessage}</small>
            {/if}
            {#if job.progressTotal}
              <div class="job-progress" aria-label={`${job.progressCompleted} of ${job.progressTotal}`}>
                <span style={`width: ${Math.min(100, job.progressCompleted / job.progressTotal * 100)}%`}></span>
              </div>
            {/if}
          </div>
          <div class="background-job-actions">
            {#if job.canCancel}
              <button
                class="secondary-button compact-button"
                disabled={actionJob === job.id}
                onclick={() => control(job, "cancel")}
              ><CirclePause size={13} /> Cancel</button>
            {/if}
            {#if job.canRetry}
              <button
                class="secondary-button compact-button"
                disabled={actionJob === job.id}
                onclick={() => control(job, "retry")}
              ><RotateCcw size={13} /> Retry</button>
            {/if}
          </div>
        </article>
      {:else}
        <p class="empty-inline">No background work has been scheduled.</p>
      {/each}
    </div>
  {/if}
  {#if actionError}<div class="error-panel compact">{actionError}</div>{/if}
</section>
