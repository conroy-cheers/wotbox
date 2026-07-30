<script lang="ts">
  import { createQuery, useQueryClient } from "@tanstack/svelte-query";
  import { ArrowDown, ArrowUp, CirclePause, GripVertical, Play, Radio, RotateCcw, Save } from "@lucide/svelte";
  import {
    api,
    type ChannelConfig,
    type ChannelOverview,
    type PublicConfig,
    type ProviderPolicyOverride,
    type ProviderStatus,
    type QualityPreference,
    type RuntimePreferences,
    type TrackerPreference,
    type VariantSortCriterion
  } from "../lib/api";
  import {
    defaultReleasePreferences,
    qualityLabels
  } from "../lib/releasePreferences";
  import TierEditor from "../lib/TierEditor.svelte";

  const queryClient = useQueryClient();
  const preferences = createQuery({
    queryKey: ["preferences"],
    queryFn: () => api<RuntimePreferences>("/api/v1/preferences")
  });
  const channels = createQuery({
    queryKey: ["channels"],
    queryFn: () => api<ChannelOverview[]>("/api/v1/channels")
  });
  const config = createQuery({
    queryKey: ["config"],
    queryFn: () => api<PublicConfig>("/api/v1/config")
  });
  const providers = createQuery({
    queryKey: ["providers"],
    queryFn: () => api<ProviderStatus[]>("/api/v1/providers"),
    refetchInterval: 10_000
  });
  let qualityTiers = $state<QualityPreference[][]>(
    structuredClone(defaultReleasePreferences.qualityTiers)
  );
  let qualityCutoffIndex = $state(defaultReleasePreferences.qualityCutoffIndex);
  let mediaTiers = $state<string[][]>(structuredClone(defaultReleasePreferences.mediaTiers));
  let mediaCutoffIndex = $state(defaultReleasePreferences.mediaCutoffIndex);
  let variantSortOrder = $state<VariantSortCriterion[]>(
    [...defaultReleasePreferences.variantSortOrder]
  );
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
  let apiPolicies = $state<Record<string, ProviderPolicyOverride>>({});
  let providerAction = $state("");
  let providerError = $state("");

  const criterionLabels: Record<VariantSortCriterion, string> = {
    quality: "Quality",
    tracker: "Tracker",
    media: "Media",
    edition: "Enhanced edition"
  };
  let draggedCriterion = $state<number | null>(null);

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
    qualityTiers = structuredClone(value.release.qualityTiers);
    qualityCutoffIndex = value.release.qualityCutoffIndex;
    mediaTiers = structuredClone(value.release.mediaTiers);
    mediaCutoffIndex = value.release.mediaCutoffIndex;
    variantSortOrder = [...value.release.variantSortOrder];
    trackerOrder = [...value.release.trackerOrder];
    trackerPolicies = structuredClone(value.release.trackerPolicies);
    apiPolicies = structuredClone(value.api?.providers ?? {});
  }

  function moveTracker(index: number, delta: number) {
    const destination = index + delta;
    if (destination < 0 || destination >= trackerOrder.length) return;
    const next = [...trackerOrder];
    [next[index], next[destination]] = [next[destination], next[index]];
    trackerOrder = next;
  }

  function moveCriterion(index: number, destination: number) {
    if (destination < 0 || destination >= variantSortOrder.length || index === destination) return;
    const next = [...variantSortOrder];
    const [criterion] = next.splice(index, 1);
    next.splice(destination, 0, criterion);
    variantSortOrder = next;
  }

  function dropCriterion(destination: number) {
    if (draggedCriterion === null) return;
    moveCriterion(draggedCriterion, destination);
    draggedCriterion = null;
  }

  function trackerPolicy(name: string): TrackerPreference {
    let policy = trackerPolicies.find((candidate) => candidate.tracker === name);
    if (!policy) {
      policy = {
        tracker: name,
        mode: "freeleech_only",
        autoUseTokens: false,
        downloadProfile: $config.data?.downloadProfiles.includes(name) ? name : undefined,
        autoTokenLimit: 0
      };
      trackerPolicies = [...trackerPolicies, policy];
    }
    return policy;
  }

  function payload(): RuntimePreferences {
    return {
      release: {
        qualityTiers: structuredClone(qualityTiers),
        qualityCutoffIndex,
        mediaTiers: structuredClone(mediaTiers),
        mediaCutoffIndex,
        variantSortOrder: [...variantSortOrder],
        trackerOrder: [...trackerOrder],
        trackerPolicies: trackerOrder.map((tracker) => {
          const policy = trackerPolicy(tracker);
          return {
            ...policy,
            downloadProfile: policy.downloadProfile || undefined,
            autoTokenLimit: Math.max(0, Math.min(100, Number(policy.autoTokenLimit) || 0))
          };
        })
      },
      api: { providers: structuredClone(apiPolicies) }
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
    load({
      release: structuredClone(defaultReleasePreferences),
      api: { providers: {} }
    });
    saved = false;
  }

  function providerPolicy(provider: ProviderStatus): ProviderPolicyOverride {
    return apiPolicies[provider.id] ?? {};
  }

  function setProviderPolicy(
    provider: ProviderStatus,
    field: keyof ProviderPolicyOverride,
    raw: string
  ) {
    const current = providerPolicy(provider);
    const value = raw === "" ? undefined : Math.max(1, Number(raw) || 1);
    apiPolicies = {
      ...apiPolicies,
      [provider.id]: { ...current, [field]: value }
    };
  }

  async function controlProvider(provider: ProviderStatus, action: "pause" | "resume") {
    providerAction = provider.id;
    providerError = "";
    try {
      await api<ProviderStatus>(
        `/api/v1/providers/${encodeURIComponent(provider.id)}/${action}`,
        { method: "POST" }
      );
      await queryClient.invalidateQueries({ queryKey: ["providers"] });
    } catch (cause) {
      providerError = cause instanceof Error ? cause.message : "Unable to update provider";
    } finally {
      providerAction = "";
    }
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

  function modeHelp(mode: TrackerPreference["mode"]): string {
    switch (mode) {
      case "disabled":
        return "Never plan downloads from this tracker.";
      case "freeleech_only":
        return "Only plan torrents that do not debit your tracker buffer.";
      case "freeleech_or_token":
        return "Plan freeleech torrents and neutral-leech torrents covered by a token.";
      case "any":
        return "Allow downloads that may debit your tracker buffer.";
    }
  }

  function tokenSummary(policy: TrackerPreference): string {
    if (policy.mode !== "freeleech_or_token") return "Token automation does not apply to this download rule.";
    if (!policy.autoUseTokens) return "Token spending is disabled; token-only releases will not be planned.";
    if (Number(policy.autoTokenLimit) === 0) return "No tokens may be spent automatically in a pack.";
    return `Up to ${policy.autoTokenLimit} ${Number(policy.autoTokenLimit) === 1 ? "token" : "tokens"} may be spent automatically per pack.`;
  }
</script>

<svelte:head><title>Preferences · Wotbox</title></svelte:head>

<header class="page-heading compact">
  <div>
    <p class="eyebrow">Runtime settings</p>
    <h1>Preferences</h1>
    <p>Control how Wotbox chooses downloads and builds recommendation packs.</p>
  </div>
</header>

<nav class="settings-nav" aria-label="Preference sections">
  <a href="#download-planning"><strong>Download planning</strong><span>Trackers, cost, and profiles</span></a>
  <a href="#quality-media"><strong>Quality &amp; media</strong><span>Cutoff and format ranking</span></a>
  <a href="#api-safety"><strong>API safety</strong><span>Limits and circuit state</span></a>
  <a href="#channels"><strong>Channels</strong><span>Sources and schedules</span></a>
</nav>

{#if $preferences.isPending}
  <div class="preferences-panel skeleton-card"></div>
{:else if $preferences.isError}
  <div class="error-panel">{$preferences.error.message}</div>
{:else}
  <section class="preferences-panel tracker-settings-panel" id="download-planning">
    <div class="section-heading">
      <div><p class="eyebrow">Download planning</p><h2>Trackers, cost, and destination</h2></div>
    </div>
    <p class="settings-help">Trackers are tried from top to bottom. Each card controls which downloads are allowed, where they are sent, and whether a recommendation pack may spend tokens.</p>
    {#if $config.isError}
      <div class="error-panel compact">Download profiles could not be loaded: {$config.error.message}</div>
    {:else if $config.data}
      <div class="settings-context" aria-label="Available configuration">
        <span><strong>{$config.data.trackers.length}</strong> configured trackers</span>
        <span><strong>{$config.data.downloadProfiles.length}</strong> download profiles</span>
      </div>
    {/if}
    <div class="tracker-policy-list">
      {#each trackerOrder as tracker, index}
        {@const policy = trackerPolicy(tracker)}
        <article class="tracker-policy-card">
          <header>
            <div class="tracker-identity">
              <span class="preference-rank">{index + 1}</span>
              <div>
                <p class="eyebrow">Tracker priority {index + 1}</p>
                <h3>{tracker.toUpperCase()}</h3>
              </div>
            </div>
            <div class="reorder-buttons">
              <button aria-label={`Move ${tracker} up`} disabled={index === 0} onclick={() => moveTracker(index, -1)}><ArrowUp size={15} /></button>
              <button aria-label={`Move ${tracker} down`} disabled={index === trackerOrder.length - 1} onclick={() => moveTracker(index, 1)}><ArrowDown size={15} /></button>
            </div>
          </header>
          <div class="tracker-policy-fields">
            <label class="dialog-field">
              <span>Allowed download cost</span>
              <select bind:value={policy.mode}>
                <option value="disabled">Disabled</option>
                <option value="freeleech_only">Freeleech only</option>
                <option value="freeleech_or_token">Freeleech or token</option>
                <option value="any">Any torrent, including charged</option>
              </select>
              <small>{modeHelp(policy.mode)}</small>
            </label>
            <label class="dialog-field">
              <span>Download profile</span>
              <select bind:value={policy.downloadProfile} disabled={$config.isPending}>
                <option value={undefined}>Not configured</option>
                {#each $config.data?.downloadProfiles ?? [] as profile}
                  <option value={profile}>{profile}</option>
                {/each}
              </select>
              <small>Chooses the download client, save path, and tags.</small>
            </label>
          </div>
          <fieldset class="token-policy" disabled={policy.mode !== "freeleech_or_token"}>
            <legend>Token automation</legend>
            <label class="inline-check token-auto-toggle">
              <input type="checkbox" bind:checked={policy.autoUseTokens} />
              Automatically spend tokens when a planned torrent needs one
            </label>
            <label class="dialog-field token-limit">
              <span>Maximum tokens per recommendation pack</span>
              <input
                type="number"
                min="0"
                max="100"
                bind:value={policy.autoTokenLimit}
                disabled={!policy.autoUseTokens}
              />
            </label>
            <p>
              {#if policy.tracker.toLowerCase() === "ops"}
                <strong>OPS rate: 1 token per 320 MiB, rounded up per torrent.</strong>
              {:else if policy.tracker.toLowerCase() === "red"}
                <strong>RED rate: 1 token per eligible torrent.</strong>
              {/if}
              <br />
              {tokenSummary(policy)}
              {#if policy.mode === "freeleech_or_token"}
                After the limit is reached, remaining releases are marked <strong>Token budget exceeded</strong>.
              {/if}
            </p>
          </fieldset>
        </article>
      {/each}
    </div>
  </section>

  <div class="preferences-grid quality-media-grid" id="quality-media">
    <section class="preferences-panel">
      <div class="section-heading">
        <div><p class="eyebrow">Quality &amp; media</p><h2>Quality ranking</h2></div>
      </div>
      <p class="settings-help">Drag items onto a row to tie them, between rows to separate them, or move the cutoff itself.</p>
      <TierEditor
        bind:tiers={qualityTiers}
        bind:cutoffIndex={qualityCutoffIndex}
        labels={qualityLabels}
      />
    </section>

    <section class="preferences-panel">
      <div class="section-heading">
        <div><p class="eyebrow">Quality tie-break</p><h2>Media preference</h2></div>
      </div>
      <p class="settings-help">Digital and optical formats start above the cutoff. Vinyl, cassette, and unknown media are rejected by default.</p>
      <TierEditor bind:tiers={mediaTiers} bind:cutoffIndex={mediaCutoffIndex} />
    </section>
  </div>

  <section class="preferences-panel criterion-order-panel">
    <div class="section-heading">
      <div><p class="eyebrow">Variant tie-breaks</p><h2>Decision order</h2></div>
    </div>
    <p class="settings-help">Drag the criteria into the order Wotbox should compare otherwise eligible torrents. Seeder count is always the final tie-breaker.</p>
    <div class="criterion-list">
      {#each variantSortOrder as criterion, index}
        <div
          role="group"
          aria-label={`${criterionLabels[criterion]} sort criterion`}
          class="preference-row criterion-row"
          draggable="true"
          ondragstart={() => draggedCriterion = index}
          ondragover={(event) => event.preventDefault()}
          ondrop={() => dropCriterion(index)}
          ondragend={() => draggedCriterion = null}
        >
          <GripVertical size={16} />
          <span class="preference-rank">{index + 1}</span>
          <strong>{criterionLabels[criterion]}</strong>
          <div class="reorder-buttons">
            <button type="button" aria-label={`Move ${criterionLabels[criterion]} up`} disabled={index === 0} onclick={() => moveCriterion(index, index - 1)}><ArrowUp size={15} /></button>
            <button type="button" aria-label={`Move ${criterionLabels[criterion]} down`} disabled={index === variantSortOrder.length - 1} onclick={() => moveCriterion(index, index + 1)}><ArrowDown size={15} /></button>
          </div>
        </div>
      {/each}
    </div>
  </section>

  {#if error}<div class="error-panel compact">{error}</div>{/if}
  <div class="settings-actions preference-save-actions">
    <span>These settings affect search results and every newly planned or replanned pack.</span>
    <div>
      <button class="secondary-button" onclick={restoreDefaults}><RotateCcw size={16} /> Restore defaults</button>
      <button class="primary-button" disabled={saving} onclick={save}><Save size={16} /> {saving ? "Saving…" : saved ? "Saved" : "Save download preferences"}</button>
    </div>
  </div>

  <section class="preferences-panel provider-preferences" id="api-safety">
    <div class="section-heading">
      <div><p class="eyebrow">External services</p><h2>API safety</h2></div>
    </div>
    <p class="settings-help">
      Every request—including retries and background work—passes through these shared limits.
      Values may only be made more conservative than Wotbox’s built-in safe defaults.
    </p>
    {#if $providers.isPending}
      <div class="skeleton-card"></div>
    {:else if $providers.isError}
      <div class="error-panel compact">{$providers.error.message}</div>
    {:else}
      <div class="provider-policy-list">
        {#each $providers.data ?? [] as provider}
          {@const policy = providerPolicy(provider)}
          {@const queued = Object.values(provider.queued).reduce((sum, value) => sum + value, 0)}
          <article class="provider-policy-card">
            <header>
              <div>
                <p class="eyebrow">{provider.kind.replaceAll("_", " ")}</p>
                <h3>{provider.displayName}</h3>
              </div>
              <span class={`provider-state ${provider.state}`}>{provider.state.replaceAll("_", " ")}</span>
            </header>
            {#if provider.message}
              <p class="provider-message">{provider.message}</p>
            {/if}
            <div class="provider-metrics">
              <span><strong>{queued}</strong> queued</span>
              <span><strong>{provider.consecutiveFailures}</strong> consecutive failures</span>
              {#if provider.retryAt}<span>Retry after <strong>{new Date(provider.retryAt).toLocaleString()}</strong></span>{/if}
            </div>
            <div class="provider-controls">
              <label class="dialog-field">
                <span>Minimum interval (ms)</span>
                <input
                  type="number"
                  min={provider.safeMinimumIntervalMs}
                  step="100"
                  value={policy.minimumIntervalMs ?? provider.minimumIntervalMs}
                  onchange={(event) =>
                    setProviderPolicy(provider, "minimumIntervalMs", event.currentTarget.value)}
                />
                <small>Safe minimum: {provider.safeMinimumIntervalMs.toLocaleString()} ms</small>
              </label>
              <label class="dialog-field">
                <span>Maximum concurrency</span>
                <input
                  type="number"
                  min="1"
                  max={provider.safeMaxConcurrency}
                  value={policy.maxConcurrency ?? provider.maxConcurrency}
                  onchange={(event) =>
                    setProviderPolicy(provider, "maxConcurrency", event.currentTarget.value)}
                />
                <small>Safe maximum: {provider.safeMaxConcurrency}</small>
              </label>
            </div>
            <div class="provider-actions">
              {#if provider.canResume}
                <button
                  class="secondary-button"
                  disabled={providerAction === provider.id}
                  onclick={() => controlProvider(provider, "resume")}
                ><Play size={15} /> Resume cautiously</button>
              {:else if provider.canPause}
                <button
                  class="secondary-button"
                  disabled={providerAction === provider.id}
                  onclick={() => controlProvider(provider, "pause")}
                ><CirclePause size={15} /> Pause</button>
              {/if}
              <small>Last success: {provider.lastSuccessAt ? new Date(provider.lastSuccessAt).toLocaleString() : "Never"}</small>
            </div>
          </article>
        {/each}
      </div>
    {/if}
    {#if providerError}<div class="error-panel compact">{providerError}</div>{/if}
    <div class="settings-actions">
      <span>Limit changes apply immediately after saving.</span>
      <button class="primary-button" disabled={saving} onclick={save}>
        <Save size={16} /> {saving ? "Saving…" : saved ? "Saved" : "Save API limits"}
      </button>
    </div>
  </section>

  <section class="preferences-panel channel-preferences" id="channels">
    <div class="section-heading">
      <div><p class="eyebrow">Scheduled discovery</p><h2>Recommendation channels</h2></div>
      <Radio size={22} />
    </div>
    <p class="settings-help">Each channel saves independently and builds a new pack at its own weekly time. Refreshing creates a plan but never starts downloads.</p>
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
            <div class="channel-setting-group">
              <h4>Recommendation source</h4>
              {#if channel.countryChart}
                <label class="dialog-field">
                  <span>Chart country</span>
                  <input maxlength="2" bind:value={channel.countryChart.country} placeholder="AU" />
                  <small>Two-letter country code, such as AU, GB, or US.</small>
                </label>
              {/if}
              {#if channel.lastfm}
                <div class="connection-status" class:good={channel.credentialConfigured}>
                  <span></span>
                  Last.fm API key {channel.credentialConfigured ? "configured" : "not configured"}
                </div>
                <label class="dialog-field">
                  <span>Last.fm username</span>
                  <input bind:value={channel.lastfm.username} placeholder="Username" />
                  <small>Recommendations are derived from this account’s listening history.</small>
                </label>
                <div class="channel-setting-row">
                  <label class="dialog-field">
                    <span>Listening history</span>
                    <select bind:value={channel.lastfm.period}>
                      <option value="7day">Last 7 days</option>
                      <option value="1month">Last month</option>
                      <option value="3month">Last 3 months</option>
                      <option value="6month">Last 6 months</option>
                      <option value="12month">Last year</option>
                      <option value="overall">All time</option>
                    </select>
                  </label>
                  <label class="dialog-field">
                    <span>Releases per pack</span>
                    <input type="number" min="1" max="100" bind:value={channel.lastfm.packSize} />
                  </label>
                  <label class="dialog-field">
                    <span>Repeat suppression</span>
                    <input type="number" min="0" max="52" bind:value={channel.lastfm.suppressionPacks} />
                    <small>Exclude releases seen in this many previous packs.</small>
                  </label>
                  <label class="dialog-field">
                    <span>Apple catalog country</span>
                    <input maxlength="2" bind:value={channel.lastfm.catalogCountry} placeholder="AU" />
                    <small>Used only to map unresolved singles to albums.</small>
                  </label>
                </div>
                {#if !channel.credentialConfigured}
                  <div class="notice-banner compact">Set <code>lastfm_api_key_file</code> or <code>LASTFM_API_KEY</code> before enabling this channel.</div>
                {/if}
              {/if}
            </div>
            <div class="channel-setting-group">
              <h4>Refresh schedule</h4>
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
                <label class="dialog-field"><span>Time</span><input type="time" bind:value={channel.schedule.time} /></label>
                <label class="dialog-field">
                  <span>Timezone</span>
                  <input bind:value={channel.schedule.timezone} />
                  <small>IANA name, such as Australia/Melbourne.</small>
                </label>
              </div>
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
{/if}
