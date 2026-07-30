import { describe, expect, it } from "vitest";
import type { ChannelPackItem } from "./api";
import { executableOrdinals, summarizeSelection } from "./channelPack";

function item(
  ordinal: number,
  planState: ChannelPackItem["planState"],
  tracker = "ops",
  size = 10,
  tokenCost = 0
): ChannelPackItem {
  return {
    ordinal,
    source: { id: `source:${ordinal}`, rank: ordinal, artist: "Artist", title: `Album ${ordinal}` },
    matchState: "matched",
    variants: [],
    planState,
    plan: planState === "executable"
      ? {
          tracker,
          torrentId: ordinal,
          profile: tracker,
          useToken: tokenCost > 0,
          tokenCost,
          size
        }
      : undefined
  };
}

describe("channel pack selection", () => {
  it("selects only executable items by default", () => {
    expect([...executableOrdinals([
      item(1, "executable"),
      item(2, "capacity_blocked"),
      item(3, "executable")
    ])]).toEqual([1, 3]);
  });

  it("summarizes only the approved subset", () => {
    const summary = summarizeSelection(
      [
        item(1, "executable", "ops", 40, 2),
        item(2, "executable", "red", 60),
        item(3, "duplicate")
      ],
      new Set([2])
    );
    expect(summary).toMatchObject({
      executable: 1,
      skipped: 2,
      totalSize: 60,
      tokenUses: 0,
      byTracker: { red: 1 }
    });
  });

  it("sums actual token units for selected releases", () => {
    const summary = summarizeSelection(
      [
        item(1, "executable", "ops", 40, 2),
        item(2, "executable", "red", 60, 1)
      ],
      new Set([1, 2])
    );
    expect(summary.tokenUses).toBe(3);
  });
});
