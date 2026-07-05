"""Runtime smoke for the read-only methods, driven against the `examples/semantic`
workspace (a file source + two semantic models). Picks the transport from the
env: `PAWRLY_GRPC` (gRPC) or `PAWRLY_REST` (REST)."""

import os
import sys
import time

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "src"))

import pawrly  # noqa: E402


def fail(msg: str) -> None:
    print("READ SMOKE FAIL:", msg)
    sys.exit(1)


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

sources = client.list_sources()
print("list_sources:", [(s.name, s.kind, s.status, s.table_count) for s in sources])
if not (len(sources) == 1 and sources[0].kind == "file" and sources[0].status == "ok"):
    fail(f"unexpected sources {sources}")

tables = client.list_tables()
names = {f"{t.name.schema}.{t.name.table}" for t in tables}
print("list_tables:", sorted(names))
if names != {"data.orders", "data.customers"}:
    fail(f"unexpected tables {names}")

desc = client.describe_table("data.orders")
print("describe_table columns:", [(c.name, c.data_type) for c in desc.columns][:3], "...")
if not desc.columns:
    fail("describe_table returned no columns")

snapshot = client.schema_snapshot()
schemas = {s.name for s in snapshot.schemas}
print("schema_snapshot schemas:", sorted(schemas))
if "data" not in schemas:
    fail(f"schema_snapshot missing `data`: {schemas}")

entries = client.cache_entries()
print("cache_entries:", [(e.name.table, e.mode, e.row_count) for e in entries])

functions = client.list_functions()
print("list_functions count:", len(functions))
if not functions:
    fail("no functions")
fd = client.describe_function(functions[0].namespace, functions[0].name)
print("describe_function:", fd.signature, "| args:", [(a.name, a.type) for a in fd.args])
if fd.name != functions[0].name:
    fail("describe_function name mismatch")

models = client.list_semantic_models()
print("list_semantic_models:", [(m.name, m.dimension_count, m.measure_count) for m in models])
if {m.name for m in models} != {"orders", "customers"}:
    fail(f"unexpected models {models}")

md = client.describe_semantic_model("orders")
print("describe_semantic_model(orders):")
print("  dimensions:", [(d.name, d.type, d.grains) for d in md.dimensions])
print("  measures:", [(x.name, x.agg) for x in md.measures])
print("  relationships:", [(r.name, r.kind, r.target) for r in md.relationships])
if md.name != "orders" or not md.dimensions or not md.measures:
    fail(f"describe_semantic_model bad: {md}")
if not any(d.type == "time" and d.grains for d in md.dimensions):
    fail("expected a time dimension with grains")

print("READ SMOKE OK")
