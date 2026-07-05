// Runtime smoke for the read-only methods, driven against the `examples/semantic`
// workspace (a file source + two semantic models). Picks the transport from the
// env: PAWRLY_GRPC (gRPC) or PAWRLY_REST (REST).
import { PawrlyClient } from "../dist/index.js";

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
  console.error("READ SMOKE FAIL:", msg);
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

const sources = await client.listSources();
console.log("listSources:", sources.map((s) => [s.name, s.kind, s.status, s.tableCount]));
if (!(sources.length === 1 && sources[0].kind === "file" && sources[0].status === "ok")) {
  fail(`unexpected sources ${JSON.stringify(sources)}`);
}

const tables = await client.listTables();
const names = new Set(tables.map((t) => `${t.name.schema}.${t.name.table}`));
console.log("listTables:", [...names].sort());
if (names.size !== 2 || !names.has("data.orders") || !names.has("data.customers")) {
  fail(`unexpected tables ${[...names]}`);
}

const desc = await client.describeTable("data.orders");
console.log("describeTable columns:", desc.columns.slice(0, 3).map((c) => [c.name, c.dataType]), "...");
if (desc.columns.length === 0) fail("describeTable returned no columns");

const snapshot = await client.schemaSnapshot();
const schemas = new Set(snapshot.schemas.map((s) => s.name));
console.log("schemaSnapshot schemas:", [...schemas].sort());
if (!schemas.has("data")) fail(`schemaSnapshot missing data: ${[...schemas]}`);

const entries = await client.cacheEntries();
console.log("cacheEntries:", entries.map((e) => [e.name.table, e.mode, e.rowCount]));

const functions = await client.listFunctions();
console.log("listFunctions count:", functions.length);
if (functions.length === 0) fail("no functions");
const fd = await client.describeFunction(functions[0].namespace, functions[0].name);
console.log("describeFunction:", fd.signature, "| args:", fd.args.map((a) => [a.name, a.type]));
if (fd.name !== functions[0].name) fail("describeFunction name mismatch");

const models = await client.listSemanticModels();
console.log("listSemanticModels:", models.map((m) => [m.name, m.dimensionCount, m.measureCount]));
const modelNames = new Set(models.map((m) => m.name));
if (modelNames.size !== 2 || !modelNames.has("orders") || !modelNames.has("customers")) {
  fail(`unexpected models ${[...modelNames]}`);
}

const md = await client.describeSemanticModel("orders");
console.log("describeSemanticModel(orders):");
console.log("  dimensions:", md.dimensions.map((d) => [d.name, d.type, d.grains]));
console.log("  measures:", md.measures.map((x) => [x.name, x.agg]));
console.log("  relationships:", md.relationships.map((r) => [r.name, r.kind, r.target]));
if (md.name !== "orders" || md.dimensions.length === 0 || md.measures.length === 0) {
  fail(`describeSemanticModel bad: ${JSON.stringify(md)}`);
}
if (!md.dimensions.some((d) => d.type === "time" && d.grains.length > 0)) {
  fail("expected a time dimension with grains");
}

client.close();
console.log("READ SMOKE OK");
