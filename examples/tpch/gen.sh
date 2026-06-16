#!/usr/bin/env bash
# Generate a TPC-H SF1 dataset and spread the 8 tables across three backends so
# the 22 queries become genuine federated joins through Pawrly:
#
#   facts (parquet files) : lineitem, orders, partsupp   <- big fact tables
#   pg    (Postgres)      : customer, supplier, part
#   ref   (SQLite)        : nation, region
#
# Re-runnable: drops and recreates everything. Requires `duckdb` and a running
# local Postgres reachable with libpq defaults (trust auth on the socket).
set -euo pipefail

SF="${1:-1}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DATA="$HERE/data"
PGHOST_DIR="${PGHOST:-/tmp}"
PGDB="tpch"

mkdir -p "$DATA"
rm -f "$DATA"/*.parquet "$DATA/ref.sqlite"

echo ">> (re)creating Postgres database '$PGDB'"
psql -d postgres -v ON_ERROR_STOP=1 -c "DROP DATABASE IF EXISTS $PGDB;" >/dev/null
psql -d postgres -v ON_ERROR_STOP=1 -c "CREATE DATABASE $PGDB;" >/dev/null

echo ">> generating TPC-H SF$SF and loading the three backends (this takes a moment)"
duckdb <<SQL
INSTALL tpch;     LOAD tpch;
INSTALL postgres; LOAD postgres;
INSTALL sqlite;   LOAD sqlite;

CALL dbgen(sf=$SF);

-- facts -> parquet
COPY lineitem TO '$DATA/lineitem.parquet' (FORMAT parquet);
COPY orders   TO '$DATA/orders.parquet'   (FORMAT parquet);
COPY partsupp TO '$DATA/partsupp.parquet' (FORMAT parquet);

-- dimensions -> Postgres
ATTACH 'dbname=$PGDB host=$PGHOST_DIR' AS pg (TYPE postgres);
CREATE TABLE pg.customer AS SELECT * FROM customer;
CREATE TABLE pg.supplier AS SELECT * FROM supplier;
CREATE TABLE pg.part     AS SELECT * FROM part;

-- reference -> SQLite
ATTACH '$DATA/ref.sqlite' AS ref (TYPE sqlite);
CREATE TABLE ref.nation AS SELECT * FROM nation;
CREATE TABLE ref.region AS SELECT * FROM region;
SQL

echo ">> row counts:"
echo "   facts.lineitem : $(duckdb -noheader -list -c "SELECT count(*) FROM '$DATA/lineitem.parquet'")"
echo "   facts.orders   : $(duckdb -noheader -list -c "SELECT count(*) FROM '$DATA/orders.parquet'")"
echo "   facts.partsupp : $(duckdb -noheader -list -c "SELECT count(*) FROM '$DATA/partsupp.parquet'")"
echo "   pg.customer    : $(psql -d $PGDB -tAc 'SELECT count(*) FROM customer')"
echo "   pg.supplier    : $(psql -d $PGDB -tAc 'SELECT count(*) FROM supplier')"
echo "   pg.part        : $(psql -d $PGDB -tAc 'SELECT count(*) FROM part')"
echo "   ref.nation     : $(duckdb -noheader -list -c "INSTALL sqlite; LOAD sqlite; SELECT count(*) FROM sqlite_scan('$DATA/ref.sqlite','nation')")"
echo "   ref.region     : $(duckdb -noheader -list -c "INSTALL sqlite; LOAD sqlite; SELECT count(*) FROM sqlite_scan('$DATA/ref.sqlite','region')")"
echo ">> done."
