# HTTP backend (`kind: http`)

How to shape a REST/GraphQL API into Pawrly tables by hand: request, auth, response, and pagination. To synthesize tables from an OpenAPI spec instead, see [openapi.md](openapi.md). Full prose: [docs/sources.md → Http backend](https://github.com/CITGuru/pawrly/blob/main/docs/sources.md#http-backend-http--rest--graphql-apis). Runnable examples: [cache-over-API](https://github.com/CITGuru/pawrly/blob/main/examples/cache-http/pawrly.yaml), [OpenAPI](https://github.com/CITGuru/pawrly/blob/main/examples/openapi/pawrly.yaml).

Build one collection endpoint with a few columns first; validate and query before expanding.

## Source-level `config`

| Key | Notes |
|---|---|
| `base_url` | **required** — joined with each table's `endpoint`. In OpenAPI mode this is the *spec* URL. |
| `token` | bearer shorthand → `Authorization: Bearer <token>`. |
| `auth` | full auth block (below); wins over `token` if both set. |
| `headers` | constant headers on every request (e.g. `Accept`, API-version pin). |
| `retry` | `{ max_retries(3), base_backoff_ms(200), max_backoff_ms(5000) }`; honors `Retry-After`. |
| `rate_limit` | `{ requests_per_second, remaining_header, reset_header, extra_statuses }`. |

## Authentication

Keep credential storage (`secrets:`) separate from where it's sent (`auth`/headers). A **malformed `auth:` block falls back to no auth** — if you get 401/403, check its shape first.

```yaml
# header — bearer tokens AND api keys live here; multiple entries allowed
auth:
  type: header
  headers:
    - { name: Authorization, bearer: "${secret:TOKEN}" }   # → "Bearer …"
    - { name: X-Api-Key,     value:  "${secret:API_KEY}" } # literal
# basic — base64 username:password
auth: { type: basic, username: "${secret:USER}", password: "${secret:PASS}" }
# custom — creds outside headers: query params and/or injected body fields
auth:
  type: custom
  query: [{ name: api_key, value: "${secret:API_KEY}" }]
  body:  [{ name: tenant,  value: acme }]
# oauth2 — client-credentials grant; token fetched, cached, refreshed, sent as Bearer
auth:
  type: oauth2
  token_url: https://login.example.com/oauth/token
  client_id: ${secret:CLIENT_ID}
  client_secret: ${secret:CLIENT_SECRET}
  scope: read:data        # optional; also: audience
```

## Request (per-table, flat fields)

- **`endpoint`** (required) — appended to `base_url`; may carry `{param}` path placeholders and a query string. A param matching a `{placeholder}` fills the path; remaining params become query params (unless consumed by a body `template`).
- **`method`** — defaults `GET`.
- **`headers`** — per-table; merged on top of `config.headers`, wins on collision.
- **`body`** — POST/PUT/GraphQL: `kind` (`json` default, or `form`) + `template` with `{param}` placeholders (other braces left intact).
- **`requests`** — conditional shapes tried in order; first whose `when_filters` are all bound replaces `endpoint`/`method`/`body` (e.g. get-by-id when an id filter is present, else list).

```yaml
- name: search
  endpoint: /graphql
  method: POST
  params: [{ name: q, required: true }]
  body:
    kind: json
    template: '{"query": "{ search(q: \"{q}\") { id name } }"}'
  response:
    path: $.data.search
    schema:
      - { name: id,   type: varchar }
      - { name: name, type: varchar }
```

## Params (the columns a table accepts as filters)

Each: `{ name, type, required, default, accepts, emit, explode, derive }`.

- `required: true` — must appear as a SQL filter or the scan fails (no unbounded fetch).
- Equality pushes down by default (`WHERE state = 'open'` → `?state=open`).
- **Comparisons** — list operators in `accepts`, map each to a query param in `emit`:
  ```yaml
  - { name: created, accepts: [">=", "<="], emit: { ">=": since, "<=": until } }
  ```
- **`explode: true`** — push `IN (a,b,c)` down as repeated pairs `?key=a&key=b&key=c`.
- **`derive`** — dynamic default: `{ kind: ago, seconds: 3600 }` (epoch `now-N`), or `{ kind: split, from: <param>, separator: "-", part: 0 }`.

## Response

- **`path`** — JSONPath to the rows array. `$` (default) = body is the array; `$.data` digs into a wrapper.
- **`reshape`** — turn a non-array payload into rows first: `{ kind: dict_entries }` (object → rows, key on `$._key`) or `{ kind: series_points, series, points, timestamp, value }`.
- **`schema`** — columns `{ name, type, source?, expr? }`. `source` defaults to the row's same-named field; set `$.nested.path`, `$` (whole element, with `type: json`), or `param` (inject a request param as a column). Use `expr` for computed columns a single path can't express (`coalesce`, `map_join`, `to_timestamp`, `from_base64`, `lookup`, … — see [docs/sources.md → Computed columns](https://github.com/CITGuru/pawrly/blob/main/docs/sources.md#computed-columns)). A missing path yields `NULL`, never a scan error.
- **`allow_404_empty: true`** — treat 404 as an empty result.
- **`error`** — surface failures: `status` (`[">=400"]`, `"5xx"`) and/or `path` (JSONPath to an error message inside a 200-with-error body).

```yaml
response:
  path: $.data
  allow_404_empty: true
  schema:
    - { name: id,      type: bigint }
    - { name: author,  type: varchar, source: $.user.login }
    - { name: payload, type: json,    source: $ }
    - { name: repo,    type: varchar, source: param }
  error: { status: [">=400"], path: $.message }
```

## Pagination

Absent = single request. A SQL `LIMIT` stops paging early; `safety.max_pages` caps the loop. Verify with real row fetches across pages, not `COUNT(*)`.

| `type` | Fields | Behaviour |
|---|---|---|
| `link_header` | — | follows RFC 5988 `Link: rel="next"`. |
| `cursor` | `next_path`, `param` | reads a body cursor, echoes it as a query param. |
| `body_cursor` | `cursor_path`, `next_path` | reads next cursor, writes into next request's JSON body (GraphQL `variables.after`, Notion). |
| `row_cursor` | `param`, `field`(=`id`), `more_path?` | sends last row's `field` (Stripe `starting_after`). |
| `page` | `param`, `start`(1), `size_param?`, `size?` | increments page number until an empty page. |
| `offset` | `param`, `size_param`, `size` | increments offset until a short page. |

## Raw table escape hatch

`raw_table: true` registers an **unqualified** table named after the source for endpoints with no typed spec. Query it as `FROM <source>` (not `<source>.<table>`); provide the request via filters. Columns: `request_method`, `request_path` (filter **required**), `request_query`, `response_status`, `response_body`. `raw_table_safety` overrides the default filter requirement.

## Validation loop

```bash
pawrly validate
pawrly check --source <name>          # runs the source's examples:
pawrly schema <name>.<table>
pawrly sql "SELECT ... FROM <name>.<table> WHERE <required filters> LIMIT 5"
```
