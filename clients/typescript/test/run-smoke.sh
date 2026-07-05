#!/usr/bin/env bash
# Runtime smoke: spawn a real `pawrly console` and drive the built REST transport
# against it. Requires the binary (`cargo build -p pawrly-cli`) and the built SDK
# (`pnpm run build`).
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"   # clients/typescript
root="$(cd "$here/../.." && pwd)"          # repo root
bin="$root/target/debug/pawrly"

[ -x "$bin" ] || { echo "build the binary first: cargo build -p pawrly-cli"; exit 1; }
[ -f "$here/dist/index.js" ] || { echo "build the SDK first: pnpm run build"; exit 1; }

tmp="$(mktemp -d)"
printf 'version: 1\n' > "$tmp/pawrly.yaml"
port="$(python3 -c "import socket;s=socket.socket();s.bind(('127.0.0.1',0));print(s.getsockname()[1]);s.close()")"

"$bin" --config "$tmp/pawrly.yaml" console --addr "127.0.0.1:$port" >"$tmp/console.log" 2>&1 &
pid=$!
trap 'kill $pid 2>/dev/null || true' EXIT

if ! PAWRLY_PORT="$port" node "$here/test/smoke.mjs"; then
  echo "--- console.log ---"
  cat "$tmp/console.log"
  exit 1
fi
