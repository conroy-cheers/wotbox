<script lang="ts">
  import type { DeduplicationIndexStatus } from "./api";

  let {
    status,
    detail
  }: {
    status: DeduplicationIndexStatus;
    detail: string;
  } = $props();

  // The overlap totals are scoped to the current page, while tracklist totals
  // cover the whole durable queue. Mixing them made a nearly complete library
  // look stuck behind unrelated catalogue indexing.
  const progressValue = $derived(status.checked);
  const progressTotal = $derived(status.total);
  const percentage = $derived(
    progressTotal > 0 ? Math.round((progressValue / progressTotal) * 100) : 0
  );
</script>

{#if status.total > 0 && status.checked < status.total}
  <div class="index-banner deduplication-progress compact">
    <span class="index-pulse"></span>
    <div class="deduplication-progress-body">
      <div class="deduplication-progress-heading">
        <p><strong>Checking album overlap</strong> {detail}</p>
        <span>
          {status.checked.toLocaleString()} / {status.total.toLocaleString()}
          {status.total === 1 ? "Single" : "Singles"}
        </span>
      </div>
      <div
        class="deduplication-progress-track"
        role="progressbar"
        aria-label="Album overlap checking progress"
        aria-valuemin="0"
        aria-valuemax={progressTotal}
        aria-valuenow={progressValue}
      >
        <span style={`width: ${percentage}%`}></span>
      </div>
      <div class="deduplication-progress-meta">
        <span>
          {status.tracklistsIndexed.toLocaleString()} /
          {status.tracklistsTotal.toLocaleString()} tracklists indexed
        </span>
        {#if status.tracklistsResolving}
          <span>{status.tracklistsResolving.toLocaleString()} active</span>
        {/if}
        {#if status.tracklistsPending}
          <span>{status.tracklistsPending.toLocaleString()} queued</span>
        {/if}
        {#if status.tracklistsFailed}
          <span class="warning">{status.tracklistsFailed.toLocaleString()} retrying</span>
        {/if}
      </div>
      <div class="deduplication-discovery-note">
        The queue may grow while more artist catalogs are discovered.
      </div>
    </div>
  </div>
{/if}
