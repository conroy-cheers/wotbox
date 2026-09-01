<script lang="ts">
  import { onMount } from "svelte";
  import { useQueryClient } from "@tanstack/svelte-query";
  import { basePath } from "./api";
  import { queryUsesResources, queryUsesScopes } from "./live";
  import { applyResourceChanges, type ResourceChange } from "./liveState";

  const queryClient = useQueryClient();
  let outage = $state(false);
  let reconnect: (() => void) | undefined;

  onMount(() => {
    let source: EventSource | undefined;
    let flushTimer: number | undefined;
    let outageTimer: number | undefined;
    let watchdog: number | undefined;
    let stopped = false;
    const pending = new Set<string>();
    const pendingResources = new Set<string>();

    const resetHealth = () => {
      window.clearTimeout(outageTimer);
      outageTimer = undefined;
      window.clearTimeout(watchdog);
      outage = false;
      watchdog = window.setTimeout(() => {
        source?.close();
        connect();
      }, 5 * 60_000);
    };

    const scheduleFlush = () => {
      if (flushTimer) return;
      flushTimer = window.setTimeout(async () => {
        flushTimer = undefined;
        const scopes = new Set(pending);
        const resources = new Set(pendingResources);
        pending.clear();
        pendingResources.clear();
        await queryClient.invalidateQueries({
          predicate: (query) => queryUsesResources(query, resources)
            || queryUsesScopes(query, scopes),
          refetchType: "active"
        });
      }, 150);
    };

    const receive = (event: MessageEvent) => {
      resetHealth();
      if (event.lastEventId) sessionStorage.setItem("wotbox-change-cursor", event.lastEventId);
      try {
        const payload = JSON.parse(event.data) as {
          scopes?: string[];
          resources?: string[];
          reasons?: string[];
          changes?: ResourceChange[];
        };
        applyResourceChanges(payload.changes ?? []);
        const scopes = payload.scopes ?? ["global"];
        if (scopes.includes("assets")) {
          window.dispatchEvent(new CustomEvent("wotbox-assets-changed", {
            detail: payload.reasons ?? []
          }));
        }
        for (const resource of payload.resources ?? []) pendingResources.add(resource);
        if ((payload.resources?.length ?? 0) === 0) {
          for (const scope of scopes) {
            if (scope !== "assets") pending.add(scope);
          }
        }
      } catch {
        pending.add("global");
      }
      if (pending.size > 0 || pendingResources.size > 0) scheduleFlush();
    };

    const connect = () => {
      if (stopped) return;
      source?.close();
      const cursor = sessionStorage.getItem("wotbox-change-cursor");
      source = new EventSource(`${basePath}/api/v1/events${cursor ? `?after=${encodeURIComponent(cursor)}` : ""}`);
      source.onopen = resetHealth;
      source.addEventListener("changes", receive);
      source.addEventListener("reset", receive);
      source.addEventListener("ping", resetHealth);
      source.onerror = () => {
        if (!outageTimer) {
          outageTimer = window.setTimeout(() => {
            outageTimer = undefined;
            outage = true;
          }, 30_000);
        }
      };
    };

    reconnect = connect;
    connect();
    const visibility = () => {
      if (document.visibilityState === "visible") {
        source?.close();
        connect();
      }
    };
    document.addEventListener("visibilitychange", visibility);
    return () => {
      stopped = true;
      source?.close();
      window.clearTimeout(flushTimer);
      window.clearTimeout(outageTimer);
      window.clearTimeout(watchdog);
      document.removeEventListener("visibilitychange", visibility);
    };
  });
</script>

{#if outage}
  <div class="live-update-notice" role="status">
    Live updates are disconnected. Displayed data may be stale.
    <button onclick={() => reconnect?.()}>Retry</button>
    <button onclick={() => location.reload()}>Reload</button>
  </div>
{/if}
