// Runtime smoke for the mutating methods, driven against a *disposable copy* of
// the `examples/semantic` workspace (set via PAWRLY_WS). Picks the transport
// from the env: PAWRLY_GRPC (gRPC) or PAWRLY_REST (REST).
import { PawrlyClient } from "../dist/index.js";

const ws = process.env.PAWRLY_WS;
if (!ws) {
  console.error("PAWRLY_WS (workspace dir) not set");
  process.exit(1);
}

let client;
if (process.env.PAWRLY_GRPC) {
  client = new PawrlyClient({ transport: "grpc", endpoint: process.env.PAWRLY_GRPC });
} else if (process.env.PAWRLY_REST) {
  client = new PawrlyClient({ transport: "rest", baseUrl: process.env.PAWRLY_REST });
} else {
  console.error("set PAWRLY_GRPC or PAWRLY_REST");
  process.exit(1);
}

function fail(msg) {
  console.error("MUT SMOKE FAIL:", msg);
  process.exit(1);
}

const deadline = Date.now() + 10_000;
for (;;) {
  try {
    if ((await client.health()).ok) break;
  } catch {
    /* not up yet */
  }
  if (Date.now() > deadline) fail("engine never came up");
  await new Promise((r) => setTimeout(r, 100));
}

console.log("transport:", client.transport);

// --- reports that always succeed --------------------------------------------
console.log("reloadConfig:", JSON.stringify(await client.reloadConfig()));
console.log("refreshCatalog:", JSON.stringify(await client.refreshCatalog()));
console.log("vacuumCache:", JSON.stringify(await client.vacuumCache()));

const test = await client.testSource("data");
console.log("testSource(data):", JSON.stringify(test));
if (!test.ok) fail(`testSource not ok: ${JSON.stringify(test)}`);

// --- source lifecycle: add -> list -> remove --------------------------------
const info = await client.addSource({ name: "extra", kind: "file", config: { path: `${ws}/data/*.csv` } });
console.log("addSource:", JSON.stringify(info));
if (info.name !== "extra") fail(`addSource returned ${JSON.stringify(info)}`);
if (!(await client.listSources()).some((s) => s.name === "extra")) fail("added source not listed");
if ((await client.removeSource("extra")) !== true) fail("removeSource did not return true");
if ((await client.listSources()).some((s) => s.name === "extra")) fail("removed source still listed");
console.log("source add/remove ok");

// --- materialized lifecycle: materialize -> drop ----------------------------
await client.materialize("mut_test", { kind: "query", sql: "SELECT 1 AS n" });
if ((await client.dropMaterialized("mut_test")) !== true) fail("dropMaterialized did not return true");
console.log("materialize/drop ok");

// --- cache-entry ops against a real cache entry -----------------------------
// `customers` has no RLS and a pre-aggregated rollup; this query builds it.
await (
  await client.semanticQuery({ measures: ["customers.customer_total"], dimensions: ["customers.region"] })
).collect();
const entries = await client.cacheEntries();
if (entries.length === 0) fail("expected a cache entry after the rollup query");
const target = `${entries[0].name.schema}.${entries[0].name.table}`;
const ro = await client.refreshTable(target);
console.log("refreshTable:", JSON.stringify(ro));
if (ro.rowsWritten < 0) fail("refreshTable bad rowsWritten");
const inv = await client.invalidateCache(target);
console.log("invalidateCache:", inv);
if (inv !== true) fail("invalidateCache did not return true");

client.close();
console.log("MUT SMOKE OK");
