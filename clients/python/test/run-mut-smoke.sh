#!/usr/bin/env bash
# Runtime smoke for the mutating methods over BOTH transports. Each transport
# gets its own disposable copy of the `examples/semantic` workspace (the methods
# add/remove sources and touch the cache). Needs the binary, `requests`, and the
# `grpc` extra + stubs.
#   PY=/path/to/python3.12 ./test/run-mut-smoke.sh
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"   # clients/python
root="$(cd "$here/../.." && pwd)"          # repo root
bin="$root/target/debug/pawrly"
py="${PY:-python3}"

[ -x "$bin" ] || { echo "build the binary first: cargo build -p pawrly-cli"; exit 1; }

port() { "$py" -c "import socket;s=socket.socket();s.bind(('127.0.0.1',0));print(s.getsockname()[1]);s.close()"; }

run() {  # $1=env name  $2=mode  $3=binary --addr scheme  $4=client endpoint scheme
  local ws; ws="$(mktemp -d)/ws"
  cp -r "$root/examples/semantic" "$ws"
  local p; p="$(port)"
  "$bin" --config "$ws/pawrly.yaml" "$2" --addr "$3127.0.0.1:$p" >"$ws/engine.log" 2>&1 &
  local pid=$!
  if ! env "$1=$4127.0.0.1:$p" PAWRLY_WS="$ws" "$py" "$here/test/mut_smoke.py"; then
    cat "$ws/engine.log"; kill $pid 2>/dev/null || true; exit 1
  fi
  kill $pid 2>/dev/null || true
}

echo "### REST ###"; run PAWRLY_REST console "" "http://"
echo "### gRPC ###"; run PAWRLY_GRPC serve "tcp://" "tcp://"
