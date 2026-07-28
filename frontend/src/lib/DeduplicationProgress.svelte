<script lang="ts">
  import type { DeduplicationIndexStatus } from "./api";

  let {
    status,
    detail
  }: {
    status: DeduplicationIndexStatus;
    detail: string;
  } = $props();

  const percentage = $derived(
    status.total > 0 ? Math.round((status.checked / status.total) * 100) : 0
  );
</script>

{#if status.total > 0 && status.checked < status.total}
  <div class="index-banner deduplication-progress compact">
    <span class="index-pulse"></span>
    <div class="deduplication-progress-body">
      <div class="deduplication-progress-heading">
        <p><strong>Checking album overlap</strong> {detail}</p>
        <span>{status.checked.toLocaleString()} / {status.total.toLocaleString()}</span>
      </div>
      <div
        class="deduplication-progress-track"
        role="progressbar"
        aria-label="Deduplication progress"
        aria-valuemin="0"
        aria-valuemax={status.total}
        aria-valuenow={status.checked}
      >
        <span style={`width: ${percentage}%`}></span>
      </div>
      <div class="deduplication-progress-meta">
        <span>{percentage}% checked</span>
        {#if status.resolving}<span>{status.resolving.toLocaleString()} resolving</span>{/if}
        {#if status.pending}<span>{status.pending.toLocaleString()} queued</span>{/if}
        {#if status.failed}<span class="warning">{status.failed.toLocaleString()} retrying</span>{/if}
      </div>
    </div>
  </div>
{/if}
