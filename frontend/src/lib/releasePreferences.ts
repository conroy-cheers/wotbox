import type {
  QualityPreference,
  ReleasePreferences,
  SearchTorrent,
  TorrentVariant
} from "./api";

export type DisplayVariant = SearchTorrent | TorrentVariant;

export const defaultReleasePreferences: ReleasePreferences = {
  qualityOrder: ["hi_res", "lossless", "320", "v0", "other"],
  minimumQuality: "lossless",
  mediaTiers: [["WEB", "CD"], ["Vinyl"], ["SACD", "DVD", "Blu-ray"], ["Cassette"], ["Other"]]
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
  const rank = preferences.qualityOrder.indexOf(qualityClass(variant));
  const cutoff = preferences.qualityOrder.indexOf(preferences.minimumQuality);
  return rank >= 0 && cutoff >= 0 && rank <= cutoff;
}

function mediaRank(media: string | undefined, preferences: ReleasePreferences): number {
  const normalized = (media ?? "other").trim().toLowerCase();
  const rank = preferences.mediaTiers.findIndex((tier) =>
    tier.some((candidate) => candidate.trim().toLowerCase() === normalized)
  );
  const otherRank = preferences.mediaTiers.findIndex((tier) =>
    tier.some((candidate) => candidate.trim().toLowerCase() === "other")
  );
  return rank >= 0 ? rank : otherRank >= 0 ? otherRank : preferences.mediaTiers.length;
}

export function rankVariants<T extends DisplayVariant>(
  variants: T[],
  preferences: ReleasePreferences
): T[] {
  return [...variants].sort((left, right) =>
    preferences.qualityOrder.indexOf(qualityClass(left))
      - preferences.qualityOrder.indexOf(qualityClass(right))
    || mediaRank(left.media, preferences) - mediaRank(right.media, preferences)
    || (right.seeders ?? 0) - (left.seeders ?? 0)
    || left.torrentId - right.torrentId
  );
}
