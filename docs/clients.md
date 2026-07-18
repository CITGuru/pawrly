# Client SDKs

Use the **TypeScript** (`@pawrly/client`) or **Python** (`pawrly`) SDK to run Pawrly queries and management operations from application code. Both expose the same method set and support gRPC, REST, and managed local modes.

> Not yet published to npm / PyPI; use them from the repo (`clients/typescript`, `clients/python`).

## Transports

In the SDKs, a transport is how the client reaches Pawrly. Choose one when constructing the client:

| Transport | Attach to | Use it for |
|---|---|---|
| **gRPC** | An existing `pawrly serve` daemon | Typed Arrow results, streaming, and query cancellation |
| **REST** | An existing `pawrly console` server | Plain JSON over HTTP with fewer client dependencies |
| **local** | A managed child process | Development, tests, or applications that do not want to manage a daemon |

Despite sometimes being called “in-process,” local mode runs Pawrly in a separate child process. The client starts `pawrly console` on a private loopback port, connects over REST, and stops the child when the client closes.

For the examples below, native gRPC and REST use different listeners:

```bash
pawrly serve --addr tcp://127.0.0.1:8788   # native gRPC
pawrly console --addr 127.0.0.1:8787       # REST, gRPC-Web, and Console
```

## TypeScript

```ts
import { PawrlyClient } from "@pawrly/client";

const client = new PawrlyClient({ transport: "grpc", endpoint: "tcp://127.0.0.1:8788" });
// or: { transport: "rest", baseUrl: "http://127.0.0.1:8787" }
// or: await PawrlyClient.local()  // manages a private `pawrly console` child

const res = await (await client.query("select status, count(*) n from data.orders group by status")).collect();
console.log(res.columns, res.rows);

for await (const row of await client.query("select * from data.orders")) handle(row);  // streaming

client.close();
```

## Python

```python
from pawrly import PawrlyClient

client = PawrlyClient.grpc("tcp://127.0.0.1:8788")
# or: PawrlyClient.rest("http://127.0.0.1:8787")
# or: with PawrlyClient.local() as client: ...   # manages a private `pawrly console` child

res = client.query("select status, count(*) n from data.orders group by status").collect()
print(res.columns, res.rows)

for row in client.query("select * from data.orders"):  # streaming
    handle(row)
```

The Python gRPC transport is opt-in (`pip install pawrly[grpc]`); REST and local mode need only `requests`.

## Supported operations

Both clients support:

- SQL and [semantic](./semantic.md) queries
- [Materialized tables](./materialize.md)
- Catalog, table, and function introspection
- [Cache](./config.md#caching) management
- Source and config management

Transport-specific behavior:

- gRPC provides typed Arrow streaming and server query IDs for cancellation.
- REST and local mode return JSON-shaped results.

Failures are returned as `PawrlyError` values with `PAWRLY_*` codes documented by the [REST API](./api.md#errors).

See the per-client READMEs for the complete method reference, error handling, and build steps:

- TypeScript — `clients/typescript/README.md`
- Python — `clients/python/README.md`
