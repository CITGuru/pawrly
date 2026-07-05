"""Runtime smoke for the mutating methods, driven against a *disposable copy* of
the `examples/semantic` workspace (set via PAWRLY_WS). Picks the transport from
the env: PAWRLY_GRPC (gRPC) or PAWRLY_REST (REST)."""

import os
import sys
import time

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "src"))

import pawrly  # noqa: E402


def fail(msg: str) -> None:
    print("MUT SMOKE FAIL:", msg)
    sys.exit(1)


ws = os.environ.get("PAWRLY_WS")
if not ws:
    fail("PAWRLY_WS (workspace dir) not set")

if os.environ.get("PAWRLY_GRPC"):
    client = pawrly.PawrlyClient.grpc(os.environ["PAWRLY_GRPC"])
elif os.environ.get("PAWRLY_REST"):
    client = pawrly.PawrlyClient.rest(os.environ["PAWRLY_REST"])
else:
    fail("set PAWRLY_GRPC or PAWRLY_REST")

deadline = time.time() + 10
while True:
    try:
        if client.health().ok:
            break
    except Exception:
        pass
    if time.time() > deadline:
        fail("engine never came up")
    time.sleep(0.1)

print("transport:", client.transport)

# --- reports that always succeed --------------------------------------------
print("reload_config:", client.reload_config())
print("refresh_catalog:", client.refresh_catalog())
print("vacuum_cache:", client.vacuum_cache())

test = client.test_source("data")
print("test_source(data):", test)
if not test.ok:
    fail(f"test_source not ok: {test}")

# --- source lifecycle: add -> list -> remove --------------------------------
info = client.add_source(
    {"name": "extra", "kind": "file", "config": {"path": f"{ws}/data/*.csv"}}
)
print("add_source:", info)
if info.name != "extra":
    fail(f"add_source returned {info}")
if "extra" not in {s.name for s in client.list_sources()}:
    fail("added source not listed")
if client.remove_source("extra") is not True:
    fail("remove_source did not return True")
if "extra" in {s.name for s in client.list_sources()}:
    fail("removed source still listed")
print("source add/remove ok")

# --- materialized lifecycle: materialize -> drop ----------------------------
client.materialize("mut_test", pawrly.MaterializeSpec(sql="SELECT 1 AS n"))
if client.drop_materialized("mut_test") is not True:
    fail("drop_materialized did not return True")
print("materialize/drop ok")

# --- cache-entry ops against a real cache entry -----------------------------
# `customers` has no RLS and a pre-aggregated rollup; this query builds it.
client.semantic_query(
    pawrly.SemanticQuery(
        measures=["customers.customer_total"], dimensions=["customers.region"]
    )
).collect()
entries = client.cache_entries()
if not entries:
    fail("expected a cache entry after the rollup query")
target = f"{entries[0].name.schema}.{entries[0].name.table}"
ro = client.refresh_table(target)
print("refresh_table:", ro)
if ro.rows_written < 0:
    fail("refresh_table bad rows_written")
inv = client.invalidate_cache(target)
print("invalidate_cache:", inv)
if inv is not True:
    fail("invalidate_cache did not return True")

print("MUT SMOKE OK")
