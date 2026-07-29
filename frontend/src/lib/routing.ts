import page from "page";
import { appPath } from "./api";

export type ViewQueryValue =
  | string
  | number
  | boolean
  | null
  | undefined
  | readonly (string | number)[];

export type ViewQuery = Record<string, ViewQueryValue>;

export type ReleaseSource = "search" | "library" | "downloads" | "channels";

export type ReleaseAttachment = {
  client: string;
  infoHash: string;
};

export function viewPath(path: string, query: ViewQuery = {}): string {
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(query)) {
    if (value == null || value === "" || value === false) continue;
    if (Array.isArray(value)) {
      for (const item of value) params.append(key, String(item));
    } else {
      params.set(key, value === true ? "1" : String(value));
    }
  }
  const encoded = params.toString();
  return encoded ? `${path}?${encoded}` : path;
}

export function navigateView(path: string, query: ViewQuery = {}): void {
  page(viewPath(path, query));
}

export function replaceView(path: string, query: ViewQuery = {}): void {
  page.replace(viewPath(path, query), undefined, false, false);
}

export function closeOverlay(fallbackPath: string, query: ViewQuery = {}): void {
  const router = page as typeof page & {
    back: (fallback: string) => void;
  };
  router.back(viewPath(fallbackPath, query));
}

export function browserViewPath(path: string, query: ViewQuery = {}): string {
  return appPath(viewPath(path, query));
}

export function releaseViewPath(
  releaseId: string | undefined,
  torrentId?: number,
  source: ReleaseSource = "search",
  attachment?: ReleaseAttachment,
  expanded = false,
  showClientDetails = false
): string {
  return browserViewPath(
    `/releases/${encodeURIComponent(releaseId ?? "unresolved")}`,
    {
      torrent: torrentId,
      client: attachment?.client,
      hash: attachment?.infoHash,
      from: source,
      expanded: expanded ? 1 : undefined,
      details: showClientDetails ? "client" : undefined
    }
  );
}

export function selectReleaseAttachment<T extends ReleaseAttachment>(
  downloads: readonly T[],
  client?: string,
  infoHash?: string
): T | undefined {
  if (client != null || infoHash != null) {
    if (!client || !infoHash) return undefined;
    return downloads.find(
      (download) =>
        download.client === client
        && download.infoHash.toLowerCase() === infoHash.toLowerCase()
    );
  }
  return downloads[0];
}

export function positiveInteger(
  params: URLSearchParams,
  key: string,
  fallback: number
): number {
  const value = Number(params.get(key));
  return Number.isSafeInteger(value) && value > 0 ? value : fallback;
}

export function optionalPositiveInteger(
  params: URLSearchParams,
  key: string
): number | undefined {
  const value = Number(params.get(key));
  return Number.isSafeInteger(value) && value > 0 ? value : undefined;
}

export function oneOf<T extends string>(
  params: URLSearchParams,
  key: string,
  allowed: readonly T[],
  fallback: T
): T {
  const value = params.get(key) as T | null;
  return value != null && allowed.includes(value) ? value : fallback;
}

export function integerSet(params: URLSearchParams, key: string): Set<number> {
  return new Set(
    params
      .getAll(key)
      .map(Number)
      .filter((value) => Number.isSafeInteger(value) && value > 0)
  );
}
