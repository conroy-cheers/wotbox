<script lang="ts">
  import { createMutation, createQuery, useQueryClient } from "@tanstack/svelte-query";
  import { derived, writable } from "svelte/store";
  import { Dialog } from "bits-ui";
  import { Check } from "@lucide/svelte";
  import {
    api,
    type CreateDownload,
    type DownloadJob,
    type DownloadProfile,
    type DownloadSelection,
    type RuntimePreferences
  } from "./api";
  import { defaultReleasePreferences } from "./releasePreferences";

  let {
    selection,
    tracker,
    onclose,
    oncomplete
  }: {
    selection: DownloadSelection | null;
    tracker: string;
    onclose: () => void;
    oncomplete?: () => void;
  } = $props();

  let useToken = $state(false);
  let selectedProfile = $state("");
  let initializedTorrent = $state("");
  let submittedJob = $state<DownloadJob | null>(null);
  let monitorError = $state("");
  let completedJobId = "";
  const monitoredJobId = writable("");
  const queryClient = useQueryClient();
  const processing = $derived(
    submittedJob !== null
      && ["queued", "fetching_metadata", "submitting"].includes(submittedJob.state)
  );

  const profiles = createQuery({
    queryKey: ["download-profiles"],
    queryFn: () => api<DownloadProfile[]>("/api/v1/download-profiles")
  });
  const preferences = createQuery({
    queryKey: ["preferences"],
    queryFn: () => api<RuntimePreferences>("/api/v1/preferences")
  });
  const jobStatusOptions = derived(monitoredJobId, (id) => ({
    queryKey: ["download-job", id] as const,
    queryFn: () => api<DownloadJob>(`/api/v1/download-jobs/${id}`),
    enabled: Boolean(id)
  }));
  const jobStatus = createQuery(jobStatusOptions);
  const activeTracker = $derived(selection?.torrent.tracker ?? tracker);
  const policy = $derived(
    ($preferences.data?.release ?? defaultReleasePreferences).trackerPolicies
      .find((candidate) => candidate.tracker.toLowerCase() === activeTracker.toLowerCase())
      ?? {
        tracker: activeTracker,
        mode: "freeleech_only" as const,
        autoUseTokens: false,
        autoTokenLimit: 0
      }
  );
  const policyBlocked = $derived(Boolean(selection) && (
    policy.mode === "disabled"
    || (!selection!.torrent.freeleech && policy.mode === "freeleech_only")
    || (!selection!.torrent.freeleech
      && policy.mode === "freeleech_or_token"
      && (selection!.torrent.tokenEligibilityKnown ?? true)
      && !selection!.torrent.canUseToken)
    || selection!.torrent.eligibility?.eligible === false
  ));
  const requiresToken = $derived(Boolean(selection)
    && !selection!.torrent.freeleech
    && policy.mode === "freeleech_or_token");
  const tokenCost = $derived(selection?.torrent.eligibility?.tokenCost);

  $effect(() => {
    if (!selection) return;
    const initializationKey = `${activeTracker}:${selection.torrent.torrentId}:${policy.mode}:${policy.autoUseTokens}:${policy.downloadProfile ?? ""}`;
    if (initializedTorrent === initializationKey) return;
    initializedTorrent = initializationKey;
    const eligibilityKnown = selection.torrent.tokenEligibilityKnown ?? true;
    useToken = !selection.torrent.freeleech
      && policy.autoUseTokens
      && tokenCost !== undefined
      && (selection.torrent.canUseToken || !eligibilityKnown);
    selectedProfile = policy.downloadProfile
      ?? $profiles.data?.find((profile) => profile.name === activeTracker)?.name
      ?? $profiles.data?.[0]?.name
      ?? "";
  });

  $effect(() => {
    if (selection && !selectedProfile && $profiles.data?.length) {
      selectedProfile = policy.downloadProfile
        ?? $profiles.data.find((profile) => profile.name === activeTracker)?.name
        ?? $profiles.data[0].name;
    }
  });

  const addDownload = createMutation({
    mutationFn: (request: CreateDownload) =>
      api<DownloadJob>("/api/v1/downloads", {
        method: "POST",
        headers: { "Idempotency-Key": crypto.randomUUID() },
        body: JSON.stringify(request)
      }),
    onSuccess: (job) => {
      submittedJob = job;
      monitorError = "";
      queryClient.setQueryData(["download-job", job.id], job);
      monitoredJobId.set(job.id);
    }
  });

  function statusMessage(state: DownloadJob["state"]): string {
    switch (state) {
      case "queued": return "Queued for processing…";
      case "fetching_metadata": return "Checking release metadata…";
      case "submitting": return "Submitting to qBittorrent…";
      default: return "Waiting for qBittorrent…";
    }
  }

  $effect(() => {
    const job = $jobStatus.data;
    if (job) submittedJob = job;
    if ($jobStatus.isError) monitorError = `Could not confirm the download status: ${$jobStatus.error.message}`;
    if (job && ["active", "complete"].includes(job.state) && completedJobId !== job.id) {
      completedJobId = job.id;
      finish();
    } else if (job && !["queued", "fetching_metadata", "submitting", "active", "complete", "failed"].includes(job.state)) {
      monitorError = "Wotbox returned an unknown download state. The download may not have been submitted.";
    }
  });

  function submit() {
    if (!selection || !selectedProfile || processing) return;
    submittedJob = null;
    monitoredJobId.set("");
    monitorError = "";
    $addDownload.mutate({
      tracker: activeTracker,
      torrentId: selection.torrent.torrentId,
      profile: selectedProfile,
      useToken
    });
  }

  function finish() {
    submittedJob = null;
    monitoredJobId.set("");
    monitorError = "";
    initializedTorrent = "";
    oncomplete?.();
    onclose();
  }

  function close() {
    if (processing) return;
    submittedJob = null;
    monitoredJobId.set("");
    monitorError = "";
    initializedTorrent = "";
    onclose();
  }
</script>

<Dialog.Root open={selection !== null} onOpenChange={(open) => { if (!open) close(); }}>
  <Dialog.Portal>
    <Dialog.Overlay class="dialog-overlay" />
    <Dialog.Content class="dialog-content">
      <Dialog.Title class="dialog-title">Add to qBittorrent</Dialog.Title>
      <Dialog.Description class="dialog-description">
        Confirm the release and download profile. Gazelle validates metadata and token use before submission.
      </Dialog.Description>
      {#if selection}
        <div class="dialog-release">
          <div class="release-mark">{selection.name.slice(0, 1)}</div>
          <div>
            <strong>{selection.name}</strong>
            <span>{selection.artist ?? "Various artists"} · {selection.torrent.format ?? "Unknown"} {selection.torrent.encoding ?? ""}</span>
          </div>
        </div>
        <label class="dialog-field">
          <span>Download profile</span>
          <select bind:value={selectedProfile}>
            {#each $profiles.data ?? [] as profile}
              <option value={profile.name}>{profile.name} · {profile.savePath}</option>
            {/each}
          </select>
        </label>
        {@const eligibilityKnown = selection.torrent.tokenEligibilityKnown ?? true}
        {@const tokenDisabled = selection.torrent.freeleech
          || (eligibilityKnown && !selection.torrent.canUseToken)
          || tokenCost === undefined
          || policy.mode === "disabled"
          || policy.mode === "freeleech_only"}
        <label class:disabled={tokenDisabled} class="token-toggle">
          <input type="checkbox" bind:checked={useToken} disabled={tokenDisabled} />
          <span class="toggle-box">{#if useToken}<Check size={15} />{/if}</span>
          <span>
            <strong>Use a freeleech token</strong>
            <small>
              {#if selection.torrent.freeleech}
                This torrent is already free on {activeTracker.toUpperCase()}.
              {:else if tokenCost === undefined}
                The token cost cannot be calculated because the torrent size or tracker cost model is unavailable.
              {:else if !eligibilityKnown}
                This action consumes {tokenCost} {activeTracker.toUpperCase()} {tokenCost === 1 ? "token" : "tokens"}; availability will be verified before qBittorrent is contacted.
              {:else if selection.torrent.canUseToken}
                This action consumes {tokenCost} {activeTracker.toUpperCase()} freeleech {tokenCost === 1 ? "token" : "tokens"}.
              {:else}
                The tracker reports that this torrent is not token eligible.
              {/if}
            </small>
          </span>
        </label>
        {#if policyBlocked}
          <div class="error-panel compact submission-error" role="status">
            <span>
              <strong>Blocked by tracker policy</strong>
              <small>
                {policy.mode === "disabled"
                  ? `${activeTracker.toUpperCase()} downloads are disabled.`
                  : policy.mode === "freeleech_only"
                    ? `${activeTracker.toUpperCase()} is configured for already-free torrents only.`
                    : selection.torrent.eligibility?.reason === "token_cost_unknown"
                      ? "The OPS token cost cannot be calculated because the torrent size is unavailable."
                    : "A required freeleech token is not available for this torrent."}
              </small>
            </span>
          </div>
        {:else if requiresToken && !useToken}
          <div class="stale-notice compact" role="status">
            This tracker policy requires a freeleech token. Enable token use to add the torrent.
          </div>
        {/if}
        {#if processing && submittedJob}
          <div class="submission-status" role="status" aria-live="polite">
            <span class="submission-spinner" aria-hidden="true"></span>
            <span>
              <strong>Adding download</strong>
              <small>{statusMessage(submittedJob.state)}</small>
            </span>
          </div>
        {/if}
        {#if submittedJob?.state === "failed"}
          <div class="error-panel compact submission-error" role="alert">
            <span>
              <strong>Download failed</strong>
              <small>{submittedJob.errorMessage ?? "Wotbox could not submit this torrent to qBittorrent."}</small>
            </span>
          </div>
        {:else if monitorError}
          <div class="error-panel compact submission-error" role="alert">
            <span>
              <strong>Download status unavailable</strong>
              <small>{monitorError}</small>
            </span>
          </div>
        {:else if $addDownload.isError}
          <div class="error-panel compact submission-error" role="alert">
            <span>
              <strong>Could not start download</strong>
              <small>{$addDownload.error.message}</small>
            </span>
          </div>
        {/if}
        <div class="dialog-actions">
          <button class="secondary-button" disabled={processing} onclick={close}>
            {submittedJob?.state === "failed" || monitorError || $addDownload.isError ? "Close" : "Cancel"}
          </button>
          <button
            class="primary-button"
            disabled={$addDownload.isPending || processing || !selectedProfile || policyBlocked || (requiresToken && !useToken)}
            onclick={submit}
          >
            {#if $addDownload.isPending || processing}
              Adding…
            {:else if submittedJob?.state === "failed"}
              Retry download
            {:else}
              Add download
            {/if}
          </button>
        </div>
      {/if}
    </Dialog.Content>
  </Dialog.Portal>
</Dialog.Root>
