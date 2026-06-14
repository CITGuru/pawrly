# OpenAPI HTTP sources (`config.type: openapi`)

Instead of hand-writing `tables:`, point an `http` source at an OpenAPI 3.0.x spec and Pawrly synthesizes one table per `GET` operation at load time — endpoint, params, columns, rows path, and pagination are read from the document. Full prose: [docs/sources.md → From an OpenAPI spec](https://github.com/CITGuru/pawrly/blob/main/docs/sources.md#from-an-openapi-spec-configtype-openapi). Runnable example: [examples/openapi/pawrly.yaml](https://github.com/CITGuru/pawrly/blob/main/examples/openapi/pawrly.yaml). For auth, retries, rate limiting, and the request/response model these tables inherit, see [http-backend.md](http-backend.md).

## Minimal example

```yaml
- name: stripe
  kind: http
  config:
    type: openapi
    base_url: https://raw.githubusercontent.com/stripe/openapi/refs/heads/master/latest/openapi.spec3.yaml
    auth:
      type: header
      headers:
        - { name: Authorization, bearer: ${secret:STRIPE_API_KEY} }
    openapi:
      include: { paths: ["/v1/charges*", "/v1/customers*"] }   # default: every GET
      cache: { ttl: 24h }
```

```sql
SELECT id, amount, currency, status FROM stripe.get_charges LIMIT 10
```

`base_url` here is the **spec** location (an `http(s)://` URL or a `file://` path) — the real request base comes from the spec's own `servers`. Override it with `config.openapi.base_url` only when the spec declares no usable `servers[0].url`.

## What gets synthesized

- One table per **read-only `GET`** operation; non-GET operations are never exposed.
- Endpoint, params, response rows-path, and pagination are inferred from the document. Pagination inference covers page, offset, cursor, and last-row-cursor styles.
- Where inference is uncertain (e.g. a polymorphic response), the column degrades to a `json` column and a diagnostic is logged rather than failing the source.
- Source-level `safety.max_pages` caps the pagination loop for synthesized tables — they inherit no per-table `safety`.

## `config` (top-level, shared with hand-declared HTTP)

| Key | Required | Notes |
|---|---|---|
| `type` | yes | `openapi` enables synthesis. Absent or `manual` keeps hand-declared behaviour. |
| `base_url` | yes | The **spec** location (`http(s)://` or `file://`). API base comes from the spec `servers`. |
| `auth` | no | `header` / `basic` / `custom` / `oauth2` — see [http-backend.md](http-backend.md). |
| `token` | no | Bearer shorthand → `Authorization: Bearer <token>`. |
| `headers` | no | Static headers attached to every call. |
| `retry` | no | `{ max_retries, base_backoff_ms, max_backoff_ms }`. |
| `rate_limit` | no | `{ requests_per_second, remaining_header, reset_header, extra_statuses }`. |

## `config.openapi` (synthesis-specific)

| Key | Notes |
|---|---|
| `include` | `{ tags: [...], paths: [globs], operations: [...] }` — only matching `GET`s become tables. A `*` glob matches path segments. Omit to register **every** `GET`. |
| `exclude` | Same shape as `include`; a match is dropped (wins over `include`). |
| `naming` | Table naming: `operationId` (default) \| `path` (segments joined) \| `tag` (`<tag>_<leaf>`). Collisions get a numeric suffix. |
| `base_url` | Override for the effective **request** base, used only when the spec declares no usable `servers[0].url`. |
| `cache` | `{ ttl: <duration> }` — cache the fetched spec on disk and reuse it while fresh (e.g. `24h`, `30m`). Omit to re-fetch on every load. Applies to `http(s)://` specs only. |

## Patching a synthesized table

A `tables:` entry whose `name` **matches** a synthesized table patches it — only the fields you set are merged in, the rest of the synthesis is kept. A name that matches nothing is a full new table definition. So fixing one field doesn't mean re-declaring the endpoint and every column:

```yaml
tables:
  - name: get_charges
    response: { path: "$.data" }    # patch the rows-path; keep the synthesized columns
  - name: get_events
    pagination: null                 # drop the inferred pagination
```

Fields merge per key; arrays (`response.schema`, `params`) and type-tagged blocks (`pagination`) replace wholesale, and a `null` clears a field.

## Spec caching

With `openapi.cache` set, the spec is stored under `$PAWRLY_HOME/cache/openapi/` (default `~/.pawrly`) keyed by URL and reused while fresh. Without it, the document is fetched on every load — or point `base_url` at a vendored `file://` copy to avoid the fetch entirely.

## Validation

```bash
pawrly validate
pawrly schema <name>                  # list the synthesized tables
pawrly schema <name>.<table>          # confirm inferred columns + types
pawrly check --source <name>          # runs the source's examples:
```
