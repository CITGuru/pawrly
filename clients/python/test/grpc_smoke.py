"""Runtime smoke: drive the gRPC transport against a live `pawrly serve`.

Exercises the gRPC-only guarantees the REST smoke can't: a non-empty server
query id, lossless typed Arrow values, and a real `shutdown`.
"""

import os
import sys
import time

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "src"))

import pawrly  # noqa: E402


def fail(msg: str) -> None:
    print("GRPC SMOKE FAIL:", msg)
    sys.exit(1)


endpoint = os.environ.get("PAWRLY_GRPC")
if not endpoint:
    fail("PAWRLY_GRPC not set (e.g. tcp://127.0.0.1:8788)")

c = pawrly.PawrlyClient.grpc(endpoint)

deadline = time.time() + 10
while True:
    try:
        if c.health().ok:
            break
    except Exception:
        pass
    if time.time() > deadline:
        fail("serve never came up")
    time.sleep(0.1)

h = c.health()
print("health:", h)
if not h.ok:
    fail("health not ok")

# query() streams Arrow IPC frames; values stay typed (n is an int, not "1").
handle = c.query("SELECT 1 AS n")
if not handle.id:
    fail("gRPC query handle has no server id (Started frame missing)")
print("query id:", handle.id)
res = handle.collect()
print("query:", res)
if res.row_count != 1 or res.rows[0].get("n") != 1:
    fail(f"unexpected query result {res}")
if not isinstance(res.rows[0]["n"], int):
    fail(f"expected typed int, got {type(res.rows[0]['n'])}")

# streaming iteration (Arrow batches under the hood).
streamed = list(c.query("SELECT 1 AS n"))
if len(streamed) != 1:
    fail(f"expected 1 streamed row, got {len(streamed)}")
print("streamed rows:", len(streamed))

# explain returns a plan string.
plan = c.explain("SELECT 1 AS n", False)
if "1" not in plan:
    fail(f"explain plan looks wrong: {plan!r}")
print("explain ok (", len(plan), "chars )")

# materialize a query result, then read it back.
out = c.materialize("grpc_mat_py", pawrly.MaterializeSpec(sql="SELECT 1 AS n"))
print("materialize:", out)
if out.name.get("table") != "grpc_mat_py":
    fail(f"materialize name {out.name}")
back = c.query("SELECT * FROM materialized.grpc_mat_py").collect()
if back.row_count != 1:
    fail(f"materialized readback row_count {back.row_count}")
print("materialized readback row_count:", back.row_count)

# cancel round-trips a bool; an already-finished id is simply False (no raise).
cancelled = c.cancel("does-not-exist")
if not isinstance(cancelled, bool):
    fail(f"cancel returned {cancelled!r}, expected bool")
print("cancel(unknown) ->", cancelled)

# semantic_query on an empty workspace (no models) surfaces a PawrlyError.
try:
    c.semantic_query(pawrly.SemanticQuery(measures=["orders.revenue"])).collect()
    fail("semantic_query should have errored (no models)")
except pawrly.PawrlyError as e:
    print("semantic_query -> PawrlyError:", e.code)

# shutdown is real over gRPC (do it last — it stops the server).
c.shutdown()
print("shutdown -> ok")

print("GRPC SMOKE OK")
