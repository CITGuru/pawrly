#!/usr/bin/env bash
# Time the federated *enrichment* queries in queries_extra/ — the 4- and
# 5-backend joins that reach live public HTTP APIs (CountriesNow, Frankfurter)
# on top of parquet + Postgres + SQLite.
#
# Unlike bench.sh (the canonical 22, fully local), these include live network
# latency to the APIs, so expect more run-to-run variance. We report min and
# median over ITER runs against a warm `pawrly serve`.
#
# The fx source needs the HTTP-source object-response fix, so this defaults to
# the freshly-built local binary. Override with PAWRLY_BIN=... once installed.
#
# Usage: ./bench_extra.sh [ITER]   (default 3)
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$HERE"
ITER="${1:-3}"

# Pick the binary: explicit override, else the local release build, else PATH.
if [[ -n "${PAWRLY_BIN:-}" ]]; then
  PB="$PAWRLY_BIN"
elif [[ -x "$HERE/../../target/release/pawrly" ]]; then
  PB="$HERE/../../target/release/pawrly"
else
  PB="pawrly"
fi
echo ">> using binary: $PB"

HOME_DIR="$HERE/.pawrlyhome"
SOCK="$HOME_DIR/pawrly.sock"
REMOTE="uds://$SOCK"
RESULTS="$HERE/results_extra"
mkdir -p "$HOME_DIR" "$RESULTS"

cleanup() { [[ -n "${DAEMON_PID:-}" ]] && kill "$DAEMON_PID" 2>/dev/null || true; }
trap cleanup EXIT

echo ">> starting daemon"
"$PB" --home "$HOME_DIR" --config "$HERE/pawrly.yaml" serve --socket "$SOCK" \
  >"$HOME_DIR/serve_extra.log" 2>&1 &
DAEMON_PID=$!
for i in $(seq 1 50); do
  if "$PB" --remote "$REMOTE" sql "SELECT 1" >/dev/null 2>&1; then break; fi
  sleep 0.2
  if ! kill -0 "$DAEMON_PID" 2>/dev/null; then echo "daemon died:"; cat "$HOME_DIR/serve_extra.log"; exit 1; fi
done

# time one query file in ms (output discarded); prints elapsed ms or -1 on error
time_one() {
  QF="$1" REMOTE="$REMOTE" PB="$PB" perl -MTime::HiRes=time -e '
    my $t0 = time;
    my $rc = system("\"$ENV{PB}\" --remote \"$ENV{REMOTE}\" sql --file \"$ENV{QF}\" --format csv --max-rows 0 >/dev/null 2>&1");
    printf "%.1f", ($rc == 0 ? (time - $t0) * 1000 : -1);
  '
}

printf "%-28s %8s %10s %12s\n" "query" "rows" "min_ms" "median_ms"
printf -- "----------------------------------------------------------\n"
for f in queries_extra/*.sql; do
  [[ -e "$f" ]] || { echo "no queries in queries_extra/"; exit 1; }
  q="$(basename "$f" .sql)"
  out="$RESULTS/$q.csv"
  if ! "$PB" --remote "$REMOTE" sql --file "$f" --format csv --max-rows 0 >"$out" 2>"$RESULTS/$q.err"; then
    printf "%-28s %8s %10s %12s\n" "$q" "ERR" "-" "-"; sed 's/^/    /' "$RESULTS/$q.err" | head -3; continue
  fi
  rows=$(($(wc -l < "$out") - 1)); rows=$(( rows < 0 ? 0 : rows ))
  times=()
  for ((i=0;i<ITER;i++)); do times+=("$(time_one "$f")"); done
  read -r mn md < <(printf '%s\n' "${times[@]}" | sort -n | awk '
    {a[NR]=$1} END{ printf "%.1f %.1f\n", a[1], (NR%2? a[(NR+1)/2] : (a[NR/2]+a[NR/2+1])/2) }') || true
  printf "%-28s %8s %10s %12s\n" "$q" "$rows" "$mn" "$md"
done
printf -- "----------------------------------------------------------\n"
printf ">> live-HTTP federated queries | ITER=%d (timings include API latency)\n" "$ITER"
