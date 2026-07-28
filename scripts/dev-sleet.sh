umask 077

if [[ ! -f Cargo.toml || ! -f frontend/package.json ]]; then
  echo "Run this command from the Wotbox repository root." >&2
  exit 1
fi

if [[ ! -f .env ]]; then
  echo "Missing .env. It must provide OPS_TOKEN and QBITTORRENT_API_KEY." >&2
  exit 1
fi

ssh_host="${WOTBOX_DEV_SSH_HOST:-sleet}"
qbit_remote_port="${WOTBOX_DEV_QBIT_REMOTE_PORT:-8001}"
# qBittorrent validates the request Host port during API-key authentication,
# so the local tunnel must mirror the upstream WebUI port by default.
qbit_local_port="${WOTBOX_DEV_QBIT_LOCAL_PORT:-8001}"
backend_port="${WOTBOX_DEV_BACKEND_PORT:-8780}"
frontend_port="${WOTBOX_DEV_FRONTEND_PORT:-5173}"
state_directory="${WOTBOX_DEV_STATE_DIRECTORY:-$PWD/.state}"
qbit_url="${WOTBOX_DEV_QBIT_URL:-}"
qbit_secret_path="${WOTBOX_DEV_QBIT_SECRET_PATH:-/run/agenix/wotbox.qbittorrent-api-key}"

mkdir -p "$state_directory"
chmod 700 "$state_directory"

tunnel_pid=""
backend_pid=""
frontend_pid=""

cleanup() {
  status=$?
  trap - EXIT INT TERM
  for pid in "$frontend_pid" "$backend_pid" "$tunnel_pid"; do
    if [[ -n "$pid" ]]; then
      kill "$pid" >/dev/null 2>&1 || true
    fi
  done
  wait >/dev/null 2>&1 || true
  exit "$status"
}
trap cleanup EXIT INT TERM

if [[ -z "$qbit_url" ]]; then
  if nc -z 127.0.0.1 "$qbit_local_port" >/dev/null 2>&1; then
    echo "Local qBittorrent tunnel port $qbit_local_port is already in use." >&2
    exit 1
  fi

  ssh \
    -N \
    -o BatchMode=yes \
    -o ConnectTimeout=15 \
    -o ExitOnForwardFailure=yes \
    -o ServerAliveInterval=30 \
    -o ServerAliveCountMax=3 \
    -L "127.0.0.1:${qbit_local_port}:127.0.0.1:${qbit_remote_port}" \
    "$ssh_host" &
  tunnel_pid=$!

  tunnel_ready=false
  for _ in $(seq 1 600); do
    if nc -z 127.0.0.1 "$qbit_local_port" >/dev/null 2>&1; then
      tunnel_ready=true
      break
    fi
    if ! kill -0 "$tunnel_pid" >/dev/null 2>&1; then
      echo "SSH tunnel to $ssh_host exited before becoming ready." >&2
      exit 1
    fi
    sleep 0.1
  done
  if [[ "$tunnel_ready" != true ]]; then
    echo "Timed out opening the qBittorrent tunnel to $ssh_host." >&2
    exit 1
  fi
  qbit_url="http://127.0.0.1:${qbit_local_port}"
fi

if [[ -z "${QBITTORRENT_API_KEY:-}" ]] && ! grep -q '^QBITTORRENT_API_KEY=' .env; then
  if [[ ! "$qbit_secret_path" =~ ^/[A-Za-z0-9._/-]+$ ]]; then
    echo "WOTBOX_DEV_QBIT_SECRET_PATH must be an absolute path." >&2
    exit 1
  fi
  QBITTORRENT_API_KEY="$(
    ssh \
      -o BatchMode=yes \
      -o ConnectTimeout=15 \
      "$ssh_host" \
      sudo -n cat "$qbit_secret_path"
  )"
  export QBITTORRENT_API_KEY
fi

export QBITTORRENT_URL="$qbit_url"
export WOTBOX_DATABASE_PATH="${state_directory}/wotbox.sqlite"
export WOTBOX_PORT="$backend_port"
export WOTBOX_BASE_PATH="/"
export WOTBOX_DEV_BACKEND_URL="http://127.0.0.1:${backend_port}"

pnpm --dir frontend install --frozen-lockfile

if [[ -n "$tunnel_pid" ]]; then
  echo "qBittorrent tunnel: $ssh_host:127.0.0.1:$qbit_remote_port -> 127.0.0.1:$qbit_local_port"
else
  echo "qBittorrent URL: $qbit_url"
fi
echo "Wotbox UI: http://127.0.0.1:$frontend_port"
echo "Wotbox API: http://127.0.0.1:$backend_port"

cargo run &
backend_pid=$!

pnpm --dir frontend dev --host 127.0.0.1 --port "$frontend_port" &
frontend_pid=$!

if [[ -n "$tunnel_pid" ]]; then
  wait -n "$tunnel_pid" "$backend_pid" "$frontend_pid"
else
  wait -n "$backend_pid" "$frontend_pid"
fi
