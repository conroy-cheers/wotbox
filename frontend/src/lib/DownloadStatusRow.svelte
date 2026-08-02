<script lang="ts">
  import type { LiveDownloadStatus } from "./api";
  import { formatBytes, formatSpeed } from "./api";
  import StatusPill from "./StatusPill.svelte";

  let {
    name,
    download,
    eyebrow,
    note,
    compact = false
  }: {
    name: string;
    download: LiveDownloadStatus;
    eyebrow?: string;
    note?: string;
    compact?: boolean;
  } = $props();
</script>

<div class="download-status-row" class:compact>
  <div class="download-status-copy">
    {#if eyebrow}<small>{eyebrow}</small>{/if}
    <strong>{name}</strong>
    <span>
      {Math.round(download.progress * 100)}% · {formatBytes(download.size)}
      {#if download.downloadSpeed > 0} · {formatSpeed(download.downloadSpeed)}{/if}
      {#if note} · {note}{/if}
    </span>
  </div>
  <StatusPill state={download.state} />
</div>
