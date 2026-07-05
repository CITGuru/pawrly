#!/usr/bin/env bash
# Runtime smoke for the read-only methods over BOTH transports. Spins a
# `pawrly console` (REST) and a `pawrly serve` (gRPC) on the `examples/semantic`
# workspace and drives the read methods against each. Needs the binary
# (`cargo build -p pawrly-cli`), `requests`, and the `grpc` extra + stubs.
#   PY=/path/to/python3.12 ./test/run-read-smoke.sh
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"   # clients/python
root="$(cd "$here/../.." && pwd)"          # repo root
bin="$root/target/debug/pawrly"
cfg="$root/examples/semantic/pawrly.yaml"
py="${PY:-python3}"

[ -x "$bin" ] || { echo "build the binary first: cargo build -p pawrly-cli"; exit 1; }

port() { "$py" -c "import socket;s=socket.socket();s.bind(('127.0.0.1',0));print(s.getsockname()[1]);s.close()"; }
tmp="$(mktemp -d)"

rp="$(port)"
"$bin" --config "$cfg" console --addr "127.0.0.1:$rp" >"$tmp/console.log" 2>&1 &
rpid=$!
gp="$(port)"
"$bin" --config "$cfg" serve --addr "tcp://127.0.0.1:$gp" >"$tmp/serve.log" 2>&1 &
gpid=$!
trap 'kill $rpid $gpid 2>/dev/null || true' EXIT

echo "### REST ###"
PAWRLY_REST="http://127.0.0.1:$rp" "$py" "$here/test/read_smoke.py" || { cat "$tmp/console.log"; exit 1; }
echo "### gRPC ###"
PAWRLY_GRPC="tcp://127.0.0.1:$gp" "$py" "$here/test/read_smoke.py" || { cat "$tmp/serve.log"; exit 1; }
