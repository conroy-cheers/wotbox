<script lang="ts">
  import { AlertTriangle, ArrowRight } from "@lucide/svelte";
  import type { DownloadDiagnostic as Diagnostic } from "./api";

  let {
    diagnostic,
    compact = false,
    href
  }: {
    diagnostic: Diagnostic;
    compact?: boolean;
    href?: string;
  } = $props();
</script>

{#snippet content()}
  <AlertTriangle size={compact ? 16 : 20} />
  <div class="download-diagnostic-copy">
    <strong>{diagnostic.summary}</strong>
    <p>{diagnostic.message}</p>
    {#if !compact}
      <div class="download-diagnostic-action">
        <span>Suggested fix</span>
        <p>{diagnostic.action}</p>
      </div>
    {/if}
  </div>
  {#if compact && href}
    <span class="download-diagnostic-link">Details <ArrowRight size={13} /></span>
  {/if}
{/snippet}

{#if href}
  <a class="download-diagnostic" class:compact href={href}>
    {@render content()}
  </a>
{:else}
  <div class="download-diagnostic" class:compact role="status">
    {@render content()}
  </div>
{/if}
