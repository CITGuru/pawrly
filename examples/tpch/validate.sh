#!/usr/bin/env bash
# Correctness gate: run the canonical (unqualified) TPC-H queries in native
# DuckDB over the same SF1 data, and diff the results against what Pawrly
# produced through the federated sources (examples/tpch/results/*.csv, written
# by bench.sh). Both engines are DuckDB, so faithful results must match exactly.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$HERE"
EXP="$HERE/expected"
DB="$EXP/tpch.duckdb"
mkdir -p "$EXP/sql"

if [[ ! -f "$DB" ]]; then
  echo ">> building native DuckDB SF1 reference db (once)"
  rm -f "$DB"
  duckdb "$DB" -c "INSTALL tpch; LOAD tpch; CALL dbgen(sf=1);" >/dev/null
fi

# Canonicalize a CSV for comparison: drop the header row (engines auto-label
# unaliased aggregate columns differently, e.g. sum(l_quantity) vs
# sum(facts.lineitem.l_quantity) — a label, not data), round every numeric field
# to 2 decimals so Pawrly's 6-decimal display and DuckDB's full-precision floats
# compare equal, then sort rows (guards against tie ordering).
norm() {
  tail -n +2 "$1" | perl -F',' -lane '
    my @o;
    for my $c (@F) {
      push @o, ($c =~ /^-?\d+(?:\.\d+)?$/ ? sprintf("%.2f", $c) : $c);
    }
    print join(",", @o);
  ' | sort
}

pass=0; fail=0; miss=0
printf "%-5s %s\n" "query" "result"
printf -- "----------------------\n"
for n in $(seq 1 22); do
  q=$(printf 'q%02d' "$n")
  duckdb -noheader -list -c "INSTALL tpch; LOAD tpch; SELECT rtrim(query,';') FROM tpch_queries() WHERE query_nr=$n" \
    > "$EXP/sql/$q.sql"
  duckdb "$DB" -csv -c ".read $EXP/sql/$q.sql" > "$EXP/$q.csv" 2>/dev/null

  if [[ ! -s "results/$q.csv" ]]; then
    printf "%-5s %s\n" "$q" "MISSING pawrly result (run ./bench.sh first)"; miss=$((miss+1)); continue
  fi
  d=$(diff <(norm "results/$q.csv") <(norm "$EXP/$q.csv") | grep -cE '^[<>]' || true)
  if [[ "$d" -eq 0 ]]; then
    printf "%-5s %s\n" "$q" "PASS"; pass=$((pass+1))
  else
    printf "%-5s %s\n" "$q" "DIFF ($d lines)"; fail=$((fail+1))
  fi
done
printf -- "----------------------\n"
printf ">> %d PASS  %d DIFF  %d MISSING (of 22)\n" "$pass" "$fail" "$miss"
[[ $fail -eq 0 && $miss -eq 0 ]]
