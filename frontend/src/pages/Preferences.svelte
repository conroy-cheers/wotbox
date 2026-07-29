<script lang="ts">
  import { createQuery, useQueryClient } from "@tanstack/svelte-query";
  import { ArrowDown, ArrowUp, Radio, RotateCcw, Save } from "@lucide/svelte";
  import {
    api,
    type ChannelConfig,
    type ChannelOverview,
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
  const channels = createQuery({
    queryKey: ["channels"],
    queryFn: () => api<ChannelOverview[]>("/api/v1/channels")
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
  let channelDrafts = $state<ChannelConfig[]>([]);
  let channelLoaded = $state(false);
  let channelSaving = $state("");
  let channelError = $state("");

  const knownMedia = ["WEB", "CD", "Vinyl", "SACD", "DVD", "Blu-ray", "Cassette", "Other"];

  $effect(() => {
    if (!loaded && $preferences.data) {
      load($preferences.data);
      loaded = true;
    }
  });
  $effect(() => {
    if (!channelLoaded && $channels.data) {
      channelDrafts = structuredClone($channels.data.map((overview) => overview.channel));
      channelLoaded = true;
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

  async function saveChannel(channel: ChannelConfig) {
    channelSaving = channel.id;
    channelError = "";
    try {
      const saved = await api<ChannelConfig>(`/api/v1/channels/${channel.id}`, {
        method: "PUT",
        body: JSON.stringify(channel)
      });
      const index = channelDrafts.findIndex((candidate) => candidate.id === channel.id);
      channelDrafts[index] = saved;
      channelDrafts = [...channelDrafts];
      await queryClient.invalidateQueries({ queryKey: ["channels"] });
    } catch (cause) {
      channelError = cause instanceof Error ? cause.message : "Unable to save channel";
    } finally {
      channelSaving = "";
    }
  }

  function channelLabel(id: string): string {
    return id === "country_chart" ? "Country Top 100" : "Last.fm Discovery";
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

  <section class="preferences-panel channel-preferences" id="channels">
    <div class="section-heading">
      <div><p class="eyebrow">Scheduled discovery</p><h2>Recommendation channels</h2></div>
      <Radio size={22} />
    </div>
    <p class="settings-help">Each enabled channel builds a new pack at its own local weekly time. Refreshing never downloads anything automatically.</p>
    {#if $channels.isPending}
      <div class="skeleton-card"></div>
    {:else if $channels.isError}
      <div class="error-panel compact">{$channels.error.message}</div>
    {:else}
      <div class="channel-settings-grid">
        {#each channelDrafts as channel}
          <div class="channel-setting-card">
            <header>
              <div><p class="eyebrow">{channel.kind.replace("_", " ")}</p><h3>{channelLabel(channel.id)}</h3></div>
              <label class="inline-check"><input type="checkbox" bind:checked={channel.enabled} /> Enabled</label>
            </header>
            {#if channel.countryChart}
              <label class="dialog-field">
                <span>Country code</span>
                <input maxlength="2" bind:value={channel.countryChart.country} placeholder="AU" />
              </label>
            {/if}
            {#if channel.lastfm}
              <label class="dialog-field">
                <span>Last.fm username</span>
                <input bind:value={channel.lastfm.username} placeholder="Username" />
              </label>
              <div class="channel-setting-row">
                <label class="dialog-field">
                  <span>Listening seed</span>
                  <select bind:value={channel.lastfm.period}>
                    <option value="7day">Last 7 days</option>
                    <option value="1month">Last month</option>
                    <option value="3month">Last 3 months</option>
                    <option value="6month">Last 6 months</option>
                    <option value="12month">Last year</option>
                    <option value="overall">All time</option>
                  </select>
                </label>
                <label class="dialog-field"><span>Pack size</span><input type="number" min="1" max="100" bind:value={channel.lastfm.packSize} /></label>
                <label class="dialog-field"><span>Suppress packs</span><input type="number" min="0" max="52" bind:value={channel.lastfm.suppressionPacks} /></label>
              </div>
              {#if !channel.credentialConfigured}
                <div class="notice-banner compact">Set <code>lastfm_api_key_file</code> or <code>LASTFM_API_KEY</code> before enabling this channel.</div>
              {/if}
            {/if}
            <div class="channel-setting-row schedule-row">
              <label class="dialog-field">
                <span>Weekday</span>
                <select bind:value={channel.schedule.weekday}>
                  <option value={1}>Monday</option><option value={2}>Tuesday</option>
                  <option value={3}>Wednesday</option><option value={4}>Thursday</option>
                  <option value={5}>Friday</option><option value={6}>Saturday</option>
                  <option value={7}>Sunday</option>
                </select>
              </label>
              <label class="dialog-field"><span>Local time</span><input type="time" bind:value={channel.schedule.time} /></label>
              <label class="dialog-field"><span>IANA timezone</span><input bind:value={channel.schedule.timezone} /></label>
            </div>
            <button class="secondary-button" disabled={channelSaving === channel.id} onclick={() => saveChannel(channel)}>
              <Save size={15} /> {channelSaving === channel.id ? "Saving…" : "Save channel"}
            </button>
          </div>
        {/each}
      </div>
    {/if}
    {#if channelError}<div class="error-panel compact">{channelError}</div>{/if}
  </section>

  {#if error}<div class="error-panel compact">{error}</div>{/if}
  <div class="settings-actions">
    <button class="secondary-button" onclick={restoreDefaults}><RotateCcw size={16} /> Restore defaults</button>
    <button class="primary-button" disabled={saving} onclick={save}><Save size={16} /> {saving ? "Saving…" : saved ? "Saved" : "Save preferences"}</button>
  </div>
{/if}
