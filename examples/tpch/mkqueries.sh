#!/usr/bin/env bash
# Dump the 22 canonical TPC-H queries from DuckDB's tpch extension and rewrite
# every table *reference* to its Pawrly source-qualified name, so each query runs
# as a federated query across facts/pg/ref. Writes queries/qNN.sql.
#
# We qualify a table name only where it appears in a FROM-clause position —
# right after FROM / JOIN, or after a comma in a from-list. That deliberately
# leaves SELECT-list column aliases alone (TPC-H Q8/Q9 alias a column `nation`,
# which must NOT be rewritten). Unqualified column refs keep resolving because
# `facts.lineitem` carries the implicit table alias `lineitem`.
#
# Safe to re-run; overwrites queries/*.sql.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="$HERE/queries"
mkdir -p "$OUT"
rm -f "$OUT"/q*.sql

rewrite() {
  perl -0777 -pe '
    my %src = (lineitem=>"facts", orders=>"facts", partsupp=>"facts",
               customer=>"pg", supplier=>"pg", part=>"pg",
               nation=>"ref", region=>"ref");
    my $names = join("|", keys %src);
    # after FROM / JOIN keyword
    s/\b(from|join)(\s+)($names)\b/"$1$2".$src{lc $3}.".".lc($3)/gie;
    # after a comma in a from-list
    s/(,\s*)($names)\b/$1.$src{lc $2}.".".lc($2)/gie;
  '
}

for n in $(seq 1 22); do
  f="$OUT/$(printf 'q%02d.sql' "$n")"
  duckdb -noheader -list -c "INSTALL tpch; LOAD tpch; SELECT query FROM tpch_queries() WHERE query_nr=$n" \
    | rewrite > "$f"
done
echo "wrote $(ls "$OUT"/q*.sql | wc -l | tr -d ' ') query files to $OUT"
