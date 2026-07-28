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
  const queryClient = useQueryClient();

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
    onSuccess: async () => {
      initializedTorrent = null;
      onclose();
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ["downloads"] }),
        queryClient.invalidateQueries({ queryKey: ["library-artist"] }),
        queryClient.invalidateQueries({ queryKey: ["artist-catalog"] }),
        queryClient.invalidateQueries({ queryKey: ["search"] })
      ]);
    }
  });

  function close() {
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
        {#if $addDownload.isError}<div class="error-panel compact">{$addDownload.error.message}</div>{/if}
        <div class="dialog-actions">
          <button class="secondary-button" onclick={close}>Cancel</button>
          <button
            class="primary-button"
            disabled={$addDownload.isPending || !selectedProfile}
            onclick={() => $addDownload.mutate({
              tracker,
              torrentId: selection!.torrent.torrentId,
              profile: selectedProfile,
              useToken
            })}
          >
            {$addDownload.isPending ? "Adding…" : "Add download"}
          </button>
        </div>
      {/if}
    </Dialog.Content>
  </Dialog.Portal>
</Dialog.Root>
