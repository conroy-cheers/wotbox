import type {
  QualityPreference,
  ReleasePreferences,
  SearchTorrent,
  TorrentVariant
} from "./api";

export type DisplayVariant = SearchTorrent | TorrentVariant;

export const defaultReleasePreferences: ReleasePreferences = {
  qualityTiers: [["hi_res"], ["lossless"], ["320"], ["v0"], ["other"]],
  qualityCutoffIndex: 2,
  mediaTiers: [["WEB", "CD"], ["SACD", "DVD", "Blu-ray"], ["Vinyl"], ["Cassette"], ["Other"]],
  mediaCutoffIndex: 2,
  variantSortOrder: ["quality", "tracker", "media", "edition"],
  trackerOrder: ["ops", "red"],
  trackerPolicies: [
    {
      tracker: "ops",
      mode: "freeleech_or_token",
      autoUseTokens: true,
      downloadProfile: "ops",
      autoTokenLimit: 1
    },
    {
      tracker: "red",
      mode: "freeleech_only",
      autoUseTokens: false,
      downloadProfile: "red",
      autoTokenLimit: 0
    }
  ]
};

export const qualityLabels: Record<QualityPreference, string> = {
  hi_res: "Hi-res lossless",
  lossless: "Lossless",
  "320": "320 kbps",
  v0: "V0",
  other: "Other"
};

export function qualityClass(variant: Pick<DisplayVariant, "format" | "encoding">): QualityPreference {
  const format = (variant.format ?? "").toLowerCase();
  const encoding = (variant.encoding ?? "").toLowerCase();
  if (encoding.includes("24bit") || encoding.includes("24-bit") || encoding.includes("24 bit")) {
    return "hi_res";
  }
  if (encoding.includes("lossless") || format.includes("flac")) return "lossless";
  if (encoding.includes("320")) return "320";
  if (encoding.includes("v0")) return "v0";
  return "other";
}

export function isQualityAllowed(variant: DisplayVariant, preferences: ReleasePreferences): boolean {
  const rank = preferences.qualityTiers.findIndex((tier) => tier.includes(qualityClass(variant)));
  return rank >= 0 && rank < preferences.qualityCutoffIndex;
}

export function mediaRank(media: string | undefined, preferences: ReleasePreferences): number {
  const normalized = (media ?? "other").trim().toLowerCase();
  const rank = preferences.mediaTiers.findIndex((tier) =>
    tier.some((candidate) => candidate.trim().toLowerCase() === normalized)
  );
  const otherRank = preferences.mediaTiers.findIndex((tier) =>
    tier.some((candidate) => candidate.trim().toLowerCase() === "other")
  );
  return rank >= 0 ? rank : otherRank >= 0 ? otherRank : preferences.mediaTiers.length;
}

export function isMediaAllowed(variant: DisplayVariant, preferences: ReleasePreferences): boolean {
  return mediaRank(variant.media, preferences) < preferences.mediaCutoffIndex;
}

function qualityRank(variant: DisplayVariant, preferences: ReleasePreferences): number {
  const rank = preferences.qualityTiers.findIndex((tier) => tier.includes(qualityClass(variant)));
  return rank >= 0 ? rank : preferences.qualityTiers.length;
}

function editionRank(title: string | undefined): number {
  const normalized = (title ?? "").toLowerCase();
  if (["super deluxe", "deluxe", "expanded", "extended", "anniversary", "bonus track"]
    .some((label) => normalized.includes(label))) return 0;
  if (["instrumental", "remix", "live", "karaoke"]
    .some((label) => normalized.includes(label))) return 3;
  return 1;
}

export function rankVariants<T extends DisplayVariant>(
  variants: T[],
  preferences: ReleasePreferences
): T[] {
  return [...variants].sort((left, right) => {
    for (const criterion of preferences.variantSortOrder) {
      const difference = criterion === "quality"
        ? qualityRank(left, preferences) - qualityRank(right, preferences)
        : criterion === "tracker"
          ? trackerRank(left.tracker, preferences) - trackerRank(right.tracker, preferences)
          : criterion === "media"
            ? mediaRank(left.media, preferences) - mediaRank(right.media, preferences)
            : editionRank(left.remasterTitle) - editionRank(right.remasterTitle);
      if (difference) return difference;
    }
    return (right.seeders ?? 0) - (left.seeders ?? 0)
      || left.torrentId - right.torrentId;
  });
}

function trackerRank(tracker: string | undefined, preferences: ReleasePreferences): number {
  if (!tracker) return preferences.trackerOrder.length;
  const rank = preferences.trackerOrder.findIndex((known) =>
    known.toLowerCase() === tracker.toLowerCase()
  );
  return rank >= 0 ? rank : preferences.trackerOrder.length;
}
