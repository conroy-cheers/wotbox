import { describe, expect, it } from "vitest";
import { trackerGroupUrl, uniqueReleaseSources } from "./trackerLinks";

describe("tracker release links", () => {
  it("links known historical trackers even when they are not configured", () => {
    expect(trackerGroupUrl({ tracker: "red", groupId: 42 }))
      .toBe("https://redacted.sh/torrents.php?id=42");
  });

  it("uses configured tracker origins", () => {
    expect(trackerGroupUrl(
      { tracker: "ops", groupId: 7 },
      { ops: "https://music.example" }
    )).toBe("https://music.example/torrents.php?id=7");
  });

  it("deduplicates the same source", () => {
    expect(uniqueReleaseSources([
      { tracker: "ops", groupId: 7, matchScore: 1 },
      { tracker: "OPS", groupId: 7, matchScore: 0.9 }
    ])).toHaveLength(1);
  });
});
