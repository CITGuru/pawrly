# Semantic layer

The semantic layer lets you define **business models** — named dimensions, measures, and relationships — on top of your raw tables, and query them structurally instead of writing SQL. It gives humans a clean vocabulary (`orders.revenue` by `orders.status`) and gives AI agents a curated, governed surface to query.

Models live under `semantic:` in [`pawrly.yaml`](./config.md). Query them with [`pawrly semantic`](./cli.md#pawrly-semantic) or the [`semantic_query` MCP tool](./mcp.md). Runnable examples live in `examples/semantic/` (single file) and `examples/semantic-multi-file/` (models split across files, with a pre-aggregation).

## Defining a model

A model is anchored on one table and declares the dimensions and measures you want to expose:

```yaml
semantic:
  models:
    - name: orders
      description: One row per order placed.
      source: data.orders        # <source>.<table>
      primary_key: [id]

      dimensions:
        - { name: status,     expr: status,     type: string }
        - { name: order_date, expr: ordered_at, type: time, grains: [day, week, month, quarter, year] }

      measures:
        - { name: order_count, agg: count_distinct, expr: id }
        - { name: revenue,     agg: sum,            expr: total_amount, format: "$#,##0.00" }
        - { name: paid_revenue, agg: sum, expr: total_amount, filters: ["status = 'paid'"] }
```

### Dimensions

A dimension is something you group or filter by. `expr` is a SQL expression over the model's table (usually just a column).

- `type` is one of `string`, `number`, `time`, `bool`.
- For `type: time`, list the valid `grains` (`hour`, `day`, `week`, `month`, `quarter`, `year`). A query can then ask for the column truncated to a grain — `orders.order_date.month`.

### Measures

A measure is an aggregation. `agg` is one of `sum`, `count`, `count_distinct`, `avg`, `min`, `max`, or a `custom` SQL aggregate:

```yaml
- name: aov
  agg: { custom: { sql: "SUM(total_amount) / NULLIF(COUNT(DISTINCT id), 0)" } }
  expr: total_amount
```

- `filters` are measure-scoped predicates — they compile to a `FILTER (WHERE …)` clause, so `paid_revenue` above sums only paid rows.
- `format` is a display hint passed through to clients.

## Querying

A **member** is `model.dimension` (optionally with a grain) or `model.measure`:

```bash
pawrly semantic query orders.revenue orders.order_count \
  --by orders.status \
  --by orders.order_date.month \
  --where 'orders.status = paid' \
  --order-by orders.revenue:desc \
  --limit 100
```

This compiles to a grouped aggregate over `data.orders` and runs on the same engine as any SQL query. The equivalent over MCP is the [`semantic_query` tool](./mcp.md).

Filters support `=`, `!=`, `>`, `>=`, `<`, `<=`, `in`, `not_in`, `in_range`, `contains`, `starts_with`, `ends_with`, `is_null`, `is_not_null`.

A filter on a **dimension** is a row-level `WHERE`; a filter on a **measure** (e.g. `orders.revenue > 1000`) is a post-aggregation `HAVING` and compares numerically. So you can keep only the groups above a threshold:

```bash
pawrly semantic query orders.revenue --by orders.status --where 'orders.revenue > 1000'
```

### Time zones

When a query truncates a time dimension to a grain, pass `--time-zone` to bucket on local time rather than UTC:

```bash
pawrly semantic query orders.revenue --by orders.order_date.day --time-zone America/New_York
```

## Relationships and cross-model queries

Declare relationships to join models. `this` refers to the declaring model; the target is referenced by its model name:

```yaml
- name: orders
  # ...
  relationships:
    - { name: customer, kind: many_to_one, target: customers, on: "this.customer_id = customers.id" }

- name: customers
  source: data.customers
  primary_key: [id]
  dimensions:
    - { name: region, expr: region, type: string }
  measures:
    - { name: customer_count, agg: count_distinct, expr: id }
```

Now a single query can span both models — measures from one, dimensions from a related one:

```bash
pawrly semantic query orders.revenue --by customers.region
```

The compiler walks the relationship graph from the model owning the measures, emits the joins (`many_to_one` / `one_to_one` join inner; `one_to_many` joins outer), and groups appropriately. A member that names an unreachable model is rejected (`PAWRLY_SEMANTIC_DISCONNECTED`) rather than guessed, and two equal-length join paths are rejected as ambiguous (`PAWRLY_SEMANTIC_AMBIGUOUS_PATH`) rather than chosen silently.

### Fan-out (chasm trap) is rejected, not silently wrong

Grouping a measure by a dimension reached across a `one_to_many` edge would multiply the measure's rows and over-count it — the classic fan-out / chasm trap. The compiler detects this and **refuses** the query (`PAWRLY_SEMANTIC_FANOUT`) instead of returning a plausible-but-wrong number:

```bash
# orders → order_items is one_to_many, so an order's revenue can't be
# attributed to a line-item SKU. This errors rather than over-counting.
pawrly semantic query orders.revenue --by order_items.sku
```

### Measures from more than one fact

When a query's measures span **two or more fact models** (e.g. `orders.revenue` and `order_items.qty`), a single join would inflate one side. The compiler instead uses **aggregate-locality** compilation: each fact is pre-aggregated at the shared-dimension grain in its own CTE, and the CTEs are `FULL OUTER JOIN`-ed on the shared keys. Each measure is computed at its own grain, so neither is over-counted:

```bash
pawrly semantic query orders.revenue order_items.qty --by orders.status
```

## Row-level security

A model's `safety:` block can carry `required_predicates` — predicates that are AND-ed into **every** compiled query for that model. They may reference `${param:NAME}` placeholders bound at query time:

```yaml
- name: orders
  # ...
  safety:
    required_predicates:
      - "tenant_id = ${param:tenant_id}"
    max_rows: 1000000
```

```bash
pawrly semantic query orders.revenue --by orders.status --param tenant_id=acme
```

Param values are bound as **escaped SQL literals**, never string-substituted — a value like `x' OR '1'='1` becomes a single literal that matches no row, so it can't alter the query. If a required param is missing, the query is **refused before any scan runs** (error `PAWRLY_SAFETY_UNBOUND_PARAM`) rather than leaking data. The same block's `require_filters_on`, `require_at_least_one_filter`, `max_rows`, and `timeout` apply too (see [Configuration → Safety](./config.md#safety)).

## Segments

A **segment** is a named, reusable set of filters defined on a model. Instead of repeating the same predicates in every request, declare them once and apply them by name — auditable, because the predicates live in trusted config rather than the request:

```yaml
- name: orders
  # ...
  segments:
    - name: recent_paid
      filters:
        - { member: orders.status,     op: equals, values: [paid] }
        - { member: orders.order_date, op: gte,    values: ["2026-01-01"] }
```

```bash
pawrly semantic query orders.revenue --by orders.status --segment orders.recent_paid
```

A segment reference is `model.segment`. Its predicates are AND-ed in alongside any `--where` filters at compile time. Segments are returned by `describe_semantic_model`, so an agent can discover and compose them.

## Pre-aggregations

A model can declare rollups it expects to be queried often:

```yaml
pre_aggregations:
  - name: daily_by_status
    dimensions: [order_date.day, status]
    measures:   [revenue, order_count]
    refresh:    1h
    partition_by: order_date.month
```

The engine **materializes** each pre-aggregation to a cached rollup table (`"semantic"."<model>__<preagg>"`) and a covered query reads it transparently instead of re-scanning the base table. A `refresh:` cadence keeps it warm via a background refresher; without one, the rollup is built lazily on first use and stays until invalidated. You can see materialized rollups with [`pawrly cache list`](./cli.md#pawrly-cache).

A rollup **covers** a query when it groups by at least the query's dimensions (at a compatible-or-finer grain), aggregates at least its measures, and carries every filtered dimension. When it does, the compiler reads the rollup — re-aggregating the stored partials (`sum`/`count` add up, `min`/`max` extend) and re-truncating grains as needed (e.g. a `day` rollup serves a `month` query).

A rollup is used **only** when it is safe to do so; otherwise the query transparently falls through to the live table, so a missing or ineligible rollup never changes a result, only how it's computed. A query reads the base table when it:

- joins or fans out across models (rollups serve single-model, single-fact queries),
- uses a non-additive measure — `avg`, `count_distinct`, or `custom` can't be re-aggregated from a partial,
- targets a model with `required_predicates` (RLS) — a rollup would need to carry the RLS columns, or
- passes a `--time-zone` (the rollup is pre-truncated).

A pre-aggregation that can't be compiled or planned is skipped at startup and logged — it never blocks the engine from booting.

## Splitting models across files

As the model set grows, list each model (or group) in its own file and pull them in with `semantic.include` — the parallel of the top-level `include:` for sources. Each included file contains **only** models (a `models:` list or a bare sequence), never sources or secrets:

```yaml
semantic:
  include:
    - ./models/*.yaml      # each file holds only models
  models:
    - name: inline_model   # inline models still allowed, merged alongside
      # ...
```

Everything is merged before validation, so a model in one file can reference a `source:` declared elsewhere and relationships may span files. See [Configuration → Multi-file configs](./config.md#multi-file-configs).

## Browsing models

```bash
pawrly semantic list              # models with dimension/measure counts
pawrly semantic describe orders   # full spec: dimensions, measures, relationships, segments
```

These are also available to agents over MCP (`list_semantic_models`, `describe_semantic_model`), which surfaces a model's relationships, segments, and required filters / RLS params so an assistant can satisfy them up front.
