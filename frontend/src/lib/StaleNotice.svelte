<script lang="ts">
  import { CloudOff } from "@lucide/svelte";
  import type { Provenance } from "./api";
  let { provenance }: { provenance?: Provenance } = $props();
</script>

{#if provenance?.stale}
  <div class="stale-notice" role="status">
    <CloudOff size={16} />
    The tracker is unavailable. Showing a snapshot from
    {new Date(provenance.fetchedAt).toLocaleString()}.
  </div>
{/if}
