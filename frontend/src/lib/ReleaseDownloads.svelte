<script lang="ts">
  import type { ReleaseDownload } from "./api";
  import StatusPill from "./StatusPill.svelte";

  let { downloads }: { downloads: ReleaseDownload[] } = $props();
</script>

{#if downloads.length}
  <div class="release-downloads" aria-label="Existing downloads">
    {#each downloads as download}
      <div class="release-download-row">
        <div>
          <strong>{download.name}</strong>
          <small>
            {download.tracker?.toUpperCase() ?? "Unlinked torrent"}
            {#if download.inLibrary} · In library{/if}
          </small>
        </div>
        <StatusPill state={download.live.state} />
      </div>
    {/each}
  </div>
{/if}
