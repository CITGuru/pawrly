# MCP backend (`kind: mcp`)

Connects to an external [Model Context Protocol](https://modelcontextprotocol.io) server and exposes its tools as SQL tables: a `SELECT` runs `tools/call`, pushed-down `WHERE` filters become tool arguments, and the result rows are projected into columns. Full prose: [docs/sources.md → MCP backend](https://github.com/CITGuru/pawrly/blob/main/docs/sources.md#mcp-backend-mcp--an-mcp-servers-tools-as-tables). Runnable examples: [examples/linear/pawrly.yaml](https://github.com/CITGuru/pawrly/blob/main/examples/linear/pawrly.yaml), [examples/mcp.yaml](https://github.com/CITGuru/pawrly/blob/main/examples/mcp.yaml).

> This is the inverse of Pawrly's own MCP server (`pawrly mcp-stdio`, the `pawrly` skill's surface): here Pawrly is the *client*, attaching someone else's MCP server as a data source.

## Connect

Two transports — a remote server over **streamable HTTP**, or a local subprocess over **stdio**:

```yaml
sources:
  - name: linear
    kind: mcp
    config:
      transport: streamable_http
      url: https://mcp.linear.app/mcp
      expose: read_only            # read_only (default) | all | listed
      auth:
        type: header
        headers:
          - { name: Authorization, bearer: ${secret:LINEAR_API_KEY} }

  - name: github
    kind: mcp
    config:
      transport: stdio
      command: ["npx", "-y", "@modelcontextprotocol/server-github"]
      env:
        GITHUB_TOKEN: ${secret:GITHUB_TOKEN}
```

```sql
SELECT id, title, status FROM linear.list_issues WHERE assignee = 'me@example.com' LIMIT 20
```

## Two ways to get tables, one dial

A source produces tables from introspection (`tools/list`) and from declaration (`tables:`). `config.expose` sets how much introspection auto-exposes:


| `expose`              | auto-exposed                                  | use                                          |
| --------------------- | --------------------------------------------- | -------------------------------------------- |
| `read_only` (default) | tools with `annotations.readOnlyHint == true` | zero-config, safe                            |
| `all`                 | every non-destructive tool                    | you accept a `SELECT` may call any read tool |
| `listed`              | none — only `tables:` / `include:`            | fully declarative                            |


`include` / `exclude` (by tool name) narrow whatever `expose` admits. A `destructiveHint` tool is **never** auto-exposed.

## Output columns

A tool result is exposed as a single `result` JSON column unless the tool declares an `outputSchema` (columns inferred) or a `tables:` entry declares them. Pin typed columns to get real SQL columns:

```yaml
    tables:
      - name: list_issues
        columns:
          - { name: id,       type: varchar, path: [id] }
          - { name: title,    type: varchar, path: [title] }
          - { name: status,   type: varchar, path: [status] }
          - { name: priority, type: varchar, path: [priority, name] }   # nested
          - { name: raw,      type: json,    path: [] }                  # whole element as JSON
```

## `tables:` entry fields

Each entry **patches** a synthesized table of the same name, or **defines** a new one:


| Field           | Meaning                                                                                                                              |
| --------------- | ------------------------------------------------------------------------------------------------------------------------------------ |
| `tool`          | The MCP tool to call (defaults to the table name on a patch).                                                                        |
| `columns`       | `{ name, type, path: [keys] }` — pull a (possibly nested) field from each row element; an empty `path` is the whole element as JSON. |
| `tool_args`     | Static arguments always sent.                                                                                                        |
| `filters`       | Bind a SQL filter to a differently-named tool argument.                                                                              |
| `limit_binding` | `{ tool_arg, max }` — push SQL `LIMIT` into a tool argument.                                                                         |
| `pagination`    | `{ cursor_arg, response_cursor_path }` — cursor pagination (default: follow `nextCursor`).                                           |


A pushed-down `WHERE` filter becomes a tool argument; use `filters` when the SQL column name and the tool's argument name differ.

## Security

- A `streamable_http` `url` must be `https` (or `http` only for loopback), and may **not** embed credentials in the URL.
- Carry credentials in the `auth` block (`header` / `bearer` / `basic`, same shapes as the [http backend](http-backend.md)) with `${secret:…}` tokens.
- These rules are validated at config-load time.

## Validation

```bash
pawrly validate
pawrly source test <name>             # check the MCP server is reachable
pawrly schema <name>                  # list the exposed tool-tables
pawrly schema <name>.<table>          # confirm columns
pawrly sql "SELECT ... FROM <name>.<table> WHERE <required args> LIMIT 5"
```

