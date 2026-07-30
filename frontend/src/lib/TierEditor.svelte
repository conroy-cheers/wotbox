<script lang="ts">
  import { ArrowDown, ArrowUp, GripVertical, Scissors } from "@lucide/svelte";

  let {
    tiers = $bindable(),
    cutoffIndex = $bindable(),
    labels = {}
  }: {
    tiers: string[][];
    cutoffIndex: number;
    labels?: Record<string, string>;
  } = $props();

  let dragged = $state<{ tier: number; item: string } | null>(null);
  let draggingCutoff = $state(false);

  function label(item: string) {
    return labels[item] ?? item;
  }

  function removeDragged(next: string[][], target: number) {
    if (!dragged) return { target, cutoff: cutoffIndex };
    let cutoff = cutoffIndex;
    const source = dragged.tier;
    const itemIndex = next[source]?.indexOf(dragged.item) ?? -1;
    if (itemIndex >= 0) next[source].splice(itemIndex, 1);
    if (next[source]?.length === 0) {
      next.splice(source, 1);
      if (source < cutoff) cutoff -= 1;
      if (source < target) target -= 1;
    }
    return { target, cutoff };
  }

  function dropOnRow(target: number) {
    if (!dragged) return;
    const next = tiers.map((tier) => [...tier]);
    const adjusted = removeDragged(next, target);
    if (!next[adjusted.target].includes(dragged.item)) {
      next[adjusted.target].push(dragged.item);
    }
    tiers = next;
    cutoffIndex = Math.min(adjusted.cutoff, next.length);
    dragged = null;
  }

  function dropAtBoundary(target: number) {
    if (draggingCutoff) {
      cutoffIndex = Math.max(0, Math.min(target, tiers.length));
      draggingCutoff = false;
      return;
    }
    if (!dragged) return;
    const next = tiers.map((tier) => [...tier]);
    const adjusted = removeDragged(next, target);
    next.splice(adjusted.target, 0, [dragged.item]);
    let cutoff = adjusted.cutoff;
    if (adjusted.target < cutoff) cutoff += 1;
    tiers = next;
    cutoffIndex = Math.min(cutoff, next.length);
    dragged = null;
  }

  function move(item: string, source: number, delta: number) {
    const target = source + delta;
    if (target < 0 || target >= tiers.length) return;
    dragged = { tier: source, item };
    dropOnRow(target);
  }

  function untie(item: string, source: number) {
    if (tiers[source].length < 2) return;
    dragged = { tier: source, item };
    dropAtBoundary(source + 1);
  }

  function allowDrop(event: DragEvent) {
    event.preventDefault();
    if (event.dataTransfer) event.dataTransfer.dropEffect = "move";
  }
</script>

<div class="tier-editor">
  {#each Array(tiers.length + 1) as _, boundary}
    <div
      role="group"
      aria-label={`Rank boundary ${boundary}`}
      class="tier-boundary"
      class:cutoff-boundary={cutoffIndex === boundary}
      ondragover={allowDrop}
      ondrop={() => dropAtBoundary(boundary)}
    >
      {#if cutoffIndex === boundary}
        <div
          role="group"
          aria-label="Acceptable cutoff"
          class="tier-cutoff"
          draggable="true"
          ondragstart={() => draggingCutoff = true}
          ondragend={() => draggingCutoff = false}
        >
          <GripVertical size={14} />
          <strong>Acceptable cutoff</strong>
          <span>Items below are never planned</span>
          <button
            type="button"
            aria-label="Move cutoff up"
            disabled={boundary === 0}
            onclick={() => cutoffIndex = boundary - 1}
          ><ArrowUp size={13} /></button>
          <button
            type="button"
            aria-label="Move cutoff down"
            disabled={boundary === tiers.length}
            onclick={() => cutoffIndex = boundary + 1}
          ><ArrowDown size={13} /></button>
        </div>
      {:else}
        <span class="tier-drop-hint">Drop here for a separate rank</span>
      {/if}
    </div>
    {#if boundary < tiers.length}
      <div
        role="group"
        aria-label={`Preference rank ${boundary + 1}`}
        class="tier-row"
        class:unacceptable-tier={boundary >= cutoffIndex}
        ondragover={allowDrop}
        ondrop={() => dropOnRow(boundary)}
      >
        <span class="preference-rank">{boundary + 1}</span>
        <div class="tier-chips">
          {#each tiers[boundary] as item}
            <div
              role="group"
              aria-label={label(item)}
              class="tier-chip"
              draggable="true"
              ondragstart={(event) => {
                dragged = { tier: boundary, item };
                if (event.dataTransfer) event.dataTransfer.effectAllowed = "move";
              }}
              ondragend={() => dragged = null}
            >
              <GripVertical size={14} />
              <strong>{label(item)}</strong>
              <div class="tier-chip-actions">
                <button
                  type="button"
                  aria-label={`Tie ${label(item)} with the row above`}
                  disabled={boundary === 0}
                  onclick={() => move(item, boundary, -1)}
                ><ArrowUp size={12} /></button>
                <button
                  type="button"
                  aria-label={`Tie ${label(item)} with the row below`}
                  disabled={boundary === tiers.length - 1}
                  onclick={() => move(item, boundary, 1)}
                ><ArrowDown size={12} /></button>
                <button
                  type="button"
                  aria-label={`Give ${label(item)} its own rank`}
                  disabled={tiers[boundary].length < 2}
                  onclick={() => untie(item, boundary)}
                ><Scissors size={12} /></button>
              </div>
            </div>
          {/each}
        </div>
      </div>
    {/if}
  {/each}
</div>
