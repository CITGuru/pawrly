#!/usr/bin/env bash
# Runtime smoke for the mutating methods over BOTH transports. Each transport
# gets its own disposable copy of the `examples/semantic` workspace (the methods
# add/remove sources and touch the cache). Needs the binary and the built SDK.
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"   # clients/typescript
root="$(cd "$here/../.." && pwd)"          # repo root
bin="$root/target/debug/pawrly"

[ -x "$bin" ] || { echo "build the binary first: cargo build -p pawrly-cli"; exit 1; }
[ -f "$here/dist/index.js" ] || { echo "build the SDK first: pnpm run build"; exit 1; }

port() { node -e "const s=require('net').createServer();s.listen(0,'127.0.0.1',()=>{console.log(s.address().port);s.close()})"; }

run() {  # $1=env name  $2=mode  $3=binary --addr scheme  $4=client endpoint scheme
  local ws; ws="$(mktemp -d)/ws"
  cp -r "$root/examples/semantic" "$ws"
  local p; p="$(port)"
  "$bin" --config "$ws/pawrly.yaml" "$2" --addr "$3127.0.0.1:$p" >"$ws/engine.log" 2>&1 &
  local pid=$!
  if ! env "$1=$4127.0.0.1:$p" PAWRLY_WS="$ws" node "$here/test/mut_smoke.mjs"; then
    cat "$ws/engine.log"; kill $pid 2>/dev/null || true; exit 1
  fi
  kill $pid 2>/dev/null || true
}

echo "### REST ###"; run PAWRLY_REST console "" "http://"
echo "### gRPC ###"; run PAWRLY_GRPC serve "tcp://" "tcp://"
