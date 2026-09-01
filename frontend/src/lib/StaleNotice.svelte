<script lang="ts">
  import { createQuery } from "@tanstack/svelte-query";
  import { Clock3 } from "@lucide/svelte";
  import { api, appPath, type Provenance, type ProviderStatus, type SourceProvenance } from "./api";
  import { freshnessMessage } from "./freshness";
  import { providerNeedsAttention } from "./providerStatus";
  let {
    provenance,
    onrefresh
  }: { provenance?: Provenance; onrefresh?: () => void | Promise<void> } = $props();

  const providers = createQuery({
    queryKey: ["providers"],
    queryFn: () => api<ProviderStatus[]>("/api/v1/providers")
  });
  const affected = $derived((provenance?.sources ?? []).filter((source) => source.state !== "fresh"));

  function providerFor(source: SourceProvenance): ProviderStatus | undefined {
    return ($providers.data ?? []).find((provider) => provider.id === source.providerId);
  }

</script>

{#if affected.length}
  <div class="stale-notice" role="status">
    <Clock3 size={16} />
    <div>
      {#each affected as source}
        <span>{freshnessMessage(source, providerFor(source))}</span>
      {/each}
    </div>
    {#if onrefresh && affected.some((source) => source.refreshState === "failed")}
      <button class="inline-link" onclick={() => onrefresh()}>Retry</button>
    {:else if affected.some((source) => providerNeedsAttention(providerFor(source)))}
      <a href={appPath("/preferences#api-safety")}>Review</a>
    {/if}
  </div>
{/if}
