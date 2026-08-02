<script lang="ts">
  import { createQuery } from "@tanstack/svelte-query";
  import { ExternalLink } from "@lucide/svelte";
  import { api, type PublicConfig, type ReleaseSource } from "./api";
  import { trackerGroupUrl, uniqueReleaseSources } from "./trackerLinks";

  let {
    sources = [],
    tracker,
    groupId
  }: {
    sources?: ReleaseSource[];
    tracker?: string;
    groupId?: number;
  } = $props();

  const config = createQuery({
    queryKey: ["config"],
    queryFn: () => api<PublicConfig>("/api/v1/config"),
    staleTime: Infinity
  });
  const links = $derived(uniqueReleaseSources(
    sources.length
      ? sources
      : tracker && groupId
        ? [{ tracker, groupId, matchScore: 1 }]
        : []
  ));
</script>

{#if links.some((source) => trackerGroupUrl(source, $config.data?.trackerSites))}
  <span class="tracker-links" aria-label="Original tracker pages">
    {#each links as source}
      {@const href = trackerGroupUrl(source, $config.data?.trackerSites)}
      {#if href}
        <a href={href} target="_blank" rel="noreferrer" title={`Open on ${source.tracker.toUpperCase()}`}>
          {source.tracker.toUpperCase()} <ExternalLink size={11} />
        </a>
      {/if}
    {/each}
  </span>
{/if}
