// Runtime smoke test: drive the built REST transport against a live
// `pawrly console`. Run via `pnpm smoke` (needs the built binary + SDK).
import { PawrlyClient, PawrlyError, UnsupportedByTransport } from "../dist/index.js";

const port = process.env.PAWRLY_PORT;
if (!port) {
  console.error("PAWRLY_PORT not set");
  process.exit(1);
}

const client = new PawrlyClient({
  transport: "rest",
  baseUrl: `http://127.0.0.1:${port}`,
});

function fail(msg) {
  console.error("SMOKE FAIL:", msg);
  process.exit(1);
}

// Wait for the console to come up.
const deadline = Date.now() + 10_000;
for (;;) {
  try {
    if ((await client.health()).ok) break;
  } catch {
    /* not up yet */
  }
  if (Date.now() > deadline) fail("console never came up");
  await new Promise((r) => setTimeout(r, 100));
}

const h = await client.health();
console.log("health:", JSON.stringify(h));
if (!h.ok) fail("health not ok");

// query() -> streaming QueryHandle, then collect().
const res = await (await client.query("SELECT 1 AS n")).collect();
console.log("query:", JSON.stringify(res));
if (res.rowCount !== 1) fail(`expected rowCount 1, got ${res.rowCount}`);
if (res.columns[0] !== "n") fail(`expected column n, got ${res.columns}`);
if (res.rows[0]?.n !== 1) fail(`expected n=1, got ${JSON.stringify(res.rows[0])}`);

// Async iteration (NDJSON streaming under the hood).
let streamed = 0;
for await (const row of await client.query("SELECT 1 AS n")) {
  streamed += 1;
  if (row.n !== 1) fail(`stream row n=${JSON.stringify(row)}`);
}
if (streamed !== 1) fail(`expected 1 streamed row, got ${streamed}`);
console.log("streamed rows:", streamed);

// materialize a query result, then read it back.
const out = await client.materialize("smoke_mat", {
  kind: "query",
  sql: "SELECT 1 AS n",
});
console.log("materialize:", JSON.stringify(out));
if (out.name.table !== "smoke_mat") fail(`materialize name ${JSON.stringify(out.name)}`);
const back = await (await client.query("SELECT * FROM materialized.smoke_mat")).collect();
if (back.rowCount !== 1) fail(`materialized readback rowCount ${back.rowCount}`);
console.log("materialized readback rowCount:", back.rowCount);

// semanticQuery reaches /v1/query; an empty workspace (no models) errors —
// which proves the endpoint + request serialization + error mapping.
try {
  await client.semanticQuery({ measures: ["orders.revenue"] });
  fail("semanticQuery should have errored (no models)");
} catch (e) {
  if (!(e instanceof PawrlyError)) fail(`semanticQuery wrong error: ${e}`);
  console.log("semanticQuery -> PawrlyError:", e.code);
}

// shutdown is unsupported over REST.
try {
  await client.shutdown();
  fail("shutdown should have thrown");
} catch (e) {
  if (!(e instanceof UnsupportedByTransport) || e.code !== "PAWRLY_UNSUPPORTED") {
    fail(`wrong shutdown error: ${e}`);
  }
  console.log("shutdown -> UnsupportedByTransport (PAWRLY_UNSUPPORTED)");
}

console.log("SMOKE OK");
