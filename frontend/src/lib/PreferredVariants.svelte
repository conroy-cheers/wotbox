<script lang="ts">
  import { createQuery } from "@tanstack/svelte-query";
  import { ArrowDownToLine, ChevronDown, ChevronUp, Users } from "@lucide/svelte";
  import {
    api,
    formatBytes,
    type ReleaseFulfillment,
    type RuntimePreferences
  } from "./api";
  import {
    defaultReleasePreferences,
    isMediaAllowed,
    isQualityAllowed,
    rankVariants,
    selectFeaturedVariant,
    type DisplayVariant
  } from "./releasePreferences";
  import { releaseViewPath, type ReleaseSource } from "./routing";
  import { liveDownloads, variantDownloads } from "./liveState";
  import StatusPill from "./StatusPill.svelte";

  let {
    variants,
    releaseId,
    tracker,
    groupId,
    title,
    requestedTorrentId,
    fulfillment,
    source = "search",
    expanded: controlledExpanded,
    onexpandedchange,
    onadd
  }: {
    variants: DisplayVariant[];
    releaseId?: string;
    tracker: string;
    groupId: number;
    title: string;
    requestedTorrentId?: number;
    fulfillment?: ReleaseFulfillment;
    source?: ReleaseSource;
    expanded?: boolean;
    onexpandedchange?: (expanded: boolean) => void;
    onadd?: (variant: DisplayVariant) => void;
  } = $props();

  let localExpanded = $state(false);
  const expanded = $derived(controlledExpanded ?? localExpanded);
  const preferences = createQuery({
    queryKey: ["preferences"],
    queryFn: () => api<RuntimePreferences>("/api/v1/preferences"),
    staleTime: 30_000
  });
  const policy = $derived($preferences.data?.release ?? defaultReleasePreferences);
  const ranked = $derived(rankVariants(variants, policy));
  const fulfillmentTarget = $derived(
    fulfillment?.requirement.target
      ?? fulfillment?.actions.find((action) => action.primary && action.target)?.target
  );
  const focusTorrentId = $derived(requestedTorrentId ?? fulfillmentTarget?.torrentId);
  const preferred = $derived(selectFeaturedVariant(
    ranked,
    focusTorrentId === undefined
      ? undefined
      : { tracker: fulfillmentTarget?.tracker, torrentId: focusTorrentId },
    isDownloadable
  ));
  const remaining = $derived(ranked.filter((variant) => variant.torrentId !== preferred?.torrentId));

  function libraryState(variant: DisplayVariant) {
    return "library" in variant ? variant.library : undefined;
  }

  function tokenCost(variant: DisplayVariant): number | undefined {
    return "eligibility" in variant ? variant.eligibility?.tokenCost : undefined;
  }

  function isDownloadable(variant: DisplayVariant): boolean {
    if (!isQualityAllowed(variant, policy)) return false;
    if (!isMediaAllowed(variant, policy)) return false;
    if ("eligibility" in variant && variant.eligibility) return variant.eligibility.eligible;
    const variantTracker = variant.tracker ?? tracker;
    const trackerPolicy = policyFor(variantTracker);
    if (trackerPolicy.mode === "disabled") return false;
    if (variant.freeleech) return true;
    if (trackerPolicy.mode === "freeleech_only") return false;
    if (trackerPolicy.mode === "freeleech_or_token") return variant.canUseToken;
    return true;
  }

  function fulfillmentAction(variant: DisplayVariant) {
    if (!fulfillment) return undefined;
    const variantTracker = (variant.tracker ?? tracker).toLowerCase();
    return fulfillment.actions.find((action) =>
      action.target
        && action.target.torrentId === variant.torrentId
        && action.target.tracker.toLowerCase() === variantTracker
        && ["add", "add_another", "retry"].includes(action.kind)
    );
  }

  function policyFor(variantTracker: string) {
    return policy.trackerPolicies.find((candidate) =>
      candidate.tracker.toLowerCase() === variantTracker.toLowerCase()
    ) ?? {
      tracker: variantTracker,
      mode: "freeleech_only" as const,
      autoUseTokens: false,
      autoTokenLimit: 0
    };
  }

  function policyBlockExplanation(variant: DisplayVariant): string {
    const variantTracker = variant.tracker ?? tracker;
    const trackerLabel = variantTracker.toUpperCase();
    const reason = "eligibility" in variant ? variant.eligibility?.reason : undefined;
    let explanation: string;
    if (reason === "tracker_disabled" || policyFor(variantTracker).mode === "disabled") {
      explanation = `${trackerLabel} downloads are disabled.`;
    } else if (
      reason === "freeleech_required"
      || policyFor(variantTracker).mode === "freeleech_only"
    ) {
      explanation = `${trackerLabel} is set to “Already free only”, and this torrent is not freeleech.`;
    } else if (reason === "token_unavailable") {
      explanation = `${trackerLabel} is set to “Free or token”, but this torrent cannot use a freeleech token.`;
    } else if (reason === "token_cost_unknown") {
      explanation = `${trackerLabel} token cost cannot be calculated because the torrent size is unavailable.`;
    } else {
      explanation = `This ${trackerLabel} torrent does not satisfy the current download-cost policy.`;
    }
    return `${explanation} Change this under Preferences → Trackers and download cost.`;
  }

  function formatSummary(variant: DisplayVariant): string {
    const format = variant.format?.trim() || "Unknown";
    const encoding = variant.encoding?.trim() || "";
    const media = variant.media?.trim();
    let quality: string;
    if (/24[\s-]*bit/i.test(encoding)) {
      quality = `24-bit ${format}`;
    } else if (/lossless/i.test(encoding)) {
      quality = `Lossless ${format}`;
    } else if (encoding) {
      quality = `${format} ${encoding}`;
    } else {
      quality = format;
    }
    return media ? `${quality} · ${media}` : quality;
  }

  function toggleExpanded() {
    const next = !expanded;
    if (onexpandedchange) onexpandedchange(next);
    else localExpanded = next;
  }

  function variantPath(variant: DisplayVariant): string {
    const download = currentDownloads(variant)[0];
    return releaseViewPath(
      releaseId,
      variant.torrentId,
      source,
      download
        ? { client: download.client, infoHash: download.infoHash }
        : undefined,
      expanded
    );
  }

  function currentDownloads(variant: DisplayVariant) {
    return variantDownloads(variant, $liveDownloads);
  }
</script>

{#snippet toggleButton()}
  <button
    class="variant-toggle"
    onclick={toggleExpanded}
    aria-expanded={expanded}
    aria-label={expanded ? "Hide other formats" : `Show ${remaining.length} other ${remaining.length === 1 ? "format" : "formats"}`}
    title={expanded ? "Hide other formats" : `${remaining.length} other ${remaining.length === 1 ? "format" : "formats"}`}
  >
    {#if expanded}<ChevronUp size={14} />{:else}<ChevronDown size={14} />{/if}
    <span>{remaining.length}</span>
  </button>
{/snippet}

{#snippet variantRow(variant: DisplayVariant, isPreferred = false, showToggle = false)}
  {@const qualityAllowed = isQualityAllowed(variant, policy)}
  {@const mediaAllowed = isMediaAllowed(variant, policy)}
  {@const allowed = isDownloadable(variant)}
  {@const action = fulfillmentAction(variant)}
  {@const downloads = currentDownloads(variant)}
  {@const canAdd = Boolean(onadd) && allowed && (!fulfillment || action?.enabled === true)}
  {@const library = libraryState(variant)}
  {@const policyTooltipId = `policy-${variant.tracker ?? tracker}-${variant.torrentId}`}
  <div class="torrent-row preferred-torrent-row" class:matched={isPreferred}>
    <div class="variant-toggle-slot">
      {#if showToggle && remaining.length}
        {@render toggleButton()}
      {/if}
    </div>
    <div class="torrent-format">
      <strong title={formatSummary(variant)}>{formatSummary(variant)}</strong>
      <small>
        {(variant.tracker ?? tracker).toUpperCase()}
        {#if variant.remasterTitle} · {variant.remasterTitle}{/if}
      </small>
    </div>
    <span class="torrent-size">{formatBytes(variant.size)}</span>
    <span class="peer-count" title="Seeders"><Users size={14} /> {variant.seeders ?? 0}</span>
    <div class="torrent-flags">
      {#if isPreferred && allowed}<span class="preferred-badge">Preferred</span>{/if}
      {#if !qualityAllowed}<span class="cutoff-badge">Quality rejected</span>
      {:else if !mediaAllowed}<span class="cutoff-badge">Media rejected</span>
      {:else if !allowed}
        <span class="policy-tooltip">
          <button
            type="button"
            class="cutoff-badge policy-badge"
            aria-describedby={policyTooltipId}
          >Policy blocked</button>
          <span id={policyTooltipId} role="tooltip" class="policy-tooltip-content">
            {policyBlockExplanation(variant)}
          </span>
        </span>
      {/if}
      {#if variant.freeleech}<span class="free-badge">Free</span>{/if}
      {#if !variant.freeleech}
        {@const cost = tokenCost(variant)}
        {#if cost !== undefined}
          <span class="token-badge">
            {cost} {cost === 1 ? "token" : "tokens"}
          </span>
        {/if}
      {/if}
      {#if library?.availability === "present"}<span class="library-badge">In Library</span>{/if}
      {#if library?.availability === "missing"}<span class="missing-badge">Missing</span>{/if}
    </div>
    <div class="variant-actions">
      {#if downloads.length}
        <a class="download-status-link" href={variantPath(variant)}>
          <StatusPill state={downloads[0].state} />
        </a>
      {:else if library?.availability === "present" || !onadd || (fulfillment && !action)}
        <a class="secondary-button compact-button" href={variantPath(variant)}>View</a>
      {:else}
        <button
          class="download-button catalog-add-button"
          disabled={!canAdd}
          title={canAdd ? `${action?.kind === "add_another" ? "Add another variant of" : action?.kind === "retry" ? "Retry" : "Add"} ${title}` : "This variant is blocked by your release preferences"}
          aria-label={canAdd ? `${action?.kind === "add_another" ? "Add another variant of" : action?.kind === "retry" ? "Retry" : "Add"} ${title}` : `${title} is blocked by release preferences`}
          onclick={() => canAdd && onadd?.(variant)}
        >
          <ArrowDownToLine size={15} />
          <span>{action?.kind === "add_another" ? "Add another" : action?.kind === "retry" ? "Retry" : library?.availability === "missing" ? "Re-add" : "Add"}</span>
        </button>
      {/if}
    </div>
  </div>
{/snippet}

{#if variants.length === 0}
  <div class="variant-empty" role="status">
    <strong>No active torrents</strong>
    <span>{tracker.toUpperCase()} does not currently list a downloadable variant for this release.</span>
  </div>
{:else}
  <div class="preferred-variants" class:expanded>
    {#if expanded && remaining.length}
      <div class="variant-control-anchor">
        <div class="variant-toggle-slot">{@render toggleButton()}</div>
      </div>
      <div class="torrent-list compact-variant-list expanded">
        {#if preferred}
          {@render variantRow(preferred, true)}
        {/if}
        {#each remaining as variant}
          {@render variantRow(variant)}
        {/each}
      </div>
    {:else}
      <div class="torrent-list compact-variant-list">
        {#if preferred}
          {@render variantRow(preferred, true, true)}
        {/if}
      </div>
    {/if}
  </div>
{/if}
