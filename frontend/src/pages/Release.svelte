<script lang="ts">
  import { createQuery } from "@tanstack/svelte-query";
  import { ArrowLeft, Disc3 } from "@lucide/svelte";
  import { untrack } from "svelte";
  import { api, type Envelope } from "../lib/api";
  import StaleNotice from "../lib/StaleNotice.svelte";

  let { tracker, id }: { tracker: string; id: string } = $props();
  const initialTracker = untrack(() => tracker);
  const initialId = untrack(() => id);
  const release = createQuery({
    queryKey: ["release", initialTracker, initialId],
    queryFn: () => api<Envelope<Record<string, unknown>>>(`/api/v1/groups/${initialTracker}/${initialId}`)
  });

  const group = $derived(($release.data?.data.group ?? $release.data?.data ?? {}) as Record<string, unknown>);
</script>

<svelte:head><title>Release · Wotbox</title></svelte:head>

<a class="back-link" href="search"><ArrowLeft size={16} /> Back to search</a>
<StaleNotice provenance={$release.data?.provenance} />

{#if $release.isPending}
  <div class="release-hero skeleton-card"></div>
{:else if $release.isError}
  <div class="error-panel">{$release.error.message}</div>
{:else}
  <section class="release-hero">
    <div class="cover hero-cover">
      <Disc3 size={48} />
      {#if typeof group.wikiImage === "string"}
        <img
          src={group.wikiImage}
          alt=""
          referrerpolicy="no-referrer"
          onerror={(event) => (event.currentTarget as HTMLImageElement).remove()}
        />
      {/if}
    </div>
    <div>
      <p class="eyebrow">Tracker release</p>
      <h1>{String(group.name ?? `Release ${id}`)}</h1>
      <p class="release-byline">{String(group.year ?? "")} {group.recordLabel ? `· ${group.recordLabel}` : ""}</p>
      <div class="tag-list">
        {#each (Array.isArray(group.tags) ? group.tags : []).slice(0, 8) as tag}
          <span>{typeof tag === "string" ? tag : String(tag.name ?? "")}</span>
        {/each}
      </div>
    </div>
  </section>
  {#if group.wikiBody}
    <section class="section prose-panel">
      <p class="eyebrow">About this release</p>
      <p>{String(group.wikiBody)}</p>
    </section>
  {/if}
  <details class="source-panel">
    <summary>View tracker source metadata</summary>
    <pre>{JSON.stringify($release.data?.data, null, 2)}</pre>
  </details>
{/if}
