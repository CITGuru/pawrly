# Client SDKs

First-party clients let application code embed a Pawrly workspace the way it embeds a database driver — construct a client, call typed methods. Like the [REST API](./api.md) and [MCP server](./mcp.md), they're **frontends**: they run the same engine, so they see exactly the data, caching, and [semantic models](./semantic.md) you do.

Two clients ship today — **TypeScript** (`@pawrly/client`) and **Python** (`pawrly`) — and both expose the full engine surface identically over three transports.

> Not yet published to npm / PyPI; use them from the repo (`clients/typescript`, `clients/python`).

## Transports

Pick a transport at construction; every method after that is the same regardless of wire.

| Transport | Attach to | Use it for |
|---|---|---|
| **gRPC** | `pawrly serve` | Highest fidelity — typed Arrow, streaming, query cancel ids |
| **REST** | `pawrly console` | Plain JSON over HTTP; firewall / browser-friendly |
| **in-process** | — (spawns its own) | Zero infra — the client spawns and owns a `pawrly console` child |

## TypeScript

```ts
import { PawrlyClient } from "@pawrly/client";

const client = new PawrlyClient({ transport: "grpc", endpoint: "tcp://127.0.0.1:8787" });
// or: { transport: "rest", baseUrl: "http://127.0.0.1:8787" }
// or: await PawrlyClient.local()  // spawns its own `pawrly console`

const res = await (await client.query("select status, count(*) n from data.orders group by status")).collect();
console.log(res.columns, res.rows);

for await (const row of await client.query("select * from data.orders")) handle(row);  // streaming

client.close();
```

## Python

```python
from pawrly import PawrlyClient

client = PawrlyClient.grpc("tcp://127.0.0.1:8787")
# or: PawrlyClient.rest("http://127.0.0.1:8787")
# or: with PawrlyClient.local() as client: ...   # spawns its own `pawrly console`

res = client.query("select status, count(*) n from data.orders group by status").collect()
print(res.columns, res.rows)

for row in client.query("select * from data.orders"):  # streaming
    handle(row)
```

The Python gRPC transport is opt-in (`pip install pawrly[grpc]`); REST and in-process need only `requests`.

## What they cover

The full `EngineService` surface — queries and [semantic](./semantic.md) queries, [materialized tables](./materialize.md), catalog/table/function introspection, [cache](./config.md#caching) management, and source/config management — with identical result shapes across transports. Every failure is a `PawrlyError` carrying the same stable `PAWRLY_*` code as the [REST API](./api.md#errors) and CLI.

See the per-client READMEs for the complete method reference, error handling, and build steps:

- TypeScript — `clients/typescript/README.md`
- Python — `clients/python/README.md`
