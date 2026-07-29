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

export type CanonicalBackfillProgress = {
  state: "pending" | "running" | "complete";
  processed: number;
  total: number;
  remaining: number;
  lastError?: string;
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

export type TrackerAccount = {
  tracker: string;
  account: Account;
  provenance: Provenance;
  error?: string;
};

export type SearchTorrent = {
  tracker?: string;
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
  leechStatus?: LeechStatus;
  canUseToken: boolean;
  remasterTitle?: string;
  infoHash?: string;
  downloads: LiveDownloadStatus[];
};

export type SearchGroup = {
  id?: string;
  tracker: string;
  groupId: number;
  name: string;
  artist?: string;
  year?: number;
  releaseType?: string;
  image?: string;
  tags: string[];
  torrents: SearchTorrent[];
  sources: ReleaseSource[];
  albumCoverage?: AlbumCoverage;
};

export type SearchPage = {
  currentPage: number;
  totalPages: number;
  totalResults?: number;
  groups: SearchGroup[];
  deduplication: DeduplicationIndexStatus;
  sourceStatus: {
    tracker: string;
    state: string;
    error?: string;
  }[];
};

export type AlbumReference = {
  tracker: string;
  groupId: number;
  title: string;
  year?: number;
};

export type AlbumCoverage = {
  albums: AlbumReference[];
  confidence: "exact" | "fuzzy";
};

export type DeduplicationIndexStatus = {
  checked: number;
  total: number;
  pending: number;
  resolving: number;
  failed: number;
  hidden: number;
  tracklistsIndexed: number;
  tracklistsTotal: number;
  tracklistsPending: number;
  tracklistsResolving: number;
  tracklistsFailed: number;
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

export type DownloadDiagnostic = {
  code: "missing_files" | "client_error" | string;
  summary: string;
  message: string;
  action: string;
};

export type LiveDownloadStatus = {
  client: string;
  infoHash: string;
  state: ClientDownloadState;
  clientState: string;
  diagnostic?: DownloadDiagnostic;
  progress: number;
  size: number;
  downloaded: number;
  uploaded: number;
  downloadSpeed: number;
  uploadSpeed: number;
  eta?: number;
  ratio: number;
  savePath: string;
  addedAt?: string;
  completedAt?: string;
};

export type ReleaseSummary = {
  id?: string;
  tracker: string;
  groupId: number;
  title: string;
  artist?: string;
  artists: ArtistCredit[];
  year?: number;
  artwork?: string;
  releaseType?: string;
  sources: ReleaseSource[];
  albumCoverage?: AlbumCoverage;
};

export type ReleaseSource = {
  tracker: string;
  groupId: number;
  matchScore: number;
};

export type LeechStatus =
  | "regular"
  | "freeleech"
  | "personal_freeleech"
  | "neutral"
  | "freeload";

export type DownloadEligibility = {
  eligible: boolean;
  reason:
    | "eligible"
    | "tracker_disabled"
    | "freeleech_required"
    | "token_unavailable"
    | "below_quality_cutoff";
  requiresToken: boolean;
  tokenAvailable: boolean;
};

export type ArtistCredit = {
  canonicalId?: string;
  key: string;
  tracker: string;
  artistId?: number;
  name: string;
  role: "primary" | "guest";
  source: "structured" | "display_fallback";
};

export type LibraryAvailability = "present" | "partial" | "missing";

export type LibraryCopy = {
  client: string;
  infoHash: string;
  present: boolean;
  completedAt: string;
  lastSeenAt: string;
  missingSince?: string;
};

export type LibraryVariantState = {
  availability: LibraryAvailability;
  copies: LibraryCopy[];
};

export type TorrentVariant = {
  tracker: string;
  torrentId: number;
  groupId: number;
  infoHash?: string;
  format?: string;
  encoding?: string;
  media?: string;
  size?: number;
  seeders?: number;
  leechers?: number;
  snatched?: number;
  freeleech: boolean;
  leechStatus: LeechStatus;
  canUseToken: boolean;
  tokenEligibilityKnown: boolean;
  eligibility?: DownloadEligibility;
  remasterTitle?: string;
  downloads: LiveDownloadStatus[];
  library?: LibraryVariantState;
};

export type ReleaseDetail = {
  release: ReleaseSummary;
  fieldProvenance: Record<string, {
    tracker?: string;
    groupId?: number;
    manual?: boolean;
  }>;
  tags: string[];
  description?: string;
  recordLabel?: string;
  variants: TorrentVariant[];
};

export type CanonicalDownload = {
  release: ReleaseSummary;
  variant: TorrentVariant;
  download: LiveDownloadStatus;
  provenance: Provenance;
};

export type DownloadsPage = {
  items: CanonicalDownload[];
  index: {
    linked: number;
    pending: number;
    resolving: number;
    failed: number;
    unconfigured: number;
  };
};

export type CrossSeedPlan = {
  sourceTracker: string;
  sourceTorrentId: number;
  sourceClient: string;
  sourceInfoHash: string;
  sourcePath: string;
  targetTracker: string;
  targetTorrentId: number;
  compatible: boolean;
  matchedFiles: number;
  targetFiles: number;
  missingFiles: string[];
  policyEligible: boolean;
  summary: string;
  dryRun: boolean;
};

export type LibraryRelease = {
  release: ReleaseSummary;
  variants: TorrentVariant[];
  availability: LibraryAvailability;
  addedAt: string;
  provenance: Provenance;
};

export type LibraryArtistSummary = {
  id?: string;
  key: string;
  tracker: string;
  artistId?: number;
  creditSource: "structured" | "display_fallback";
  name: string;
  releaseCount: number;
  missingCount: number;
  artworks: string[];
};

export type LibraryIndexStatus = {
  lastSuccessfulScanAt?: string;
  unresolvedCredits: number;
  deduplication: DeduplicationIndexStatus;
};

export type LibraryArtistsPage = {
  artists: LibraryArtistSummary[];
  releases: LibraryRelease[];
  artistTotal: number;
  releaseTotal: number;
  index: LibraryIndexStatus;
};

export type LibraryArtistPage = {
  artist: LibraryArtistSummary;
  items: LibraryRelease[];
  total: number;
  index: LibraryIndexStatus;
};

export type ArtistCatalogRole =
  | "primary"
  | "guest"
  | "remixer"
  | "composer"
  | "conductor"
  | "dj"
  | "producer"
  | "arranger";

export type ArtistCatalogRelease = {
  release: ReleaseSummary;
  tags: string[];
  variants: TorrentVariant[];
  roles: ArtistCatalogRole[];
  listedOnTracker: boolean;
  libraryAvailability?: LibraryAvailability;
  libraryAddedAt?: string;
};

export type ArtistCatalogPage = {
  artist: {
    id?: string;
    tracker: string;
    artistId: number;
    name: string;
    artwork?: string;
  };
  groups: ArtistCatalogRelease[];
  primaryCount: number;
  appearanceCount: number;
  deduplication: DeduplicationIndexStatus;
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

export type QualityPreference = "hi_res" | "lossless" | "320" | "v0" | "other";

export type ReleasePreferences = {
  qualityOrder: QualityPreference[];
  minimumQuality: QualityPreference;
  mediaTiers: string[][];
  trackerOrder: string[];
  trackerPolicies: TrackerPreference[];
};

export type TrackerDownloadMode =
  | "disabled"
  | "freeleech_only"
  | "freeleech_or_token"
  | "any";

export type TrackerPreference = {
  tracker: string;
  mode: TrackerDownloadMode;
  autoUseTokens: boolean;
};

export type RuntimePreferences = {
  release: ReleasePreferences;
};

export type DownloadSelection = {
  name: string;
  artist?: string;
  torrent: {
    tracker?: string;
    torrentId: number;
    format?: string;
    encoding?: string;
    freeleech: boolean;
    leechStatus?: LeechStatus;
    canUseToken: boolean;
    tokenEligibilityKnown?: boolean;
    eligibility?: DownloadEligibility;
  };
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
