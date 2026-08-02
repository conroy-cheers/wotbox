<script lang="ts">
  import { createQuery, useQueryClient } from "@tanstack/svelte-query";
  import { ArrowDown, ArrowUp, CirclePause, GripVertical, Library, Play, Radio, RefreshCw, RotateCcw } from "@lucide/svelte";
  import { onMount } from "svelte";
  import {
    api,
    appPath,
    type ChannelConfig,
    type ChannelOverview,
    type PlexIntegrationStatus,
    type PlexScanQueued,
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
  import { buildPreferencePayload } from "../lib/preferencePayload";
  import TierEditor from "../lib/TierEditor.svelte";
  import BackgroundJobsPanel from "../lib/BackgroundJobsPanel.svelte";

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
  const plex = createQuery({
    queryKey: ["plex-integration"],
    queryFn: () => api<PlexIntegrationStatus>("/api/v1/integrations/plex"),
    refetchInterval: 5_000
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
  let error = $state("");
  let savedRuntimePayload = $state("");
  let runtimeSaveState = $state<"idle" | "pending" | "saving" | "error">("idle");
  let runtimeSaveInFlight = false;
  let channelDrafts = $state<ChannelConfig[]>([]);
  let channelLoaded = $state(false);
  let channelError = $state("");
  let savedChannelPayloads = $state<Record<string, string>>({});
  let channelSaveState = $state<"idle" | "pending" | "saving" | "error">("idle");
  let channelSaveInFlight = false;
  let apiPolicies = $state<Record<string, ProviderPolicyOverride>>({});
  let providerAction = $state("");
  let providerError = $state("");
  let plexScanning = $state(false);
  let plexMessage = $state("");
  let plexError = $state("");

  type PreferenceSection =
    | "download-planning"
    | "quality-media"
    | "api-safety"
    | "plex"
    | "channels"
    | "background-work";

  const sectionGroups: {
    label: string;
    items: { id: PreferenceSection; label: string; description: string }[];
  }[] = [
    {
      label: "Downloads",
      items: [
        { id: "download-planning", label: "Trackers & cost", description: "Priority, profiles, and tokens" },
        { id: "quality-media", label: "Quality & ranking", description: "Formats, cutoffs, and tie-breaks" }
      ]
    },
    {
      label: "Services",
      items: [
        { id: "api-safety", label: "API safety", description: "Limits and provider state" },
        { id: "plex", label: "Plex", description: "Library scan notifications" }
      ]
    },
    {
      label: "Automation",
      items: [
        { id: "channels", label: "Channels", description: "Sources and schedules" },
        { id: "background-work", label: "Background work", description: "Queue progress and failures" }
      ]
    }
  ];
  let activeSection = $state<PreferenceSection>("download-planning");

  const criterionLabels: Record<VariantSortCriterion, string> = {
    quality: "Quality",
    tracker: "Tracker",
    media: "Media",
    edition: "Enhanced edition"
  };
  let draggedCriterion = $state<number | null>(null);

  onMount(() => {
    function syncSectionFromHash() {
      const id = location.hash.slice(1);
      if (sectionGroups.some((group) => group.items.some((item) => item.id === id))) {
        activeSection = id as PreferenceSection;
      }
    }

    syncSectionFromHash();
    addEventListener("hashchange", syncSectionFromHash);
    return () => removeEventListener("hashchange", syncSectionFromHash);
  });

  $effect(() => {
    if (!loaded && $preferences.data) {
      load($preferences.data);
      savedRuntimePayload = JSON.stringify(payload());
      loaded = true;
    }
  });
  $effect(() => {
    if (!channelLoaded && $channels.data) {
      channelDrafts = structuredClone($channels.data.map((overview) => overview.channel));
      for (const channel of channelDrafts) {
        if (channel.countryChart && !Number.isInteger(channel.countryChart.albumCount)) {
          channel.countryChart.albumCount = 100;
        }
      }
      savedChannelPayloads = Object.fromEntries(
        channelDrafts.map((channel) => [channel.id, JSON.stringify(channel)])
      );
      channelLoaded = true;
    }
  });
  $effect(() => {
    if (!loaded) return;
    const serialized = JSON.stringify(payload());
    if (serialized === savedRuntimePayload) {
      if (!runtimeSaveInFlight) runtimeSaveState = "idle";
      return;
    }
    runtimeSaveState = "pending";
    const timer = setTimeout(() => void flushRuntimeAutosave(), 700);
    return () => clearTimeout(timer);
  });
  $effect(() => {
    if (!channelLoaded) return;
    const dirty = channelDrafts.some(
      (channel) => JSON.stringify(channel) !== savedChannelPayloads[channel.id]
    );
    if (!dirty) {
      if (!channelSaveInFlight) channelSaveState = "idle";
      return;
    }
    channelSaveState = "pending";
    const timer = setTimeout(() => void flushChannelAutosave(), 700);
    return () => clearTimeout(timer);
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
    return buildPreferencePayload({
      qualityTiers,
      qualityCutoffIndex,
      mediaTiers,
      mediaCutoffIndex,
      variantSortOrder,
      trackerOrder,
      trackerPolicies: trackerOrder.map((tracker) => trackerPolicy(tracker)),
      apiPolicies
    });
  }

  async function flushRuntimeAutosave() {
    if (runtimeSaveInFlight) return;
    const serialized = JSON.stringify(payload());
    if (serialized === savedRuntimePayload) return;
    runtimeSaveInFlight = true;
    runtimeSaveState = "saving";
    error = "";
    try {
      const value = await api<RuntimePreferences>("/api/v1/preferences", {
        method: "PUT",
        body: serialized
      });
      queryClient.setQueryData(["preferences"], value);
      savedRuntimePayload = serialized;
      runtimeSaveState = JSON.stringify(payload()) === serialized ? "idle" : "pending";
    } catch (cause) {
      error = cause instanceof Error ? cause.message : "Unable to save preferences";
      runtimeSaveState = "error";
      setTimeout(() => {
        if (JSON.stringify(payload()) !== savedRuntimePayload) void flushRuntimeAutosave();
      }, 5_000);
    } finally {
      runtimeSaveInFlight = false;
    }
  }

  function restoreDefaults() {
    if (!confirm("Restore all download, quality, and API safety preferences to their defaults?")) {
      return;
    }
    load({
      release: structuredClone(defaultReleasePreferences),
      api: { providers: {} }
    });
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

  async function flushChannelAutosave() {
    if (channelSaveInFlight) return;
    const changed = channelDrafts
      .map((channel) => ({ channel, serialized: JSON.stringify(channel) }))
      .filter(({ channel, serialized }) => serialized !== savedChannelPayloads[channel.id]);
    if (changed.length === 0) return;
    channelSaveInFlight = true;
    channelSaveState = "saving";
    channelError = "";
    try {
      for (const { channel, serialized } of changed) {
        await api<ChannelConfig>(`/api/v1/channels/${channel.id}`, {
          method: "PUT",
          body: serialized
        });
        savedChannelPayloads = { ...savedChannelPayloads, [channel.id]: serialized };
      }
      await queryClient.invalidateQueries({ queryKey: ["channels"] });
      channelSaveState = channelDrafts.some(
        (channel) => JSON.stringify(channel) !== savedChannelPayloads[channel.id]
      ) ? "pending" : "idle";
    } catch (cause) {
      channelError = cause instanceof Error ? cause.message : "Unable to save channel";
      channelSaveState = "error";
      setTimeout(() => {
        const dirty = channelDrafts.some(
          (channel) => JSON.stringify(channel) !== savedChannelPayloads[channel.id]
        );
        if (dirty) void flushChannelAutosave();
      }, 5_000);
    } finally {
      channelSaveInFlight = false;
    }
  }

  async function scanPlex() {
    plexScanning = true;
    plexMessage = "";
    plexError = "";
    try {
      const queued = await api<PlexScanQueued>("/api/v1/integrations/plex", { method: "POST" });
      plexMessage = queued.jobIds.length === 1
        ? "Plex library scan queued."
        : `${queued.jobIds.length} Plex library scans queued.`;
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ["plex-integration"] }),
        queryClient.invalidateQueries({ queryKey: ["background-jobs"] })
      ]);
    } catch (cause) {
      plexError = cause instanceof Error ? cause.message : "Unable to notify Plex";
    } finally {
      plexScanning = false;
    }
  }

  function channelLabel(channel: ChannelConfig): string {
    switch (channel.kind) {
      case "country_chart": return `Country Top ${channel.countryChart?.albumCount ?? 100}`;
      case "lastfm": return "Last.fm Discovery";
      case "trumped_downloads": return "Trumped downloads";
    }
  }

  function setCountryChartAlbumCount(channel: ChannelConfig, count: number) {
    if (channel.countryChart) channel.countryChart.albumCount = count;
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

  function selectSection(section: PreferenceSection) {
    activeSection = section;
    location.hash = section;
  }

  function autosaveState(): "loading" | "saved" | "saving" | "error" {
    if (!loaded || (!channelLoaded && $channels.isPending)) return "loading";
    if (runtimeSaveState === "error" || channelSaveState === "error") return "error";
    if ([runtimeSaveState, channelSaveState].some((state) => state === "pending" || state === "saving")) {
      return "saving";
    }
    return "saved";
  }
</script>

<svelte:head><title>Preferences · Wotbox</title></svelte:head>

<header class="page-heading compact settings-page-heading">
  <div>
    <p class="eyebrow">Runtime settings</p>
    <h1>Preferences</h1>
    <p>Control how Wotbox chooses downloads and builds recommendation packs.</p>
  </div>
  <div class:autosave-error={autosaveState() === "error"} class:autosave-saving={autosaveState() === "saving"} class="autosave-status" role="status">
    <span></span>
    <div>
      <strong>{autosaveState() === "loading" ? "Loading preferences…" : autosaveState() === "error" ? "Changes not saved" : autosaveState() === "saving" ? "Saving changes…" : "All changes saved"}</strong>
      <small>{autosaveState() === "error" ? error || channelError : "Preferences save automatically"}</small>
    </div>
  </div>
</header>

<div class="settings-workspace">
  <nav class="settings-menu" aria-label="Preference sections">
    {#each sectionGroups as group}
      <div class="settings-menu-group">
        <p>{group.label}</p>
        {#each group.items as item}
          <a
            href={appPath(`/preferences#${item.id}`)}
            class:active={activeSection === item.id}
            aria-current={activeSection === item.id ? "page" : undefined}
            onclick={() => activeSection = item.id}
          >
            <strong>{item.label}</strong>
            <span>{item.description}</span>
          </a>
        {/each}
      </div>
    {/each}
  </nav>

  <label class="settings-mobile-picker">
    <span>Settings section</span>
    <select value={activeSection} onchange={(event) => selectSection(event.currentTarget.value as PreferenceSection)}>
      {#each sectionGroups as group}
        <optgroup label={group.label}>
          {#each group.items as item}
            <option value={item.id}>{item.label}</option>
          {/each}
        </optgroup>
      {/each}
    </select>
  </label>

  <div class="settings-content">
  {#if $preferences.isPending}
    <div class="preferences-panel skeleton-card"></div>
  {:else if $preferences.isError}
    <div class="error-panel">{$preferences.error.message}</div>
  {:else if activeSection === "download-planning"}
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
              <span>Automatically spend tokens when a planned torrent needs one</span>
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

  {:else if activeSection === "quality-media"}
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

  <div class="settings-actions preference-reset-actions">
    <span>These settings affect search results and every newly planned or replanned pack.</span>
    <button class="secondary-button" onclick={restoreDefaults}><RotateCcw size={16} /> Restore all defaults</button>
  </div>

  {:else if activeSection === "api-safety"}
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
                <span>Background interval (ms)</span>
                <input
                  type="number"
                  min={provider.safeBackgroundMinimumIntervalMs}
                  step="100"
                  value={policy.backgroundMinimumIntervalMs ?? provider.backgroundMinimumIntervalMs}
                  onchange={(event) =>
                    setProviderPolicy(provider, "backgroundMinimumIntervalMs", event.currentTarget.value)}
                />
                <small>Safe background minimum: {provider.safeBackgroundMinimumIntervalMs.toLocaleString()} ms</small>
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
  </section>

  {:else if activeSection === "plex"}
  <section class="preferences-panel plex-preferences" id="plex">
    <div class="section-heading">
      <div><p class="eyebrow">Media server</p><h2>Plex library updates</h2></div>
      <Library size={22} />
    </div>
    <p class="settings-help">
      Wotbox queues a partial Plex scan after a music torrent first completes. Completions close
      together are combined, and failed notifications survive restarts and retry safely.
    </p>
    {#if $plex.isPending}
      <div class="skeleton-card"></div>
    {:else if $plex.isError}
      <div class="error-panel compact">{$plex.error.message}</div>
    {:else if !$plex.data?.configured}
      <div class="integration-empty">
        <strong>Plex is not configured</strong>
        <span>Add the server URL, token file, music section, and library roots to the server configuration.</span>
      </div>
    {:else}
      <div class="plex-status-grid">
        <div><span>Connection</span><strong class="status-good">Configured</strong></div>
        <div><span>Music section</span><strong>#{$plex.data.sectionId}</strong></div>
        <div><span>Queued scans</span><strong>{$plex.data.pendingScans}</strong></div>
      </div>
      <div class="plex-library-roots">
        <span>Partial-scan roots</span>
        {#each $plex.data.libraryRoots as root}
          <code>{root}</code>
        {/each}
      </div>
      <div class="integration-actions">
        <span>Queue a partial scan for every configured music root.</span>
        <button class="secondary-button" disabled={plexScanning} onclick={scanPlex}>
          <RefreshCw size={15} class={plexScanning ? "spin" : ""} /> {plexScanning ? "Queueing…" : "Scan now"}
        </button>
      </div>
    {/if}
    {#if plexMessage}<div class="success-panel compact">{plexMessage}</div>{/if}
    {#if plexError}<div class="error-panel compact">{plexError}</div>{/if}
  </section>

  {:else if activeSection === "background-work"}
  <BackgroundJobsPanel />

  {:else if activeSection === "channels"}
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
              <div><p class="eyebrow">{channel.kind.replaceAll("_", " ")}</p><h3>{channelLabel(channel)}</h3></div>
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
                <label class="dialog-field">
                  <span>Albums per pack</span>
                  <input type="number" min="1" max="100" bind:value={channel.countryChart.albumCount} />
                  <small>Choose any chart size from 1 to 100.</small>
                </label>
                <div class="chart-size-presets" aria-label="Album count presets">
                  <span>Quick choices</span>
                  {#each [10, 25, 50, 100] as count}
                    <button
                      type="button"
                      class="secondary-button compact-button"
                      class:active={channel.countryChart.albumCount === count}
                      onclick={() => setCountryChartAlbumCount(channel, count)}
                    >{count}</button>
                  {/each}
                </div>
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
              {#if channel.kind === "trumped_downloads"}
                <div class="notice-banner compact">
                  Includes completed downloads only when OPS rejects their hash and qBittorrent explicitly reports the torrent as unregistered. Refreshing searches for current replacement releases but never removes the old torrent.
                </div>
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
          </div>
        {/each}
      </div>
    {/if}
    {#if channelError}<div class="error-panel compact">{channelError}</div>{/if}
  </section>
  {/if}
  </div>
</div>
