import type { Query } from "@tanstack/svelte-query";

export type LiveScope =
  | "activity"
  | "assets"
  | "catalog"
  | "channels"
  | "operations"
  | "providers"
  | "settings"
  | "global";

const staticQueries = new Set(["config", "download-profiles"]);
const queryScopes: Record<string, LiveScope[]> = {
  accounts: ["providers"],
  downloads: ["activity"],
  "download-job": ["activity", "operations"],
  imports: ["activity"],
  library: ["catalog"],
  "library-artist": ["catalog"],
  release: ["activity", "catalog"],
  search: ["activity", "catalog", "settings"],
  "artist-catalog": ["activity", "catalog"],
  "canonical-index": ["catalog", "operations"],
  "cross-seed-plans": ["activity", "catalog"],
  "match-candidates": ["catalog"],
  channels: ["channels", "settings"],
  "channels-overview": ["channels"],
  "channel-pack": ["activity", "channels", "settings"],
  "background-jobs": ["operations"],
  "plex-integration": ["operations"],
  providers: ["providers"],
  preferences: ["settings"]
};

const queryResources: Record<string, string[]> = {
  accounts: ["providers"],
  library: ["library"],
  "local-search": ["library"],
  "library-artist": ["library"],
  release: ["catalog"],
  search: ["catalog"],
  "artist-catalog": ["catalog"],
  "canonical-index": ["catalog"],
  "cross-seed-plans": ["catalog", "download-inventory"],
  "match-candidates": ["catalog", "download-inventory"],
  downloads: ["download-inventory"],
  imports: ["imports", "download-inventory"],
  "download-job": ["download-jobs"],
  channels: ["channels", "preferences"],
  "channels-overview": ["channels", "preferences"],
  "channel-pack": ["channels", "download-inventory"],
  "background-jobs": ["background-jobs"],
  providers: ["providers"],
  preferences: ["preferences"],
  "plex-integration": ["plex"]
};

export function isLiveQuery(query: Query): boolean {
  const root = String(query.queryKey[0] ?? "");
  return !staticQueries.has(root);
}

export function queryUsesScopes(query: Query, changed: Set<string>): boolean {
  if (!isLiveQuery(query)) return false;
  if (changed.has("global")) return true;
  const root = String(query.queryKey[0] ?? "");
  const scopes = queryScopes[root];
  // A query without a declared dependency is immutable from the live-update
  // system's point of view. This keeps newly added pages from accidentally
  // subscribing to every event; their owner must declare the actual resource.
  return scopes ? scopes.some((scope) => changed.has(scope)) : false;
}

export function queryUsesResources(query: Query, changed: Set<string>): boolean {
  const root = String(query.queryKey[0] ?? "");
  return (queryResources[root] ?? []).some((resource) => changed.has(resource));
}
