<script lang="ts">
  import { Disc3 } from "@lucide/svelte";

  let {
    image,
    href,
    label,
    iconSize = 36
  }: {
    image?: string | null;
    href?: string;
    label?: string;
    iconSize?: number;
  } = $props();

  function hideBrokenImage(event: Event) {
    (event.currentTarget as HTMLImageElement).remove();
  }
</script>

{#snippet artwork()}
  <Disc3 size={iconSize} aria-hidden="true" />
  {#if image}
    <img
      src={image}
      alt=""
      referrerpolicy="no-referrer"
      loading="lazy"
      onerror={hideBrokenImage}
    />
  {/if}
{/snippet}

{#if href}
  <a class="cover" {href} aria-label={label ? `Open ${label}` : "Open release"}>
    {@render artwork()}
  </a>
{:else}
  <div class="cover" aria-hidden="true">
    {@render artwork()}
  </div>
{/if}
