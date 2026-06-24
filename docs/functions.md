# Functions

Pawrly sources expose data as tables (`SELECT * FROM github.pulls`). But many useful operations are call-shaped rather than table-shaped: a search endpoint parameterized by a query string, a lookup that takes an input, a glob over a pattern. **Table-valued functions** model these: named, reusable operations with ordered, typed arguments that return rows.

```sql
SELECT i.title, i.state
FROM github.search_issues('is:open label:bug', 50) AS i
WHERE i.state = 'open';
```

Functions are either **builtin** (shipped with Pawrly) or **declared** in `pawrly.yaml`. A declared function is either *attached* to a source (inheriting its connection and auth) or *standalone* (with its own `namespace`, `kind`, and `config`).

---

## Calling functions from SQL

```sql
-- simple call
SELECT * FROM github.search_issues('is:open label:bug', 50);

-- alias, join, post-filter (WHERE applies on top of the function result)
SELECT i.title
FROM github.search_issues('is:open', 100) AS i
JOIN github.pulls p ON p.number = i.number
WHERE i.user_login <> 'dependabot[bot]';

-- CTEs / subqueries / UNION branches all work
WITH hot AS (SELECT * FROM github.search_issues('label:p0', 20))
SELECT count(*) FROM hot;

-- builtin
SELECT file_name, size_bytes FROM file.glob('./data/*.parquet') ORDER BY 1;
```

Rules:

- Positional arguments only, matched against the declared order. Trailing optional/defaulted arguments may be omitted.
- Arguments must be literals (strings, numbers, booleans, including negatives). Column references and expressions are rejected at plan time, a DataFusion constraint on table-function args. Use the CLI / MCP named-arg forms when you want name=value ergonomics.
- `${param:KEY}` substitution runs before the call is resolved, so parameterized queries can feed function arguments.
- A bare two-part name without parentheses (`FROM github.issues`) is always an ordinary `schema.table` lookup, never a function.

---

## Builtins

| Function | Signature | Returns |
|----------|-----------|---------|
| `file.glob` | `file.glob(pattern varchar)` | One row per matched file: `path`, `file_name`, `size_bytes`, `modified`. Relative patterns resolve against the workspace dir; `~` expands to home. Zero matches → zero rows. |
| `file.grep` | `file.grep(pattern varchar, glob varchar)` | One row per line matching the regex `pattern` across the files matched by `glob`: `path`, `line_number`, `line`. Binary/non-UTF-8 files are skipped. |
| `http.get` | `http.get(url varchar, path varchar = '$')` | Generic GET; `path` is a JSONPath into the response. Each matched element is returned as a JSON string in the single `body` column. |

---

## Declaring functions

### Source-attached (inherits the source's config)

```yaml
sources:
  - name: github
    kind: http
    config:
      base_url: https://api.github.com
      auth:
        type: header
        headers:
          - { name: Authorization, bearer: "${secret:GITHUB_TOKEN}" }
    functions:
      - name: search_issues
        description: Search issues with the GitHub search syntax.
        endpoint: /search/issues          # {arg} placeholders substitute; bare args → ?q=...
        args:
          - { name: q,     type: varchar, required: true }
          - { name: limit, type: int,     default: "50" }
        response:
          path: $.items
        pagination:
          type: page
          param: page
        returns:
          - { name: number,     type: bigint }
          - { name: title,      type: varchar }
          - { name: state,      type: varchar }
          - { name: user_login, type: varchar, source: $.user.login }
          - { name: q,          type: varchar, source: arg }   # echo the bound argument
```

Call it (the namespace is the source name):

```sql
SELECT number, title, user_login
FROM github.search_issues('is:open label:bug repo:org/repo', 100)
WHERE state = 'open';
```

Attached functions omit `namespace`, `kind`, and `config`; all are inherited. They are only valid on `http`, `mcp`, and `file` sources, and may not share a name with a table in the same source.

An MCP function attaches the same way and uses the same tool-mapping vocabulary as a declarative MCP table:

```yaml
sources:
  - name: linear
    kind: mcp
    config:
      transport: streamable_http
      url: https://mcp.linear.app/mcp
      auth:
        type: header
        headers: [{ name: Authorization, bearer: "${secret:LINEAR_TOKEN}" }]
    functions:
      - name: search_issues
        tool: search                       # MCP tool to invoke
        tool_args: { state: open }         # static tool arguments
        args:
          - { name: q,     type: varchar, required: true, tool_arg: query }
          - { name: limit, type: int,     default: "25" }
        rows_path: [issues]
        pagination: { cursor_arg: cursor, response_cursor_path: [nextCursor] }
        returns:
          - { name: key,   type: varchar }
          - { name: title, type: varchar, source: $.fields.title }
```

Call it (the `q` argument is sent to the tool as `query`):

```sql
SELECT key, title FROM linear.search_issues('priority:high', 25);
```

### Standalone (explicit namespace + config)

```yaml
functions:
  - name: geocode
    namespace: geo
    kind: http
    config:
      base_url: https://nominatim.openstreetmap.org
    endpoint: /search?format=json&q={address}
    args:
      - { name: address, type: varchar, required: true }
    returns:
      - { name: lat,          type: double,  source: $.lat }
      - { name: lon,          type: double,  source: $.lon }
      - { name: display_name, type: varchar }

  - name: logs
    namespace: ops
    kind: file
    path: ./logs/{service}/*.jsonl
    args:
      - { name: service, type: varchar, required: true }
    returns:
      - { name: path,       type: varchar }
      - { name: file_name,  type: varchar }
```

Call them (each uses its declared `namespace`):

```sql
SELECT display_name, lat, lon FROM geo.geocode('Eiffel Tower, Paris');

-- the {service} placeholder is filled from the call argument
SELECT file_name FROM ops.logs('api') ORDER BY file_name;
```

### Reference

**Common fields** (both shapes):

| Field | Required | Notes |
|-------|----------|-------|
| `name` | yes | Function name; a valid SQL identifier, no `__`. |
| `namespace` | standalone only | SQL qualifier; inherited from the source name when attached. |
| `kind` | standalone only | `http` \| `mcp` \| `file`; inherited from the source kind when attached. |
| `description`, `wiki`, `examples` | no | Documentation surfaced by `describe` and the MCP `describe_function` tool. |
| `args` | no | Ordered argument declarations; list order is the positional call order. |
| `returns` | yes | Output columns; non-empty. The schema is fixed at plan time. |
| `config` | standalone only | Connection block, same shape as the matching source kind's `config`. |

**Each `args` entry:**

| Field | Default | Notes |
|-------|---------|-------|
| `name` | — | Valid SQL identifier, no `__`. |
| `type` | `varchar` | Column-vocabulary type (`int`, `bigint`, `double`, `bool`, `timestamp`, …). |
| `required` | `false` | Mutually exclusive with `default`; required args must precede optional/defaulted ones. |
| `default` | — | Value used when the call omits this trailing arg. |
| `tool_arg` | — | **mcp only**: wire name of the tool argument when it differs from `name`. |

**Each `returns` column:**

| Field | Notes |
|-------|-------|
| `name`, `type` | Column name and column-vocabulary type. |
| `source` | A JSONPath into each response row (`$.user.login`), or the literal `arg` to inject the bound call argument of the same name. |
| `description` | Optional. |

**Kind-specific body:**

| Kind | Fields | Notes |
|------|--------|-------|
| `http` | `endpoint`, `method`, `headers`, `body`, `response.path`, `pagination` | An arg that appears as a `{placeholder}` in `endpoint`/`body` substitutes there; otherwise it is sent as a query parameter. Standalone needs `config.base_url` unless `endpoint` is absolute. |
| `mcp` | `tool`, `tool_args`, `rows_path`, `pagination` (`cursor_arg`, `response_cursor_path`), `limit_binding` | Each arg may set `tool_arg` to rename its wire argument. Standalone needs a `transport` + `command`/`url` connection block. |
| `file` | `path` (glob with `{arg}` placeholders) | Declared `file` functions return file *metadata*, with the fixed schema `path`, `file_name`, `size_bytes`, `modified`. Line-content search is the builtin `file.grep`. |

Reserved namespaces (`http`, `file`, `mcp`, `materialized`) and the `__` separator can't be used in function / namespace / argument names.

---

## CLI

```
pawrly function list [--json]
pawrly function describe <ns.name> [--json]
pawrly function call <ns.name> [ARG]... [--arg name=value]... [--limit N] [--format table|json|csv|ndjson]
```

`call` fetches the declaration, orders the literals (numeric/bool unquoted, strings escaped), composes `SELECT * FROM ns.name(...)`, and runs it through the normal query path.

---

## MCP tools

| Tool | Input | Output |
|------|-------|--------|
| `list_functions` | — | `{ functions: [{namespace, name, kind, builtin, signature, description}] }` |
| `describe_function` | `{ function: "ns.name" }` | Full spec: args, returns, examples |
| `call_function` | `{ function, args: {name: value, ...}, max_rows }` | `{ columns, rows, row_count, truncated }` |

`call_function` composes SQL with the same renderer as the CLI and executes through the engine's query path.

---

## Notes & current limits

- Functions are live in v1: calls are not cached (they don't participate in the Parquet snapshot cache).
- `WHERE` filters apply on top of the function result (the parameters come from the call args, not from filter pushdown).
- An attached function shares its parent source's live connection (one rate-limiter / one MCP session), so it inherits the source's auth and quota state rather than opening a parallel client.
