import { snapshotRevision, type SnapshotRevision } from "./snapshot";
import { writable } from "svelte/store";
import type { LiveDownloadStatus, ReleaseDownload, TorrentVariant } from "./api";

type DownloadEntry = {
  status: LiveDownloadStatus | null;
  sequence: number;
  epoch?: string;
};

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

export function applyResourceChanges(changes: ResourceChange[], epoch?: string): boolean {
  let overflow = false;
  const updates = changes.filter((change) => change.resources.includes("downloads") && change.payload);
  if (updates.length === 0) return overflow;
  liveDownloads.update((known) => {
    const next = new Map(known);
    for (const change of updates) {
      const payload = change.payload as {
        resetClient?: string;
        downloads?: LiveDownloadStatus[];
        removed?: { client: string; infoHash: string }[];
      };
      if (payload.resetClient) {
        const prefix = payload.resetClient.toLowerCase() + ":";
        for (const [key, entry] of next) {
          if (key.startsWith(prefix) && (entry.epoch !== epoch || entry.sequence <= change.id)) next.delete(key);
        }
      }
      const set = (key: string, status: LiveDownloadStatus | null) => {
        const existing = next.get(key);
        if (existing && existing.epoch === epoch && existing.sequence >= change.id) return;
        next.set(key, { status, epoch, sequence: change.id });
      };
      for (const download of payload.downloads ?? []) set(downloadKey(download.client, download.infoHash), download);
      for (const removed of payload.removed ?? []) set(downloadKey(removed.client, removed.infoHash), null);
    }
    if (next.size > 10_000) {
      overflow = true;
      return new Map();
    }
    return next;
  });
  return overflow;
}

function isNewer(entry: DownloadEntry, revision: SnapshotRevision | undefined): boolean {
  if (!revision || !entry.epoch) return true;
  return entry.epoch === revision.epoch && entry.sequence > revision.sequence;
}

function overlay(download: LiveDownloadStatus, live: Map<string, DownloadEntry>): LiveDownloadStatus | null {
  const entry = live.get(downloadKey(download.client, download.infoHash));
  return entry && isNewer(entry, snapshotRevision(download)) ? entry.status : download;
}

const downloadsByHash = new WeakMap<Map<string, DownloadEntry>, Map<string, DownloadEntry[]>>();

function matchingLiveDownloads(live: Map<string, DownloadEntry>, hash: string): DownloadEntry[] {
  let index = downloadsByHash.get(live);
  if (index == null) {
    index = new Map();
    for (const entry of live.values()) {
      if (!entry.status) continue;
      const key = entry.status.infoHash.toLowerCase();
      const entries = index.get(key) ?? [];
      entries.push(entry);
      index.set(key, entries);
    }
    downloadsByHash.set(live, index);
  }
  return index.get(hash.toLowerCase()) ?? [];
}

export function variantDownloads(
  variant: Pick<TorrentVariant, "infoHash" | "downloads">,
  live: Map<string, DownloadEntry>
): LiveDownloadStatus[] {
  const downloads = new Map<string, LiveDownloadStatus>();
  const snapshotKeys = new Set<string>();
  for (const download of variant.downloads) {
    const key = downloadKey(download.client, download.infoHash);
    snapshotKeys.add(key);
    const current = overlay(download, live);
    if (current) downloads.set(key, current);
  }
  if (variant.infoHash) {
    const revision = snapshotRevision(variant.downloads) ?? snapshotRevision(variant);
    for (const entry of matchingLiveDownloads(live, variant.infoHash)) {
      const download = entry.status!;
      const key = downloadKey(download.client, download.infoHash);
      if (!snapshotKeys.has(key) && isNewer(entry, revision)) downloads.set(key, download);
    }
  }
  return [...downloads.values()];
}

export function transferDownloads<T extends { download: LiveDownloadStatus }>(
  items: T[],
  live: Map<string, DownloadEntry>
): { item: T; download: LiveDownloadStatus }[] {
  return items.flatMap((item) => {
    const download = overlay(item.download, live);
    return download ? [{ item, download }] : [];
  });
}

export function releaseDownloads(
  downloads: ReleaseDownload[],
  live: Map<string, DownloadEntry>
): ReleaseDownload[] {
  return downloads.flatMap((download) => {
    const status = overlay(download.live, live);
    return status ? [{ ...download, live: status }] : [];
  });
}
