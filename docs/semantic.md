# Semantic layer

The semantic layer lets you define **business models** — named dimensions, measures, and relationships — on top of your raw tables, and query them structurally instead of writing SQL. It gives humans a clean vocabulary (`orders.revenue` by `orders.status`) and gives AI agents a curated, governed surface to query.

Models live under `semantic:` in [`pawrly.yaml`](./config.md). Query them with [`pawrly semantic`](./cli.md#pawrly-semantic) or the [`semantic_query` MCP tool](./mcp.md). A runnable example lives in `examples/semantic/`.

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

The compiler walks the relationship graph from the model owning the measures, emits the joins (`many_to_one` / `one_to_one` join inner; `one_to_many` joins outer), and groups appropriately. A member that names an unreachable model is rejected rather than guessed.

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

The query compiler **matches** a query against declared rollups today: a rollup covers a query when it groups by at least the query's dimensions (at a compatible-or-finer grain), aggregates at least its measures, and spans every filtered member. When nothing covers — or a covering rollup isn't materialized yet — the query transparently falls through to the live table, so a missing rollup never fails a query. Automatic **materialization** of rollups (keeping them warm on disk) builds on the cache layer and is on the roadmap; declaring pre-aggregations now is forward-compatible.

## Browsing models

```bash
pawrly semantic list              # models with dimension/measure counts
pawrly semantic describe orders   # full spec: dimensions, measures, relationships
```

These are also available to agents over MCP (`list_semantic_models`, `describe_semantic_model`), which advertises a model's required filters and RLS params so an assistant can satisfy them up front.
