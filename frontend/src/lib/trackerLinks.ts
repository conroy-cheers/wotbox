import type { ReleaseSource } from "./api";

const knownTrackerSites: Record<string, string> = {
  ops: "https://orpheus.network",
  orpheus: "https://orpheus.network",
  red: "https://redacted.sh",
  redacted: "https://redacted.sh"
};

export function trackerGroupUrl(
  source: Pick<ReleaseSource, "tracker" | "groupId">,
  configuredSites: Record<string, string> = {}
): string | undefined {
  const key = source.tracker.trim().toLowerCase();
  const origin = configuredSites[key] ?? knownTrackerSites[key];
  if (!origin || source.groupId <= 0) return undefined;
  try {
    const url = new URL("torrents.php", `${origin.replace(/\/$/, "")}/`);
    url.searchParams.set("id", String(source.groupId));
    return url.toString();
  } catch {
    return undefined;
  }
}

export function uniqueReleaseSources(sources: ReleaseSource[]): ReleaseSource[] {
  const seen = new Set<string>();
  return sources.filter((source) => {
    const key = `${source.tracker.toLowerCase()}:${source.groupId}`;
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}
