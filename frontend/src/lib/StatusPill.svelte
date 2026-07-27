<script lang="ts">
  import type { ClientDownloadState } from "./api";

  let { state }: { state: ClientDownloadState } = $props();

  const labels: Record<ClientDownloadState, string> = {
    downloading: "Downloading",
    seeding: "Seeding",
    paused: "Paused",
    queued: "Queued",
    checking: "Checking",
    stalled: "Stalled",
    complete: "Complete",
    error: "Error",
    unknown: "Unknown"
  };
</script>

<span
  class:good={state === "complete" || state === "seeding"}
  class:bad={state === "error"}
  class:active={state === "downloading"}
  class="status-pill"
>
  <span class="status-dot"></span>
  {labels[state]}
</span>
