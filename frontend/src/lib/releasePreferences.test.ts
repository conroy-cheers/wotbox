import { describe, expect, it } from "vitest";
import { defaultReleasePreferences, isQualityAllowed, rankVariants } from "./releasePreferences";

const variant = (
  torrentId: number,
  format: string,
  encoding: string,
  media: string,
  seeders: number
) => ({
  torrentId,
  format,
  encoding,
  media,
  seeders,
  freeleech: false,
  canUseToken: false,
  downloads: []
});

describe("release preferences", () => {
  it("ranks quality before media and popularity", () => {
    const values = [
      variant(1, "FLAC", "Lossless", "WEB", 100),
      variant(2, "FLAC", "24bit Lossless", "Vinyl", 1),
      variant(3, "FLAC", "Lossless", "CD", 200)
    ];
    expect(rankVariants(values, defaultReleasePreferences).map((item) => item.torrentId))
      .toEqual([2, 3, 1]);
  });

  it("treats tied media by popularity", () => {
    const values = [
      variant(1, "FLAC", "Lossless", "WEB", 10),
      variant(2, "FLAC", "Lossless", "CD", 20)
    ];
    expect(rankVariants(values, defaultReleasePreferences)[0].torrentId).toBe(2);
  });

  it("enforces the default lossless cutoff", () => {
    expect(isQualityAllowed(variant(1, "FLAC", "Lossless", "WEB", 1), defaultReleasePreferences)).toBe(true);
    expect(isQualityAllowed(variant(2, "MP3", "320", "WEB", 1), defaultReleasePreferences)).toBe(false);
  });
});
