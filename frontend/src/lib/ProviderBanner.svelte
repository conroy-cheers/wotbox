<script lang="ts">
  import { createQuery } from "@tanstack/svelte-query";
  import { AlertTriangle } from "@lucide/svelte";
  import { api, appPath, type ProviderStatus } from "./api";
  import { providerStatusSummary } from "./providerStatus";

  const providers = createQuery({
    queryKey: ["providers"],
    queryFn: () => api<ProviderStatus[]>("/api/v1/providers"),
    refetchInterval: 10_000
  });
  const affected = $derived(
    ($providers.data ?? []).filter((provider) =>
      ["cooldown", "blocked", "paused"].includes(provider.state)
    )
  );
</script>

{#if affected.length}
  <div class="provider-banner" role="status">
    <AlertTriangle size={17} />
    <div>
      <strong>External services are limited</strong>
      <span>
        {affected.map(providerStatusSummary).join("; ")}.
        Cached library data remains available.
      </span>
    </div>
    <a href={appPath("/preferences#api-safety")}>Review</a>
  </div>
{/if}
