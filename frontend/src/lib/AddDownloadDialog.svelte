<script lang="ts">
  import { createMutation, createQuery, useQueryClient } from "@tanstack/svelte-query";
  import { Dialog } from "bits-ui";
  import { Check } from "@lucide/svelte";
  import {
    api,
    type CreateDownload,
    type DownloadJob,
    type DownloadProfile,
    type DownloadSelection
  } from "./api";

  let {
    selection,
    tracker,
    onclose
  }: {
    selection: DownloadSelection | null;
    tracker: string;
    onclose: () => void;
  } = $props();

  let useToken = $state(false);
  let selectedProfile = $state("");
  let initializedTorrent = $state<number | null>(null);
  let submittedJob = $state<DownloadJob | null>(null);
  let monitorError = $state("");
  let monitorGeneration = 0;
  const queryClient = useQueryClient();
  const processing = $derived(
    submittedJob !== null
      && ["queued", "fetching_metadata", "submitting"].includes(submittedJob.state)
  );

  const profiles = createQuery({
    queryKey: ["download-profiles"],
    queryFn: () => api<DownloadProfile[]>("/api/v1/download-profiles")
  });

  $effect(() => {
    if (!selection || initializedTorrent === selection.torrent.torrentId) return;
    initializedTorrent = selection.torrent.torrentId;
    const eligibilityKnown = selection.torrent.tokenEligibilityKnown ?? true;
    useToken = !selection.torrent.freeleech
      && (selection.torrent.canUseToken || !eligibilityKnown);
    selectedProfile = $profiles.data?.[0]?.name ?? "";
  });

  $effect(() => {
    if (selection && !selectedProfile && $profiles.data?.length) {
      selectedProfile = $profiles.data[0].name;
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
      void monitorDownload(job);
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

  async function monitorDownload(initial: DownloadJob) {
    const generation = ++monitorGeneration;
    let job = initial;
    try {
      while (["queued", "fetching_metadata", "submitting"].includes(job.state)) {
        await new Promise((resolve) => window.setTimeout(resolve, 500));
        if (generation !== monitorGeneration) return;
        job = await api<DownloadJob>(`/api/v1/download-jobs/${job.id}`);
        if (generation !== monitorGeneration) return;
        submittedJob = job;
      }
      if (job.state === "active" || job.state === "complete") {
        await Promise.all([
          queryClient.invalidateQueries({ queryKey: ["downloads"] }),
          queryClient.invalidateQueries({ queryKey: ["library-artist"] }),
          queryClient.invalidateQueries({ queryKey: ["artist-catalog"] }),
          queryClient.invalidateQueries({ queryKey: ["search"] })
        ]);
        if (generation === monitorGeneration) finish();
      } else if (job.state !== "failed") {
        monitorError = "Wotbox returned an unknown download state. The download may not have been submitted.";
      }
    } catch (error) {
      if (generation === monitorGeneration) {
        monitorError = error instanceof Error
          ? `Could not confirm the download status: ${error.message}`
          : "Could not confirm the download status.";
      }
    }
  }

  function submit() {
    if (!selection || !selectedProfile || processing) return;
    submittedJob = null;
    monitorError = "";
    $addDownload.mutate({
      tracker,
      torrentId: selection.torrent.torrentId,
      profile: selectedProfile,
      useToken
    });
  }

  function finish() {
    monitorGeneration++;
    submittedJob = null;
    monitorError = "";
    initializedTorrent = null;
    onclose();
  }

  function close() {
    if (processing) return;
    monitorGeneration++;
    submittedJob = null;
    monitorError = "";
    initializedTorrent = null;
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
        {@const tokenDisabled = selection.torrent.freeleech || (eligibilityKnown && !selection.torrent.canUseToken)}
        <label class:disabled={tokenDisabled} class="token-toggle">
          <input type="checkbox" bind:checked={useToken} disabled={tokenDisabled} />
          <span class="toggle-box">{#if useToken}<Check size={15} />{/if}</span>
          <span>
            <strong>Use a freeleech token</strong>
            <small>
              {#if selection.torrent.freeleech}
                This torrent is already freeleech.
              {:else if !eligibilityKnown}
                OPS will verify token availability before qBittorrent is contacted.
              {:else if selection.torrent.canUseToken}
                This action consumes the required tracker token.
              {:else}
                The tracker reports that this torrent is not token eligible.
              {/if}
            </small>
          </span>
        </label>
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
            disabled={$addDownload.isPending || processing || !selectedProfile}
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
