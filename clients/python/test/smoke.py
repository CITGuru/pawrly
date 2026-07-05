"""Runtime smoke: drive the REST transport against a live `pawrly console`."""

import os
import sys
import time

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "src"))

import pawrly  # noqa: E402


def fail(msg: str) -> None:
    print("SMOKE FAIL:", msg)
    sys.exit(1)


port = os.environ.get("PAWRLY_PORT")
if not port:
    fail("PAWRLY_PORT not set")

c = pawrly.PawrlyClient.rest(f"http://127.0.0.1:{port}")

deadline = time.time() + 10
while True:
    try:
        if c.health().ok:
            break
    except Exception:
        pass
    if time.time() > deadline:
        fail("console never came up")
    time.sleep(0.1)

h = c.health()
print("health:", h)
if not h.ok:
    fail("health not ok")

# query() -> streaming QueryHandle, then collect().
res = c.query("SELECT 1 AS n").collect()
print("query:", res)
if res.row_count != 1 or res.rows[0].get("n") != 1:
    fail(f"unexpected query result {res}")

# streaming iteration (NDJSON under the hood).
streamed = list(c.query("SELECT 1 AS n"))
if len(streamed) != 1:
    fail(f"expected 1 streamed row, got {len(streamed)}")
print("streamed rows:", len(streamed))

# materialize a query result, then read it back.
out = c.materialize("smoke_mat_py", pawrly.MaterializeSpec(sql="SELECT 1 AS n"))
print("materialize:", out)
if out.name.get("table") != "smoke_mat_py":
    fail(f"materialize name {out.name}")
back = c.query("SELECT * FROM materialized.smoke_mat_py").collect()
if back.row_count != 1:
    fail(f"materialized readback row_count {back.row_count}")
print("materialized readback row_count:", back.row_count)

# semantic_query reaches /v1/query; an empty workspace (no models) errors —
# which proves the endpoint + request serialization + error mapping.
try:
    c.semantic_query(pawrly.SemanticQuery(measures=["orders.revenue"]))
    fail("semantic_query should have errored (no models)")
except pawrly.PawrlyError as e:
    print("semantic_query -> PawrlyError:", e.code)

# shutdown is unsupported over REST.
try:
    c.shutdown()
    fail("shutdown should have raised")
except pawrly.UnsupportedByTransport as e:
    if e.code != "PAWRLY_UNSUPPORTED":
        fail(f"wrong shutdown code {e.code}")
    print("shutdown -> UnsupportedByTransport (PAWRLY_UNSUPPORTED)")

print("PY SMOKE OK")
