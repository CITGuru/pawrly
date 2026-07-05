#!/usr/bin/env bash
# Runtime smoke for the read-only methods over BOTH transports. Spins a
# `pawrly console` (REST) and a `pawrly serve` (gRPC) on the `examples/semantic`
# workspace and drives the read methods against each. Needs the binary
# (`cargo build -p pawrly-cli`) and the built SDK (`pnpm run build`).
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"   # clients/typescript
root="$(cd "$here/../.." && pwd)"          # repo root
bin="$root/target/debug/pawrly"
cfg="$root/examples/semantic/pawrly.yaml"

[ -x "$bin" ] || { echo "build the binary first: cargo build -p pawrly-cli"; exit 1; }
[ -f "$here/dist/index.js" ] || { echo "build the SDK first: pnpm run build"; exit 1; }

port() { node -e "const s=require('net').createServer();s.listen(0,'127.0.0.1',()=>{console.log(s.address().port);s.close()})"; }
tmp="$(mktemp -d)"

rp="$(port)"
"$bin" --config "$cfg" console --addr "127.0.0.1:$rp" >"$tmp/console.log" 2>&1 &
rpid=$!
gp="$(port)"
"$bin" --config "$cfg" serve --addr "tcp://127.0.0.1:$gp" >"$tmp/serve.log" 2>&1 &
gpid=$!
trap 'kill $rpid $gpid 2>/dev/null || true' EXIT

echo "### REST ###"
PAWRLY_REST="http://127.0.0.1:$rp" node "$here/test/read_smoke.mjs" || { cat "$tmp/console.log"; exit 1; }
echo "### gRPC ###"
PAWRLY_GRPC="tcp://127.0.0.1:$gp" node "$here/test/read_smoke.mjs" || { cat "$tmp/serve.log"; exit 1; }
