"""Runtime smoke for in-process (managed subprocess) mode. `local()` spawns and
owns a `pawrly console` child — no external orchestration needed.

    PYTHONPATH=src PAWRLY_BIN=../../target/debug/pawrly python3 test/local_smoke.py
"""

import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "src"))

import pawrly  # noqa: E402

binary = os.environ.get("PAWRLY_BIN", "pawrly")

with pawrly.PawrlyClient.local(binary=binary) as c:
    assert c.transport == "local", c.transport
    res = c.query("SELECT 1 AS n").collect()
    assert res.row_count == 1 and res.rows[0]["n"] == 1, res
    out = c.materialize("local_mat", pawrly.MaterializeSpec(sql="SELECT 1 AS n"))
    assert out.name["table"] == "local_mat", out
    back = c.query("SELECT * FROM materialized.local_mat").collect()
    assert back.row_count == 1, back

print("LOCAL SMOKE OK")
