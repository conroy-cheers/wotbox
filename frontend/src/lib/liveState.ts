import { writable } from "svelte/store";
import type { LiveDownloadStatus, ReleaseDownload, TorrentVariant } from "./api";

type DownloadEntry = LiveDownloadStatus | null;

export type ResourceChange = {
  id: number;
  resources: string[];
  reason: string;
  payload?: unknown;
};

export const liveDownloads = writable<Map<string, DownloadEntry>>(new Map());

function downloadKey(client: string, infoHash: string): string {
  return `${client.toLowerCase()}:${infoHash.toLowerCase()}`;
}

export function applyResourceChanges(changes: ResourceChange[]): void {
  for (const change of changes) {
    if (!change.resources.includes("downloads") || !change.payload) continue;
    const payload = change.payload as {
      downloads?: LiveDownloadStatus[];
      removed?: { client: string; infoHash: string }[];
    };
    liveDownloads.update((known) => {
      const next = new Map(known);
      for (const download of payload.downloads ?? []) {
        next.set(downloadKey(download.client, download.infoHash), download);
      }
      for (const removed of payload.removed ?? []) {
        next.set(downloadKey(removed.client, removed.infoHash), null);
      }
      return next;
    });
  }
}

export function variantDownloads(
  variant: Pick<TorrentVariant, "infoHash" | "downloads">,
  live: Map<string, DownloadEntry>
): LiveDownloadStatus[] {
  const existing = variant.downloads.filter((download) => {
    return live.get(downloadKey(download.client, download.infoHash)) !== null;
  });
  const byKey = new Map(existing.map((download) => [
    downloadKey(download.client, download.infoHash),
    live.get(downloadKey(download.client, download.infoHash)) ?? download
  ]));
  if (variant.infoHash) {
    for (const [key, download] of live) {
      if (download?.infoHash.toLowerCase() === variant.infoHash.toLowerCase()) {
        byKey.set(key, download);
      }
    }
  }
  return [...byKey.values()].filter((download): download is LiveDownloadStatus => download != null);
}

export function currentDownload(
  download: LiveDownloadStatus,
  live: Map<string, DownloadEntry>
): LiveDownloadStatus {
  return live.get(downloadKey(download.client, download.infoHash)) ?? download;
}

export function releaseDownloads(
  downloads: ReleaseDownload[],
  live: Map<string, DownloadEntry>
): ReleaseDownload[] {
  return downloads
    .filter((download) => live.get(downloadKey(download.live.client, download.live.infoHash)) !== null)
    .map((download) => ({
      ...download,
      live: live.get(downloadKey(download.live.client, download.live.infoHash)) ?? download.live
    }));
}
