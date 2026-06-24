# TPC-H — federated benchmark

Runs the canonical 22 [TPC-H](https://www.tpc.org/tpch/) queries through Pawrly, with the 8 tables deliberately spread across **three different backends** so every multi-table query becomes a real federated join:

| Source (`pawrly.yaml`) | Backend  | Tables                                                              |
| ---------------------- | -------- | ------------------------------------------------------------------ |
| `facts`                | parquet  | `lineitem`, `orders`, `partsupp`                                   |
| `pg`                   | Postgres | `customer`, `supplier`, `part`                                     |
| `ref`                  | SQLite   | `nation`, `region`                                                 |
| `world`                | HTTP API | `currency` ([CountriesNow](https://countriesnow.space), no auth)   |
| `fx`                   | HTTP API | `rates` ([Frankfurter](https://frankfurter.dev), live FX, no auth) |

So Q5 (`customer ⋈ orders ⋈ lineitem ⋈ supplier ⋈ nation ⋈ region`) touches all three storage backends in one plan, while Q1 (lineitem-only) stays single-source.

The `world` and `fx` sources are live public HTTP APIs (no auth). They are **not** part of the canonical 22, but power the enrichment queries in `queries_extra/` (see below), which join parquet + Postgres + SQLite + HTTP in a single plan. The `fx` source also exposes a table-valued **function** `fx.rates_for(base)` — the `rates` lookup with the base currency as an argument — used by `queries_extra/fx_rate_via_function.sql` (`SELECT … FROM ref.nation JOIN world.currency … CROSS JOIN fx.rates_for('EUR')`).

## Prerequisites

- `duckdb` and `pawrly` on `PATH`.
- A local Postgres reachable with libpq defaults (the scripts use trust auth on the socket and create a database named `tpch`). Adjust the DSN in `pawrly.yaml` (`dbname=tpch host=/tmp`) and `PGHOST` in `gen.sh` if yours differs.
- Network access for the `world` and `fx` HTTP sources (only needed for the enrichment queries; the canonical 22 are fully local).

## Run it

```bash
./gen.sh 1         # generate TPC-H SF1 and load the three backends (arg = scale factor)
./mkqueries.sh     # dump the 22 queries, qualified to facts/pg/ref → queries/qNN.sql
./bench.sh 3       # time all 22 against a warm `pawrly serve` (arg = iterations)
./validate.sh      # (optional) prove results match native DuckDB on the same data
./bench_extra.sh 3 # (optional) time the live-HTTP enrichment queries in queries_extra/
```

## What each script does

- **`gen.sh`** — runs `CALL dbgen(sf=N)` in DuckDB, then exports the fact tables to parquet, the dimension tables into Postgres, and the reference tables into SQLite. Re-runnable; row counts are printed at the end.
- **`mkqueries.sh`** — pulls the 22 canonical queries from DuckDB's `tpch` extension and qualifies each table reference with its source (`lineitem` → `facts.lineitem`, …). Qualification is applied only in FROM/JOIN positions, so the column alias `nation` in Q8/Q9 is left untouched; unqualified columns still resolve because `facts.lineitem` carries the implicit alias `lineitem`.
- **`bench.sh`** — starts a `pawrly serve` daemon, warms each query once, then times it `ITER` times and reports the min and median wall time (client→daemon roundtrip included). Per-query result CSVs land in `results/`.
- **`validate.sh`** — runs the unqualified queries in native DuckDB over the same SF1 data and diffs the results against `results/` (numerics rounded to 2dp; auto-generated aggregate column labels are ignored). Expect **22/22 PASS**.
- **`bench_extra.sh`** — times the live-HTTP enrichment queries in `queries_extra/` (the 4- and 5-backend joins). Defaults to the local `target/release/pawrly` so it works before you reinstall; override with `PAWRLY_BIN=...`. Timings include live API latency, so they are noisier than the canonical 22 — the bulk is the API round-trip (~250ms for CountriesNow), while the TPC-H join itself is small.

## Sample run (SF1, Apple Silicon, ITER=3)

```
query     rows       min_ms    median_ms
q01          4         43.7         44.7     (lineitem only — facts)
q05          5         75.2         77.6     (3-source join)
q09        175        107.7        127.9     (3-source join)
q18         57        123.8        137.4
...
>> 22 queries | total(min) = 1472 ms | geomean(min) = 60.2 ms | ITER=3
```

Numbers vary by machine; treat them as relative. The point of the harness is a repeatable, *correctness-checked* federated workload, not an absolute score.

## 4-backend enrichment query

`queries_extra/revenue_by_nation_currency.sql` joins TPC-H revenue (parquet + Postgres + SQLite) against each nation's live currency from the `world` HTTP API — all four backends in one Pawrly plan:

```bash
pawrly --no-remote --config pawrly.yaml sql --file queries_extra/revenue_by_nation_currency.sql
```

```
nation      currency  iso3   revenue
FRANCE      EUR       FRA    8960205391.83
INDONESIA   IDR       IDN    8942575217.62
RUSSIA      RUB       RUS    8925318302.07
...
```

All 25 TPC-H nations are real countries, so they join to the API by name.

## 5-backend enrichment query (live FX)

`queries_extra/revenue_local_currency.sql` adds a second HTTP API ([Frankfurter](https://frankfurter.dev)) and converts each nation's revenue from notional USD into its **local currency at live rates** — joining all five backends in one plan:

```bash
pawrly --no-remote --config pawrly.yaml sql --file queries_extra/revenue_local_currency.sql
```

```
nation      currency  revenue_usd     usd_to_local  revenue_local
FRANCE      EUR       8960205391.83   0.86155       7719664955.33
INDONESIA   IDR       8942575217.62   17719.0       158453490281074.3
CHINA       CNY       8809189670.71   6.757         59523694604.96
...
```

Frankfurter covers ~30 major currencies, so 11 of the 25 nations (those with a supported currency) convert. It returns a single JSON *object* of rates rather than an array, so the `fx` source relies on the HTTP source treating a lone object as a one-row result. If you installed `pawrly` from a release, reinstall after building so the global binary picks up that behavior.
