#!/usr/bin/env bash
# Runtime smoke: spawn a real `pawrly serve` (gRPC) and drive the gRPC transport
# against it. Requires the binary (`cargo build -p pawrly-cli`), the `grpc` extra
# (grpcio + pyarrow), and the generated stubs (scripts/generate.sh).
#   PY=/path/to/python3.12 ./test/run-grpc-smoke.sh
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"   # clients/python
root="$(cd "$here/../.." && pwd)"          # repo root
bin="$root/target/debug/pawrly"
py="${PY:-python3}"

[ -x "$bin" ] || { echo "build the binary first: cargo build -p pawrly-cli"; exit 1; }

tmp="$(mktemp -d)"
printf 'version: 1\n' > "$tmp/pawrly.yaml"
port="$("$py" -c "import socket;s=socket.socket();s.bind(('127.0.0.1',0));print(s.getsockname()[1]);s.close()")"

"$bin" --config "$tmp/pawrly.yaml" serve --addr "tcp://127.0.0.1:$port" >"$tmp/serve.log" 2>&1 &
pid=$!
trap 'kill $pid 2>/dev/null || true' EXIT

if ! PAWRLY_GRPC="tcp://127.0.0.1:$port" "$py" "$here/test/grpc_smoke.py"; then
  echo "--- serve.log ---"
  cat "$tmp/serve.log"
  exit 1
fi
