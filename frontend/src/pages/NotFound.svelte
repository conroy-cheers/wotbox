<script lang="ts">
  import { ArrowLeft, SearchX } from "@lucide/svelte";
  import { appPath } from "../lib/api";

  let { path }: { path: string } = $props();
</script>

<svelte:head><title>Page not found · Wotbox</title></svelte:head>

<section class="search-welcome not-found" aria-labelledby="not-found-title">
  <SearchX size={36} />
  <p class="eyebrow">404 · Route not found</p>
  <h1 id="not-found-title">This view does not exist</h1>
  <p>Wotbox has no view at <code>{path}</code>.</p>
  <a class="primary-button" href={appPath("/")}><ArrowLeft size={16} /> Back to Dashboard</a>
</section>
