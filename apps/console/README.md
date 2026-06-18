# pawrly-console

The browser **Console** for a running Pawrly workspace — a Vite + React single-page app that talks to the daemon over **gRPC-Web** (the same gRPC contract; no separate backend). It's a non-cargo workspace member; its built `dist/` is embedded into `pawrly-server` via `rust-embed` behind the `console` cargo feature.

Read more: [`docs/console.md`](../../docs/console.md).

## Prerequisites

- **pnpm** and **Node ≥ 20**.
- A running Pawrly daemon to talk to (see the dev workflow below).

## Setup

```bash
pnpm install
```

The gRPC-Web client is **generated from the proto** (`crates/pawrly-proto/proto`) with `buf` + `protoc-gen-es` into `src/gen/` (gitignored). It runs automatically as part of `build`, or on demand:

```bash
pnpm run generate
```

Re-run it after changing the proto.

## Dev workflow

`vite dev` serves the SPA on **http://localhost:5173**, and `vite.config.ts` **proxies gRPC-Web calls** (`/pawrly.v1.*`) to a daemon, so dev is same-origin — no CORS flag and no manual endpoint:

```bash
# 1. start a daemon (loopback, no token needed)
pawrly console            # serves gRPC-Web (+ embedded UI) on 127.0.0.1:8787

# 2. start the dev server (in apps/console)
pnpm dev                  # http://localhost:5173, hot reload
```

The proxy targets `http://127.0.0.1:8787` by default; point it elsewhere with:

```bash
PAWRLY_CONSOLE_DAEMON=http://127.0.0.1:9000 pnpm dev
```

> Without the proxy you'd have to run the daemon cross-origin
> (`pawrly serve --console --cors-origin http://localhost:5173`) **and** set the
> sidebar **Endpoint** to the daemon URL. The proxy avoids both.

## Build

```bash
pnpm build                # generate → tsc -b → vite build  →  dist/
pnpm preview              # preview the production build
pnpm typecheck            # tsc -b, no emit
```

`dist/` is what `pawrly-server` embeds. To produce a single binary with the UI bundled:

```bash
# from the repo root, after `pnpm build` here
cargo build --release -p pawrly-cli --features console
```

## Stack

- **Vite** + **React 19** + **TypeScript**.
- **Connect-ES** (`@connectrpc/connect` + `createGrpcWebTransport`) over the generated client; a bearer interceptor adds `Authorization`, and a per-call `traceparent` correlates queries in `system.activity`.
- **apache-arrow** decodes streamed query results (`QueryResponse.ipc_stream`).
- **Tailwind v4** + **shadcn/ui** (vendored in `src/components/ui/`).
