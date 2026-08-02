<script lang="ts">
  import { Check } from "@lucide/svelte";
  import { appPath, type ReleaseSummary } from "./api";
  import TrackerLinks from "./TrackerLinks.svelte";

  let {
    candidates,
    pending = false,
    onselect
  }: {
    candidates: ReleaseSummary[];
    pending?: boolean;
    onselect: (release: ReleaseSummary) => void;
  } = $props();
</script>

{#if candidates.length}
  <div class="release-candidate-picker" aria-label="Possible tracker matches">
    <p>Choose the matching tracker release</p>
    {#each candidates as candidate}
      <div class="release-candidate-row">
        <div>
          <strong>
            {#if candidate.id}
              <a href={appPath(`/releases/${candidate.id}?from=channels`)}>{candidate.title}</a>
            {:else}
              {candidate.title}
            {/if}
          </strong>
          <small>
            {candidate.artist ?? "Various artists"} ·
            {[candidate.year, candidate.releaseType].filter(Boolean).join(" · ") || "Release details unavailable"}
          </small>
          <TrackerLinks sources={candidate.sources} tracker={candidate.tracker} groupId={candidate.groupId} />
        </div>
        <button
          class="secondary-button compact-button"
          disabled={pending || !candidate.id}
          onclick={() => onselect(candidate)}
        ><Check size={14} /> {pending ? "Choosing…" : "Choose"}</button>
      </div>
    {/each}
  </div>
{/if}
