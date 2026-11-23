import { get } from "svelte/store";
import { beforeEach, describe, expect, it } from "vitest";
import type { LiveDownloadStatus, TorrentVariant } from "./api";
import { tagSnapshot } from "./snapshot";
import { applyResourceChanges, liveDownloads, transferDownloads, variantDownloads } from "./liveState";

function download(progress: number): LiveDownloadStatus {
  return {
    client: "music",
    infoHash: "ABC123",
    state: progress >= 1 ? "seeding" : "downloading",
    clientState: progress >= 1 ? "uploading" : "downloading",
    progress,
    size: 100,
    downloaded: progress * 100,
    uploaded: 0,
    downloadSpeed: 10,
    uploadSpeed: 0,
    ratio: 0,
    savePath: "/music"
  };
}

const variant: TorrentVariant = {
  tracker: "ops",
  torrentId: 1,
  groupId: 2,
  infoHash: "abc123",
  freeleech: true,
  leechStatus: "regular",
  canUseToken: true,
  tokenEligibilityKnown: true,
  downloads: [download(0.1)]
};

describe("normalized live download state", () => {
  beforeEach(() => liveDownloads.set(new Map()));

  it("overlays progress without replacing catalog query data", () => {
    applyResourceChanges([{
      id: 1,
      resources: ["downloads"],
      reason: "download_client_sync",
      payload: { downloads: [download(0.75)], removed: [] }
    }]);
    expect(variantDownloads(variant, get(liveDownloads))[0].progress).toBe(0.75);
    expect(variant.downloads[0].progress).toBe(0.1);
  });

  it("uses tombstones to suppress removed embedded downloads", () => {
    applyResourceChanges([{
      id: 2,
      resources: ["downloads"],
      reason: "download_client_sync",
      payload: { downloads: [], removed: [{ client: "music", infoHash: "abc123" }] }
    }]);
    expect(variantDownloads(variant, get(liveDownloads))).toEqual([]);
    expect(transferDownloads([{ download: variant.downloads[0] }], get(liveDownloads))).toEqual([]);
  });
  it("clears tombstones for the client whose full snapshot replaced them", () => {
    applyResourceChanges([{
      id: 2,
      resources: ["downloads"],
      reason: "download_client_sync",
      payload: { removed: [{ client: "music", infoHash: "abc123" }] }
    }, {
      id: 3,
      resources: ["downloads"],
      reason: "download_client_sync",
      payload: { resetClient: "music" }
    }]);
    expect(variantDownloads(variant, get(liveDownloads))).toEqual(variant.downloads);
  });

  it("ignores older tombstones with a fresh snapshot and overlays a late response", () => {
    const fresh = structuredClone(variant);
    tagSnapshot(fresh, { epoch: "instance", sequence: 9 });
    applyResourceChanges([{
      id: 8, resources: ["downloads"], reason: "download_client_sync",
      payload: { removed: [{ client: "music", infoHash: "abc123" }] }
    }], "instance");
    expect(variantDownloads(fresh, get(liveDownloads))).toEqual(fresh.downloads);
    const late = structuredClone(variant);
    tagSnapshot(late, { epoch: "instance", sequence: 7 });
    expect(variantDownloads(late, get(liveDownloads))).toEqual([]);
    expect(transferDownloads([{ download: late.downloads[0] }], get(liveDownloads))).toEqual([]);
    const freshRow = { download: fresh.downloads[0] };
    expect(transferDownloads([freshRow], get(liveDownloads))).toEqual([{ item: freshRow, download: freshRow.download }]);
  });

});
