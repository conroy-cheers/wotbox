<script lang="ts">
  import { onMount } from "svelte";

  let {
    src,
    alt = "",
    loading = "lazy",
    referrerpolicy = "no-referrer"
  }: {
    src: string;
    alt?: string;
    loading?: "eager" | "lazy";
    referrerpolicy?: ReferrerPolicy;
  } = $props();

  let retry = $state(0);
  let failed = $state(false);

  $effect(() => {
    src;
    retry = 0;
    failed = false;
  });

  onMount(() => {
    const changed = (event: Event) => {
      const hash = src.match(/\/assets\/([a-f0-9]{64})\//)?.[1];
      if (!hash) return;
      const reasons = (event as CustomEvent<string[]>).detail ?? [];
      if (reasons.length === 0 || reasons.includes(`asset:${hash}`)) {
        failed = false;
        retry += 1;
      }
    };
    window.addEventListener("wotbox-assets-changed", changed);
    return () => window.removeEventListener("wotbox-assets-changed", changed);
  });
</script>

{#if !failed}
  <img
    src={`${src}${src.includes("?") ? "&" : "?"}asset=${retry}`}
    {alt}
    {loading}
    {referrerpolicy}
    onerror={() => (failed = true)}
  />
{/if}
