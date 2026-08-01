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

export type PlexIntegrationStatus = {
  configured: boolean;
  sectionId?: number;
  libraryRoots: string[];
  pendingScans: number;
};

export type PlexScanQueued = {
  jobIds: string[];
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
  eligibility?: DownloadEligibility;
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
    | "token_cost_unknown"
    | "below_quality_cutoff"
    | "below_media_cutoff";
  requiresToken: boolean;
  tokenAvailable: boolean;
  tokenCost?: number;
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
export type VariantSortCriterion = "quality" | "tracker" | "media" | "edition";

export type ReleasePreferences = {
  qualityTiers: QualityPreference[][];
  qualityCutoffIndex: number;
  mediaTiers: string[][];
  mediaCutoffIndex: number;
  variantSortOrder: VariantSortCriterion[];
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
  downloadProfile?: string;
  autoTokenLimit: number;
};

export type RuntimePreferences = {
  release: ReleasePreferences;
  api: ApiPreferences;
};

export type ApiPreferences = {
  providers: Record<string, ProviderPolicyOverride>;
};

export type ProviderPolicyOverride = {
  minimumIntervalMs?: number;
  backgroundMinimumIntervalMs?: number;
  maxConcurrency?: number;
};

export type ProviderCircuitState =
  | "available"
  | "cooldown"
  | "half_open"
  | "blocked"
  | "paused";

export type ProviderStatus = {
  id: string;
  displayName: string;
  kind: string;
  state: ProviderCircuitState;
  reasonCode?: string;
  message?: string;
  lastRequestAt?: string;
  lastSuccessAt?: string;
  lastFailureAt?: string;
  retryAt?: string;
  lastBackgroundRequestAt?: string;
  consecutiveFailures: number;
  minimumIntervalMs: number;
  safeMinimumIntervalMs: number;
  backgroundMinimumIntervalMs: number;
  safeBackgroundMinimumIntervalMs: number;
  maxConcurrency: number;
  safeMaxConcurrency: number;
  queued: {
    interactive: number;
    download: number;
    manual: number;
    scheduled: number;
    background: number;
  };
  canPause: boolean;
  canResume: boolean;
};

export type BackgroundJobState =
  | "pending"
  | "running"
  | "retrying"
  | "waiting"
  | "completed"
  | "failed"
  | "cancelled";

export type BackgroundJobStatus = {
  id: string;
  deduplicationKey: string;
  kind: string;
  state: BackgroundJobState;
  providerId?: string;
  lane: string;
  priority: number;
  attempts: number;
  deferrals: number;
  maxAttempts: number;
  nextRunAt?: string;
  leaseUntil?: string;
  progressCompleted: number;
  progressTotal?: number;
  progressMessage?: string;
  lastErrorCode?: string;
  lastErrorMessage?: string;
  parentId?: string;
  createdAt: string;
  updatedAt: string;
  startedAt?: string;
  finishedAt?: string;
  canCancel: boolean;
  canRetry: boolean;
};

export type BackgroundJobsOverview = {
  counts: Record<BackgroundJobState, number>;
  jobs: BackgroundJobStatus[];
};

export type ChannelKind = "country_chart" | "lastfm";

export type ChannelConfig = {
  id: string;
  kind: ChannelKind;
  enabled: boolean;
  schedule: {
    weekday: number;
    time: string;
    timezone: string;
  };
  countryChart?: {
    country: string;
  };
  lastfm?: {
    username: string;
    period: "7day" | "1month" | "3month" | "6month" | "12month" | "overall";
    packSize: number;
    suppressionPacks: number;
    catalogCountry: string;
  };
  credentialConfigured: boolean;
  nextRefreshAt?: string;
  lastSuccessfulAt?: string;
  lastAttemptAt?: string;
  lastError?: string;
  failureCount: number;
  updatedAt: string;
};

export type ChannelRun = {
  id: string;
  channelId: string;
  trigger: "scheduled" | "manual";
  status: "running" | "successful" | "partial" | "failed";
  phase?: "discovering" | "matching" | "planning" | "saving";
  progressCompleted: number;
  progressTotal?: number;
  progressMessage?: string;
  packId?: string;
  error?: string;
  startedAt: string;
  updatedAt: string;
  finishedAt?: string;
};

export type ChannelPlanSummary = {
  executable: number;
  skipped: number;
  totalSize: number;
  tokenUses: number;
  byTracker: Record<string, number>;
  byReason: Record<string, number>;
};

export type ChannelPackSummary = {
  id: string;
  channelId: string;
  decision: "open" | "accepted" | "rejected";
  partial: boolean;
  sourceTitle: string;
  planVersion: number;
  summary: ChannelPlanSummary;
  createdAt: string;
};

export type ChannelOverview = {
  channel: ChannelConfig;
  activeRun?: ChannelRun;
  latestPack?: ChannelPackSummary;
};

export type ChannelPackItem = {
  ordinal: number;
  source: {
    id: string;
    rank: number;
    artist: string;
    title: string;
    year?: number;
    artwork?: string;
    url?: string;
    mbid?: string;
    score?: number;
    catalogCountry?: string;
    substitutedFrom?: {
      title: string;
      url?: string;
      mbid?: string;
      releaseType: string;
    };
  };
  matchState: "matched" | "unmatched" | "ambiguous" | "error";
  release?: ReleaseSummary;
  variants: TorrentVariant[];
  planState:
    | "executable"
    | "already_owned"
    | "already_downloading"
    | "duplicate"
    | "token_budget_exceeded"
    | "capacity_blocked"
    | "excluded"
    | "unmatched"
    | "ambiguous"
    | "policy_blocked"
    | "no_profile"
    | "source_error"
    | "submitted";
  plan?: {
    tracker: string;
    torrentId: number;
    profile: string;
    useToken: boolean;
    tokenCost: number;
    size?: number;
    format?: string;
    encoding?: string;
    media?: string;
  };
  reason?: string;
  jobId?: string;
  job?: DownloadJob;
};

export type ChannelPack = ChannelPackSummary & {
  planStale: boolean;
  items: ChannelPackItem[];
  decidedAt?: string;
};

export type ChannelBatchResult = {
  packId: string;
  submitted: number;
  skipped: number;
  jobs: DownloadJob[];
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

export class ApiError extends Error {
  constructor(
    message: string,
    readonly status: number,
    readonly code?: string,
    readonly retryable = false
  ) {
    super(message);
    this.name = "ApiError";
  }
}

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
    throw new ApiError(
      message,
      response.status,
      body?.error?.code,
      Boolean(body?.error?.retryable)
    );
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
