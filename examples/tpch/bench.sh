#!/usr/bin/env bash
# Benchmark the 22 federated TPC-H queries through Pawrly.
#
# Runs against a warm `pawrly serve` daemon so timings reflect engine query time
# (DuckDB plan + federated scans of Postgres/SQLite/parquet) rather than
# per-invocation process + DuckDB startup. Each query is run once to warm, then
# ITER times; we report the minimum and median wall time (client->daemon
# roundtrip included).
#
# Usage: ./bench.sh [ITER]   (default 3)
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$HERE"
ITER="${1:-3}"
PAWRLY="${PAWRLY:-pawrly}"
HOME_DIR="$HERE/.pawrlyhome"
SOCK="$HOME_DIR/pawrly.sock"
REMOTE="uds://$SOCK"
RESULTS="$HERE/results"
mkdir -p "$HOME_DIR" "$RESULTS"

cleanup() { [[ -n "${DAEMON_PID:-}" ]] && kill "$DAEMON_PID" 2>/dev/null || true; }
trap cleanup EXIT

echo ">> starting daemon"
$PAWRLY --home "$HOME_DIR" --config "$HERE/pawrly.yaml" serve --socket "$SOCK" \
  >"$HOME_DIR/serve.log" 2>&1 &
DAEMON_PID=$!

# wait for health
for i in $(seq 1 50); do
  if $PAWRLY --remote "$REMOTE" sql "SELECT 1" >/dev/null 2>&1; then break; fi
  sleep 0.2
  if ! kill -0 "$DAEMON_PID" 2>/dev/null; then echo "daemon died:"; cat "$HOME_DIR/serve.log"; exit 1; fi
done

# time one `pawrly sql --file` call in milliseconds (query output discarded so
# only the timing number reaches stdout); prints elapsed ms, or -1 on error.
time_one() {
  QF="$1" REMOTE="$REMOTE" PAWRLY_BIN="$PAWRLY" perl -MTime::HiRes=time -e '
    my $t0 = time;
    my $rc = system("$ENV{PAWRLY_BIN} --remote \"$ENV{REMOTE}\" sql --file \"$ENV{QF}\" --format csv --max-rows 0 >/dev/null 2>&1");
    printf "%.1f", ($rc == 0 ? (time - $t0) * 1000 : -1);
  '
}

printf "%-5s %8s %12s %12s\n" "query" "rows" "min_ms" "median_ms"
printf -- "------------------------------------------\n"

total_min=0; declare -a geo
for f in queries/q*.sql; do
  q="$(basename "$f" .sql)"
  # warm + capture row count
  out="$RESULTS/$q.csv"
  if ! $PAWRLY --remote "$REMOTE" sql --file "$f" --format csv --max-rows 0 >"$out" 2>"$RESULTS/$q.err"; then
    printf "%-5s %8s %12s %12s\n" "$q" "ERR" "-" "-"; sed 's/^/    /' "$RESULTS/$q.err" | head -3; continue
  fi
  rows=$(($(wc -l < "$out") - 1)); rows=$(( rows < 0 ? 0 : rows ))
  # timed iterations
  times=()
  for ((i=0;i<ITER;i++)); do times+=("$(time_one "$f")"); done
  read -r mn md < <(printf '%s\n' "${times[@]}" | sort -n | awk '
    {a[NR]=$1} END{ printf "%.1f %.1f\n", a[1], (NR%2? a[(NR+1)/2] : (a[NR/2]+a[NR/2+1])/2) }') || true
  printf "%-5s %8s %12s %12s\n" "$q" "$rows" "$mn" "$md"
  total_min=$(perl -e "print $total_min + $mn")
  geo+=("$mn")
done

gm=$(printf '%s\n' "${geo[@]}" | perl -ne 'push @v,$_; END{ my $s=0; $s+=log($_) for @v; printf "%.1f", exp($s/scalar @v) }')
printf -- "------------------------------------------\n"
printf ">> %d queries | total(min) = %.0f ms | geomean(min) = %s ms | ITER=%d\n" \
  "${#geo[@]}" "$total_min" "$gm" "$ITER"
