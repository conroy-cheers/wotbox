# Wotbox

Wotbox is a single-user web manager for Gazelle music trackers and
qBittorrent. Tracker metadata remains authoritative at the source; Wotbox
normalizes it into stable local release and artist identities with explicit
field provenance, sanitized snapshots, and local download state.

## MVP architecture

- A Rust service owns the Gazelle-compatible tracker and download-client
  interfaces, rate limiting, cache provenance, download idempotency, and
  reconciliation.
- The embedded Svelte UI provides account status, tracker search, release
  metadata, and a download queue with live torrent detail. It works at `/` or
  behind a stripped reverse proxy subpath.
- SQLite stores expiring, sanitized tracker snapshots, canonical release
  records, hash-to-release resolution state, and Wotbox's download job state.
  Tracker payloads are refreshed on demand and remain the source of truth.
- qBittorrent 5.2 is accessed with its bearer API-key flow. Password sessions
  are deliberately not part of the application contract. Its torrent name,
  tags, category, and announce URL are never public metadata. The adapter keeps
  only the normalized announce hostname for tracker routing and attaches live
  transfer state to releases resolved from the tracker by info hash.

Every external request passes through Wotbox's provider governor. Tracker,
Last.fm, Apple, and qBittorrent limits are enforced across interactive,
download, scheduled, and background work; retries cannot bypass the same
queue. Rate-limit cooldowns, hard blocks, manual pauses, and failure history
are persisted in SQLite. Hard blocks require an explicit resume in
Preferences, while expired cooldowns allow one half-open probe. Wotbox also
holds an exclusive lock beside the SQLite database so two processes cannot
silently double the request rate. Each adapter normalizes its source while
retaining a sanitized payload. Releases and artists have local UUIDs;
high-confidence matches merge automatically, borderline matches enter Match
Review, and rejected pairs stay separate. Metadata fields are selected by
source-neutral completeness scoring and retain field-level provenance.
Tracker preference remains deliberately limited to download economics and
variant ranking: OPS tokens are plentiful, while RED defaults to
already-free torrents only.

Long-running library work uses a durable SQLite queue instead of detached
polling loops. Jobs have deduplication keys, priorities, bounded attempts,
provider-aware retry times, progress, cancellation, parent links, and expiring
worker leases. Two workers bound concurrency; expired leases are recovered
without allowing stale workers to commit outcomes. Domain changes and their
dependent jobs are written in one transaction, and tracklist changes invalidate
only affected Single-to-Album coverage. Preferences → Background work exposes
the live queue, failures, cancellation, and manual retry. Completed task history
is retained for 30 days.

An optional Plex integration watches the same reconciled qBittorrent completion
transitions. The first completed observation queues a durable, debounced partial
scan for the matching configured music root; nearby completions coalesce, retries
survive restarts, and no Plex token is exposed through the API or UI. Preferences
shows the configured section and roots and can queue a manual scan of every root.

The download API exposes `GET /api/v1/downloads` as `{ items, index }`, where
each visible item combines a canonical release, its matched torrent variant,
and live client state. Pending, failed, and unconfigured torrents remain
hidden while the background index progressively resolves configured announce
hosts. `POST /api/v1/downloads` remains the tracker-to-client submission
endpoint; canonical release detail is served by
`GET /api/v1/releases/{releaseId}`.

Completed, linked torrents also form a durable Library. Once a torrent reaches
100%, its canonical release remains in the Library even if its client copy
later disappears; successful client scans mark that copy missing instead of
deleting the catalog record. `GET /api/v1/library/artists` provides the
artist-sorted index and mixed artist/release search, while
`GET /api/v1/library/artists/{artistId}` returns one canonical artist's
completed releases. Artist membership comes only from structured Gazelle
primary and guest credits, with the tracker's exact display artist used as a
temporary fallback while group metadata is enriched.

Browsing views progressively suppress Singles whose complete tracker file
lists are covered by one or more quality-eligible Albums from the same primary
artist. Matching is exact after conservative filename normalization, with only
bounded typo tolerance; version qualifiers and numbered parts remain distinct.
Unresolved matches stay visible, and each release list can temporarily reveal
confirmed matches and their covering Albums. Operational Downloads and
Dashboard views are never filtered.

## UI routes

Every application view is deep-linkable and browser history restores its
query-backed state:

- `/` — Dashboard
- `/search` — tracker search; filters, result page, covered Singles, expanded
  variants, and the add confirmation are represented in the query string
- `/library` — artist index and Library search with filter and result-limit
  state
- `/library/artists/{artistId}` — one artist's combined catalog with filters,
  sorting, covered Singles, expanded variants, and add confirmation
- `/downloads` — canonical downloads with links to their releases
- `/channels` — independently scheduled recommendation channels and pack history
- `/channels/{channel}/packs/{packId}` — one immutable recommendation pack and
  its current download plan
- `/releases/{releaseId}` — canonical release detail, selected torrent,
  exact live client attachment, expanded variants, and source context
- `/matches` — review ambiguous artist and release matches
- `/preferences` — runtime release, channel, external API safety, and background
  task controls

The embedded server returns the SPA shell for these routes so they can be
loaded directly. Unknown paths render the Wotbox 404 view and carry an HTTP
404 status in packaged builds. Legacy tracker/group and tracker/artist URLs
are intentionally not redirected.

## Development

For ordinary local development against the music qBittorrent instance on
`sleet`, put `OPS_TOKEN` and (optionally) `RED_TOKEN` in the ignored `.env`
file, then run:

```console
nix run .#dev-sleet
```

This opens an SSH local-forward from `127.0.0.1:8001` to qBittorrent on
`sleet`, starts the Rust API on port 8780, and starts the Vite UI on
<http://127.0.0.1:5173>. The tunnel and both development processes are stopped
together. Local SQLite state lives under the ignored `.state/` directory;
tracker credentials remain in `.env`. Unless `QBITTORRENT_API_KEY` is already
provided, the workflow reads qBittorrent's key from
`/run/agenix/wotbox.qbittorrent-api-key` over SSH and keeps it only in the
backend process environment.

The defaults can be changed with `WOTBOX_DEV_SSH_HOST`,
`WOTBOX_DEV_QBIT_LOCAL_PORT`, `WOTBOX_DEV_QBIT_REMOTE_PORT`,
`WOTBOX_DEV_BACKEND_PORT`, `WOTBOX_DEV_FRONTEND_PORT`, and
`WOTBOX_DEV_STATE_DIRECTORY`. `WOTBOX_DEV_QBIT_SECRET_PATH` changes the remote
agenix path. Set `WOTBOX_DEV_QBIT_URL` to use an existing forward or another
qBittorrent endpoint instead of creating the managed SSH tunnel.

For manual development without the managed tunnel:

```console
nix develop
pnpm --dir frontend install
pnpm --dir frontend dev
cargo run -- --config config.example.toml
```

The backend listens on `127.0.0.1:8780` by default. The Vite development
server proxies `/api` and `/health` to it.

The packaged Rust build uses crate2nix and keeps its generated `Cargo.nix` in
the repository. This lets Nix cache dependency crates independently, so a
normal source edit rebuilds the Wotbox crate without recompiling its entire
dependency graph. Regenerate the file whenever `Cargo.toml` or `Cargo.lock`
changes:

```console
nix run .#update-cargo-nix
```

The same `crate2nix` command is available inside `nix develop`. A clean
crate2nix build has more derivations than the previous monolithic build because
it populates the per-crate cache; subsequent builds reuse them.

Run the full validation set with:

```console
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
pnpm --dir frontend check
pnpm --dir frontend test
nix flake check
```

The single/album overlap detector has a separately reviewable validation
corpus at `tests/fixtures/dedupe_validation.json`. It contains labeled filename
pairs plus end-to-end release scenarios covering normalization, credits,
qualifiers, numbers, fuzzy-match boundaries, multiple editions, quality
cutoffs, B-sides, and re-recordings. Run it with metrics visible using:

```console
cargo test dedupe_validation_corpus_meets_accuracy_bars -- --nocapture
```

The regression gate requires at least 200 title comparisons and 20 release
scenarios, 99.5% precision and specificity, 98% recall, and 97% match-kind
accuracy. End-to-end release decisions must all agree with their labels.
False-positive resistance is deliberately weighted most heavily because an
incorrect positive hides a release.

Cross-tracker release matching has a separate labeled corpus at
`tests/fixtures/release_match_validation.json`. Run its precision and recall
gate with:

```console
cargo test cross_tracker_validation_corpus_meets_accuracy_gate -- --nocapture
```

Release pages also expose read-only cross-seed compatibility plans. They
compare the target tracker file list with completed qBittorrent files by name
and size; generating a plan never adds, resumes, relocates, or otherwise
changes a torrent.

Recommendation Channels turn external album discovery into reviewable,
historical packs. The country chart channel reads Apple's country-specific Top
100 Albums feed; the Last.fm channel expands recent top artists through
similar artists and their leading albums. Each channel has its own weekly,
timezone-aware schedule and is disabled until explicitly enabled or manually
refreshed. Refreshing resolves Albums and EPs against configured
trackers and creates a plan under the normal quality, tracker, freeleech, and
token rules, but never downloads automatically. A user may accept every
executable plan item or a selected subset, reject the batch while retaining it
in history, attach a manually found Album/EP to an unresolved recommendation,
or add individual releases through the normal confirmation dialog. Pack
planning removes duplicate releases, observes per-tracker automatic-token
limits, uses an explicit tracker download profile, and excludes items that
would exceed the download client's currently reported free space. Token limits
count actual tracker charges: OPS rounds each torrent up at one token per 320
MiB, while RED uses one token for each eligible torrent.

Channel and provider settings live in Preferences. Provider limits may only be
made more conservative than the built-in defaults; the status cards expose
queues, cooldowns, blocks, last success, pause, and cautious resume controls.
When a provider is unavailable, cached snapshots remain usable and the global
banner links back to those controls. Tracker Search uses fresh or stale cached
results when possible and redirects to the locally indexed Library when every
tracker is unavailable.

The Last.fm API key remains a file-based
secret configured with `lastfm_api_key_file`; environment-based development
may instead provide `LASTFM_API_KEY`. The recommendation methods are read-only
and do not use a Last.fm shared secret. A source request is attempted once;
provider cooldowns and the channel scheduler own any later retry so nested
backoff loops cannot amplify traffic. Failed scheduled refreshes receive an
exponential scheduler retry and remain visible on the Channels page.
Last.fm API error bodies are interpreted even when the HTTP status is non-success,
so a missing account reports the configured username and points back to
Preferences instead of surfacing an opaque HTTP 404.

Plex is configured with the optional `[plex]` block: `base_url`, file-based
`token_file`, numeric `section_id`, and one or more absolute `library_roots`.
Wotbox calls Plex's partial refresh endpoint only for those allowlisted roots;
environment-based development can instead use `PLEX_TOKEN`, `PLEX_SECTION_ID`,
`PLEX_LIBRARY_ROOTS`, and optionally `PLEX_URL`.

`flake.nix` exports the packaged service, the separately buildable frontend,
the development shell, and `nixosModules.default`. See
[`config.example.toml`](config.example.toml) for the runtime contract; secrets
are always referenced by file path.

## License

AGPL-3.0-only.
