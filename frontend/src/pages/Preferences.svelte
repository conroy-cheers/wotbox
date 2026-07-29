<script lang="ts">
  import { createQuery, useQueryClient } from "@tanstack/svelte-query";
  import { ArrowDown, ArrowUp, RotateCcw, Save } from "@lucide/svelte";
  import {
    api,
    type QualityPreference,
    type RuntimePreferences,
    type TrackerPreference
  } from "../lib/api";
  import {
    defaultReleasePreferences,
    qualityLabels
  } from "../lib/releasePreferences";

  const queryClient = useQueryClient();
  const preferences = createQuery({
    queryKey: ["preferences"],
    queryFn: () => api<RuntimePreferences>("/api/v1/preferences")
  });
  let qualityOrder = $state<QualityPreference[]>([...defaultReleasePreferences.qualityOrder]);
  let minimumQuality = $state<QualityPreference>(defaultReleasePreferences.minimumQuality);
  let mediaPriorities = $state<Record<string, number>>({});
  let trackerOrder = $state<string[]>([...defaultReleasePreferences.trackerOrder]);
  let trackerPolicies = $state<TrackerPreference[]>(
    structuredClone(defaultReleasePreferences.trackerPolicies)
  );
  let loaded = $state(false);
  let saving = $state(false);
  let saved = $state(false);
  let error = $state("");

  const knownMedia = ["WEB", "CD", "Vinyl", "SACD", "DVD", "Blu-ray", "Cassette", "Other"];

  $effect(() => {
    if (!loaded && $preferences.data) {
      load($preferences.data);
      loaded = true;
    }
  });

  function load(value: RuntimePreferences) {
    qualityOrder = [...value.release.qualityOrder];
    minimumQuality = value.release.minimumQuality;
    mediaPriorities = Object.fromEntries(
      value.release.mediaTiers.flatMap((tier, index) =>
        tier.map((media) => [media, index + 1])
      )
    );
    for (const media of knownMedia) mediaPriorities[media] ??= knownMedia.length;
    trackerOrder = [...value.release.trackerOrder];
    trackerPolicies = structuredClone(value.release.trackerPolicies);
  }

  function moveQuality(index: number, delta: number) {
    const destination = index + delta;
    if (destination < 0 || destination >= qualityOrder.length) return;
    const next = [...qualityOrder];
    [next[index], next[destination]] = [next[destination], next[index]];
    qualityOrder = next;
  }

  function moveTracker(index: number, delta: number) {
    const destination = index + delta;
    if (destination < 0 || destination >= trackerOrder.length) return;
    const next = [...trackerOrder];
    [next[index], next[destination]] = [next[destination], next[index]];
    trackerOrder = next;
  }

  function trackerPolicy(name: string): TrackerPreference {
    let policy = trackerPolicies.find((candidate) => candidate.tracker === name);
    if (!policy) {
      policy = { tracker: name, mode: "freeleech_only", autoUseTokens: false };
      trackerPolicies = [...trackerPolicies, policy];
    }
    return policy;
  }

  function payload(): RuntimePreferences {
    const grouped = new Map<number, string[]>();
    for (const media of knownMedia) {
      const priority = Math.max(1, Number(mediaPriorities[media]) || knownMedia.length);
      grouped.set(priority, [...(grouped.get(priority) ?? []), media]);
    }
    return {
      release: {
        qualityOrder: [...qualityOrder],
        minimumQuality,
        mediaTiers: [...grouped.entries()]
          .sort(([left], [right]) => left - right)
          .map(([, values]) => values),
        trackerOrder: [...trackerOrder],
        trackerPolicies: trackerOrder.map((tracker) => ({ ...trackerPolicy(tracker) }))
      }
    };
  }

  async function save() {
    saving = true;
    saved = false;
    error = "";
    try {
      const value = await api<RuntimePreferences>("/api/v1/preferences", {
        method: "PUT",
        body: JSON.stringify(payload())
      });
      queryClient.setQueryData(["preferences"], value);
      load(value);
      saved = true;
    } catch (cause) {
      error = cause instanceof Error ? cause.message : "Unable to save preferences";
    } finally {
      saving = false;
    }
  }

  function restoreDefaults() {
    load({ release: structuredClone(defaultReleasePreferences) });
    saved = false;
  }
</script>

<svelte:head><title>Preferences · Wotbox</title></svelte:head>

<header class="page-heading compact">
  <div>
    <p class="eyebrow">Runtime settings</p>
    <h1>Release preferences</h1>
    <p>Choose which torrent is presented first and set the lowest quality Wotbox may add.</p>
  </div>
</header>

{#if $preferences.isPending}
  <div class="preferences-panel skeleton-card"></div>
{:else if $preferences.isError}
  <div class="error-panel">{$preferences.error.message}</div>
{:else}
  <div class="preferences-grid">
    <section class="preferences-panel">
      <div class="section-heading">
        <div><p class="eyebrow">First priority</p><h2>Trackers and download cost</h2></div>
      </div>
      <p class="settings-help">The first tracker with an eligible above-cutoff variant wins. “Free or token” never permits a charged download.</p>
      <div class="preference-list">
        {#each trackerOrder as tracker, index}
          {@const policy = trackerPolicy(tracker)}
          <div class="preference-row tracker-preference">
            <span class="preference-rank">{index + 1}</span>
            <strong>{tracker.toUpperCase()}</strong>
            <select bind:value={policy.mode}>
              <option value="disabled">Disabled</option>
              <option value="freeleech_only">Already free only</option>
              <option value="freeleech_or_token">Free or token</option>
              <option value="any">Allow charged</option>
            </select>
            <label class="inline-check">
              <input
                type="checkbox"
                bind:checked={policy.autoUseTokens}
                disabled={policy.mode === "disabled" || policy.mode === "freeleech_only"}
              />
              Auto-spend tokens
            </label>
            <div class="reorder-buttons">
              <button aria-label={`Move ${tracker} up`} disabled={index === 0} onclick={() => moveTracker(index, -1)}><ArrowUp size={15} /></button>
              <button aria-label={`Move ${tracker} down`} disabled={index === trackerOrder.length - 1} onclick={() => moveTracker(index, 1)}><ArrowDown size={15} /></button>
            </div>
          </div>
        {/each}
      </div>
    </section>

    <section class="preferences-panel">
      <div class="section-heading">
        <div><p class="eyebrow">Within a tracker</p><h2>Quality order</h2></div>
      </div>
      <p class="settings-help">Higher rows win. The cutoff is inclusive: lower qualities remain visible when expanded, but cannot be added.</p>
      <div class="preference-list">
        {#each qualityOrder as quality, index}
          <div class="preference-row">
            <span class="preference-rank">{index + 1}</span>
            <strong>{qualityLabels[quality]}</strong>
            <div class="reorder-buttons">
              <button aria-label={`Move ${qualityLabels[quality]} up`} disabled={index === 0} onclick={() => moveQuality(index, -1)}><ArrowUp size={15} /></button>
              <button aria-label={`Move ${qualityLabels[quality]} down`} disabled={index === qualityOrder.length - 1} onclick={() => moveQuality(index, 1)}><ArrowDown size={15} /></button>
            </div>
          </div>
        {/each}
      </div>
      <label class="cutoff-select">
        <span>Minimum downloadable quality</span>
        <select bind:value={minimumQuality}>
          {#each qualityOrder as quality}<option value={quality}>{qualityLabels[quality]}</option>{/each}
        </select>
      </label>
    </section>

    <section class="preferences-panel">
      <div class="section-heading">
        <div><p class="eyebrow">Quality tie-break</p><h2>Source format</h2></div>
      </div>
      <p class="settings-help">Set the same priority number to tie sources. Seeder popularity breaks ties.</p>
      <div class="preference-list">
        {#each knownMedia as media}
          <label class="preference-row media-preference">
            <strong>{media}</strong>
            <span>Priority</span>
            <input type="number" min="1" max={knownMedia.length} bind:value={mediaPriorities[media]} />
          </label>
        {/each}
      </div>
    </section>
  </div>

  {#if error}<div class="error-panel compact">{error}</div>{/if}
  <div class="settings-actions">
    <button class="secondary-button" onclick={restoreDefaults}><RotateCcw size={16} /> Restore defaults</button>
    <button class="primary-button" disabled={saving} onclick={save}><Save size={16} /> {saving ? "Saving…" : saved ? "Saved" : "Save preferences"}</button>
  </div>
{/if}
