<script lang="ts">
  import type { ReleaseDownload } from "./api";
  import DownloadStatusRow from "./DownloadStatusRow.svelte";

  let { downloads }: { downloads: ReleaseDownload[] } = $props();
</script>

{#if downloads.length}
  <div class="release-downloads" aria-label="Existing downloads">
    {#each downloads as download}
      <DownloadStatusRow
        name={download.name}
        download={download.live}
        eyebrow={download.tracker?.toUpperCase() ?? "Unlinked torrent"}
        note={download.inLibrary ? "In library" : undefined}
        compact
      />
    {/each}
  </div>
{/if}
