import type { ProviderStatus, SourceProvenance } from "./api";
import { providerStatusSummary } from "./providerStatus";

function snapshotSuffix(source: SourceProvenance): string {
  return source.fetchedAt
    ? ` Showing the snapshot from ${new Date(source.fetchedAt).toLocaleString()}.`
    : " Your library releases remain available meanwhile.";
}

export function freshnessMessage(
  source: SourceProvenance,
  provider?: ProviderStatus
): string {
  if (provider && provider.state !== "available") {
    return `${providerStatusSummary(provider)}.${snapshotSuffix(source)}`;
  }
  if (source.errorCode === "artist_source_unresolved") {
    return `The tracker catalogue link for this artist is not known yet.${snapshotSuffix(source)}`;
  }
  if (source.refreshState === "failed") {
    return `${source.tracker.toUpperCase()} catalogue refresh failed.${snapshotSuffix(source)}`;
  }
  if (["pending", "running", "retrying"].includes(source.refreshState ?? "")) {
    return source.state === "missing"
      ? `Loading releases from ${source.tracker.toUpperCase()}.${snapshotSuffix(source)}`
      : `Refreshing ${source.tracker.toUpperCase()} catalogue data.${snapshotSuffix(source)}`;
  }
  return source.state === "missing"
    ? `${source.tracker.toUpperCase()} catalogue data has not been cached yet.${snapshotSuffix(source)}`
    : `${source.tracker.toUpperCase()} catalogue data is stale.${snapshotSuffix(source)}`;
}
