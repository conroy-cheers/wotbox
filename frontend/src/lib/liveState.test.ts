import { get } from "svelte/store";
import { beforeEach, describe, expect, it } from "vitest";
import type { LiveDownloadStatus, TorrentVariant } from "./api";
import { applyResourceChanges, liveDownloads, variantDownloads } from "./liveState";

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
  });
});
