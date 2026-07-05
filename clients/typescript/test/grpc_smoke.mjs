// Runtime smoke: drive the built gRPC transport against a live `pawrly serve`.
// Exercises the gRPC-only guarantees the REST smoke can't: a non-empty server
// query id, lossless typed Arrow values, and a real `shutdown`.
import { PawrlyClient, PawrlyError } from "../dist/index.js";

const endpoint = process.env.PAWRLY_GRPC;
if (!endpoint) {
  console.error("PAWRLY_GRPC not set (e.g. tcp://127.0.0.1:8788)");
  process.exit(1);
}

const client = new PawrlyClient({ transport: "grpc", endpoint });

function fail(msg) {
  console.error("GRPC SMOKE FAIL:", msg);
  process.exit(1);
}

// BigInt-safe stringify (64-bit ints beyond 2^53 stay BigInt).
const j = (x) => JSON.stringify(x, (_, v) => (typeof v === "bigint" ? v.toString() : v));

// Wait for serve to come up.
const deadline = Date.now() + 10_000;
for (;;) {
  try {
    if ((await client.health()).ok) break;
  } catch {
    /* not up yet */
  }
  if (Date.now() > deadline) fail("serve never came up");
  await new Promise((r) => setTimeout(r, 100));
}

const h = await client.health();
console.log("health:", JSON.stringify(h));
if (!h.ok) fail("health not ok");

// query() streams Arrow IPC frames; values stay typed, and the handle carries
// the server-assigned query id (empty over REST).
const handle = await client.query("SELECT 1 AS n");
if (!handle.id) fail("gRPC query handle has no server id (Started frame missing)");
console.log("query id:", handle.id);
const res = await handle.collect();
console.log("query:", j(res));
if (res.rowCount !== 1) fail(`expected rowCount 1, got ${res.rowCount}`);
if (res.rows[0]?.n !== 1) fail(`expected n=1, got ${JSON.stringify(res.rows[0])}`);
if (typeof res.rows[0].n !== "number") {
  fail(`expected typed number, got ${typeof res.rows[0].n}`);
}

// Async iteration (Arrow batches under the hood).
let streamed = 0;
for await (const row of await client.query("SELECT 1 AS n")) {
  streamed += 1;
  if (row.n !== 1) fail(`stream row n=${JSON.stringify(row)}`);
}
if (streamed !== 1) fail(`expected 1 streamed row, got ${streamed}`);
console.log("streamed rows:", streamed);

// explain returns a plan string.
const plan = await client.explain("SELECT 1 AS n", false);
if (!plan.includes("1")) fail(`explain plan looks wrong: ${plan}`);
console.log("explain ok (", plan.length, "chars )");

// materialize a query result, then read it back.
const out = await client.materialize("grpc_mat_ts", { kind: "query", sql: "SELECT 1 AS n" });
console.log("materialize:", j(out));
if (out.name.table !== "grpc_mat_ts") fail(`materialize name ${JSON.stringify(out.name)}`);
const back = await (await client.query("SELECT * FROM materialized.grpc_mat_ts")).collect();
if (back.rowCount !== 1) fail(`materialized readback rowCount ${back.rowCount}`);
console.log("materialized readback rowCount:", back.rowCount);

// cancel round-trips a bool; an already-finished id is simply false (no throw).
const cancelled = await client.cancel("does-not-exist");
if (typeof cancelled !== "boolean") fail(`cancel returned ${cancelled}, expected boolean`);
console.log("cancel(unknown) ->", cancelled);

// semanticQuery on an empty workspace (no models) surfaces a PawrlyError — the
// status-level error path (mapped from a Connect status, not an in-stream frame).
try {
  await client.semanticQuery({ measures: ["orders.revenue"] });
  fail("semanticQuery should have errored (no models)");
} catch (e) {
  if (!(e instanceof PawrlyError)) fail(`semanticQuery wrong error: ${e}`);
  console.log("semanticQuery -> PawrlyError:", e.code);
}

// shutdown is real over gRPC (do it last — it stops the server).
await client.shutdown();
console.log("shutdown -> ok");

client.close();
console.log("GRPC SMOKE OK");
