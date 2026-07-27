export type Provenance = {
  tracker: string;
  fetchedAt: string;
  cacheAgeSeconds: number;
  stale: boolean;
};

export type Envelope<T> = {
  data: T;
  provenance: Provenance;
};

export type PublicConfig = {
  basePath: string;
  trackers: string[];
  downloadProfiles: string[];
};

export type Account = {
  id?: number;
  username: string;
  uploaded?: number;
  downloaded?: number;
  ratio?: number;
  requiredRatio?: number;
  userClass?: string;
  bonusPoints?: number;
  raw: unknown;
};

export type SearchTorrent = {
  torrentId: number;
  editionId?: number;
  format?: string;
  encoding?: string;
  media?: string;
  size?: number;
  seeders?: number;
  leechers?: number;
  snatched?: number;
  freeleech: boolean;
  canUseToken: boolean;
  remasterTitle?: string;
};

export type SearchGroup = {
  groupId: number;
  name: string;
  artist?: string;
  year?: number;
  releaseType?: string;
  image?: string;
  tags: string[];
  torrents: SearchTorrent[];
};

export type SearchPage = {
  currentPage: number;
  totalPages: number;
  totalResults?: number;
  groups: SearchGroup[];
};

export type DownloadProfile = {
  name: string;
  client: string;
  savePath: string;
  tag: string;
  startPaused: boolean;
};

export type ClientDownloadState =
  | "downloading"
  | "seeding"
  | "paused"
  | "queued"
  | "checking"
  | "stalled"
  | "complete"
  | "error"
  | "unknown";

export type ClientDownload = {
  client: string;
  infoHash: string;
  name: string;
  state: ClientDownloadState;
  clientState: string;
  progress: number;
  size: number;
  downloaded: number;
  uploaded: number;
  downloadSpeed: number;
  uploadSpeed: number;
  eta?: number;
  ratio: number;
  savePath: string;
  category: string;
  tags: string[];
  tracker?: string;
  addedAt?: string;
  completedAt?: string;
};

export type DownloadState =
  | "queued"
  | "fetching_metadata"
  | "submitting"
  | "active"
  | "complete"
  | "failed"
  | "unknown";

export type DownloadJob = {
  id: string;
  tracker: string;
  torrentId: number;
  groupId?: number;
  profile: string;
  useToken: boolean;
  infoHash?: string;
  name?: string;
  state: DownloadState;
  progress: number;
  downloadSpeed: number;
  uploadSpeed: number;
  eta?: number;
  errorCode?: string;
  errorMessage?: string;
  createdAt: string;
  updatedAt: string;
};

export type CreateDownload = {
  tracker: string;
  torrentId: number;
  profile: string;
  useToken: boolean;
};

const configuredBase = window.__WOTBOX_CONFIG__?.basePath ?? "";
export const basePath = configuredBase === "/" ? "" : configuredBase;

export function appPath(path = "/"): string {
  return `${basePath}${path}`;
}

export async function api<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(`${basePath}${path}`, {
    ...init,
    headers: {
      Accept: "application/json",
      ...(init?.body ? { "Content-Type": "application/json" } : {}),
      ...init?.headers
    }
  });
  const body = await response.json().catch(() => null);
  if (!response.ok) {
    const message = body?.error?.message ?? `Request failed with HTTP ${response.status}`;
    throw new Error(message);
  }
  return body as T;
}

export function formatBytes(value?: number): string {
  if (value == null) return "—";
  const units = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
  let amount = value;
  let unit = 0;
  while (Math.abs(amount) >= 1024 && unit < units.length - 1) {
    amount /= 1024;
    unit++;
  }
  return `${amount.toLocaleString(undefined, {
    maximumFractionDigits: unit === 0 ? 0 : 2
  })} ${units[unit]}`;
}

export function formatSpeed(value: number): string {
  return value > 0 ? `${formatBytes(value)}/s` : "—";
}

export function relativeTime(value: string): string {
  const seconds = Math.round((Date.now() - new Date(value).getTime()) / 1000);
  if (seconds < 60) return "just now";
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ago`;
  return `${Math.floor(seconds / 86400)}d ago`;
}
