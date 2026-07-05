// Runtime smoke for in-process (managed subprocess) mode. `local()` spawns and
// owns a `pawrly console` child — no external orchestration needed.
//   PAWRLY_BIN=../../target/debug/pawrly node test/local_smoke.mjs
import { PawrlyClient } from "../dist/index.js";

const binary = process.env.PAWRLY_BIN ?? "pawrly";
const client = await PawrlyClient.local({ binary });
try {
  if (client.transport !== "local") throw new Error(`transport ${client.transport}`);
  const res = await (await client.query("SELECT 1 AS n")).collect();
  if (res.rowCount !== 1 || res.rows[0]?.n !== 1) {
    throw new Error(`query ${JSON.stringify(res)}`);
  }
  const out = await client.materialize("local_mat_ts", {
    kind: "query",
    sql: "SELECT 1 AS n",
  });
  if (out.name.table !== "local_mat_ts") {
    throw new Error(`materialize ${JSON.stringify(out.name)}`);
  }
  const back = await (
    await client.query("SELECT * FROM materialized.local_mat_ts")
  ).collect();
  if (back.rowCount !== 1) throw new Error(`readback ${back.rowCount}`);
  console.log("LOCAL SMOKE OK");
} finally {
  client.close();
}
