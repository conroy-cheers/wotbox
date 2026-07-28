# Wotbox

Wotbox is a single-user web manager for Gazelle music trackers and
qBittorrent. Tracker metadata remains authoritative; Wotbox stores only
sanitized snapshots and local download state.

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

The `gazelle_api` crate is used for its tracker rate limiter. Its higher-level
models are not used as the public Wotbox contract because OPS and RED expose
small response differences; the adapter normalizes those responses while
retaining a sanitized source payload. The tracker and download client traits
are the intended seams for RED and future Flood support.

The download API exposes `GET /api/v1/downloads` as `{ items, index }`, where
each visible item combines a canonical release, its matched torrent variant,
and live client state. Pending, failed, and unconfigured torrents remain
hidden while the background index progressively resolves configured announce
hosts. `GET /api/v1/downloads/{client}/{infoHash}` remains as a compatibility
lookup for resolved torrents. `POST /api/v1/downloads` remains the
tracker-to-client submission endpoint.

Completed, linked torrents also form a durable Library. Once a torrent reaches
100%, its canonical release remains in the Library even if its client copy
later disappears; successful client scans mark that copy missing instead of
deleting the catalog record. `GET /api/v1/library/artists` provides the
artist-sorted index and mixed artist/release search, while
`GET /api/v1/library/artists/{tracker}/{artistKey}` returns one artist's
completed releases. Artist membership comes only from structured Gazelle
primary and guest credits, with the tracker's exact display artist used as a
temporary fallback while group metadata is enriched.

## Development

For ordinary local development against the music qBittorrent instance on
`sleet`, put `OPS_TOKEN` and `QBITTORRENT_API_KEY` in the ignored `.env` file,
then run:

```console
nix run .#dev-sleet
```

This opens an SSH local-forward from `127.0.0.1:18001` to qBittorrent on
`sleet`, starts the Rust API on port 8780, and starts the Vite UI on
<http://127.0.0.1:5173>. The tunnel and both development processes are stopped
together. Local SQLite state lives under the ignored `.state/` directory;
the OPS credential remains in `.env`. Unless `QBITTORRENT_API_KEY` is already
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

Run the full validation set with:

```console
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
pnpm --dir frontend check
pnpm --dir frontend test
nix flake check
```

`flake.nix` exports the packaged service, the separately buildable frontend,
the development shell, and `nixosModules.default`. See
[`config.example.toml`](config.example.toml) for the runtime contract; secrets
are always referenced by file path.

## License

AGPL-3.0-only.
