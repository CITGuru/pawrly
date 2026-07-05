#!/usr/bin/env bash
# Generate the gRPC stubs into src/pawrly/v1 (needs the `dev` extra: grpcio-tools).
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
proto="$here/../../crates/pawrly-proto/proto"
out="$here/src"
python -m grpc_tools.protoc -I "$proto" \
  --python_out="$out" --grpc_python_out="$out" \
  "$proto"/pawrly/v1/*.proto
touch "$out/pawrly/v1/__init__.py"
echo "generated stubs in $out/pawrly/v1"
