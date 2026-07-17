# Semantic layer

The semantic layer lets you define the business models and concepts in your raw tables: dimensions, measures, and relationships then query them structurally. You can define a calculation such as `orders.revenue` once, then query it by dimensions such as `orders.status` without rewriting the SQL. This gives humans a clean vocabulary and agents a curated, governed surface.

Models live under `semantic:` in [`pawrly.yaml`](./config.md). Query them with [`pawrly semantic`](./cli.md#pawrly-semantic) or the [`semantic_query` MCP tool](./mcp.md). Runnable examples live in `examples/semantic/` (single file) and `examples/semantic-multi-file/` (models split across files, with a pre-aggregation).

## Defining a model

A model starts with one table and declares the dimensions and measures available to query:

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
- name: weighted_score
  agg: { custom: { sql: "SUM(score * weight) / NULLIF(SUM(weight), 0)" } }
  expr: score
```

- `filters` are measure-scoped predicates — they compile to a `FILTER (WHERE …)` clause, so `paid_revenue` above sums only paid rows.
- `format` is a display hint passed through to clients.
- Use a [metric](#metrics), rather than a `custom` aggregate, for ratios or arithmetic between measures. This keeps the underlying measures available to filters, fan-out checks, and pre-aggregations.

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

Pawrly finds a join path from the model that owns the measures, emits joins ( `many_to_one` and `one_to_one` relationships use inner joins; `one_to_many` relationships use outer joins) and groups appropriatelt. It rejects unreachable models (`PAWRLY_SEMANTIC_DISCONNECTED`) and two equal-length join paths as ambiguous (`PAWRLY_SEMANTIC_AMBIGUOUS_PATH`) instead of choosing a path.

### Fan-out checks

Grouping a measure by a dimension across a `one_to_many` relationship can multiply rows and over-count the measure. Pawrly rejects the query with `PAWRLY_SEMANTIC_FANOUT`:

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

A model's `safety:` block can define `required_predicates`. These predicates are added to every query for the model and may contain `${param:NAME}` placeholders bound at query time:

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

Param values are bound as escaped SQL literals, not inserted into the SQL string. A value such as `x' OR '1'='1` remains one literal and cannot alter the query.

If a required param is missing, Pawrly returns `PAWRLY_SAFETY_UNBOUND_PARAM` before scanning data. The same block also supports `require_filters_on`, `require_at_least_one_filter`, `max_rows`, and `timeout`. See [Configuration → Safety](./config.md#safety).

## Segments

A **segment** is a named, reusable set of filters. Declare the filters in the model, then apply them by name instead of repeating them in each request:

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

A segment reference is `model.segment`. Its predicates are combined with any `--where` filters at compile time. The MCP/CLI tool `describe_semantic_model`, returns the available segments.

## Metrics

A **metric** composes measures into a named ratio or arithmetic expression, including measures from different models. Metrics are defined beside `models:` under `semantic:` and their names cannot contain dots. In a query, a metric takes the same position as a `model.measure`:

```yaml
semantic:
  models:
    # ... orders (revenue, order_count), customers (customer_count) ...
  metrics:
    # ratio — average order value
    - name: aov
      kind: { ratio: { numerator: orders.revenue, denominator: orders.order_count } }
      format: "$#,##0.00"

    # cross-model ratio — revenue per customer
    - name: arpu
      kind: { ratio: { numerator: orders.revenue, denominator: customers.customer_count } }

    # filter applied to each underlying measure
    - name: paid_aov
      filter: "status = 'paid'"
      kind: { ratio: { numerator: orders.revenue, denominator: orders.order_count } }

    # derived — arithmetic over {member} references, with optional per-token filters
    - name: food_gross_profit
      kind: { derived: { expr: "{orders.revenue | category = 'food'} - {orders.cost | category = 'food'}" } }
```

```bash
pawrly semantic query aov --by orders.status
pawrly semantic query arpu --by customers.region   # spans models via relationships
pawrly semantic query paid_aov orders.revenue      # metrics and raw measures mix freely
```

A metric is resolved to its underlying measures before the query is compiled, so fan-out checks, RLS, and time grains still apply. The final calculation runs over the aggregated columns. Ratios cast to `DOUBLE` and use `NULLIF(…, 0)` for the denominator. Metrics may reference other metrics; cycles are rejected during config validation.

Metric filters can apply to the whole metric (`filter:`), one ratio operand (`{ member: …, filter: … }`), or one token in a derived expression (`{orders.revenue | status = 'paid'}`). All three are pushed down into the underlying measure's `FILTER (WHERE …)` clause.

### Window metrics

`cumulative`, `offset`, and `share` compute over the aggregated series:

```yaml
metrics:
  # running total, year-to-date, and a rolling 7-period average
  - name: revenue_running
    kind: { cumulative: { measure: orders.revenue, window: running_total } }
  - name: revenue_ytd
    kind: { cumulative: { measure: orders.revenue, window: { grain_to_date: { grain: year } } } }
  - name: revenue_7d_avg
    kind: { cumulative: { measure: orders.revenue, window: { trailing: { periods: 7 } }, agg: avg } }

  # period-over-period: prior value, difference, or growth ratio
  - name: revenue_mom
    kind: { offset: { measure: orders.revenue, periods: 1, output: growth } }
    format: "0.0%"

  # part-of-whole: revenue ÷ the total within each region (over: [] = grand total)
  - name: pct_of_region
    kind: { share: { measure: orders.revenue, over: [orders.region] } }
    format: "0.0%"
```

```bash
pawrly semantic query revenue_running --by orders.order_date.month
pawrly semantic query revenue_mom --by orders.order_date.month --by orders.status
pawrly semantic query pct_of_region --by orders.region
```

For `cumulative` and `offset`, Pawrly joins the aggregate to a dense date axis at the query's grain. A month with no source rows still appears, so running totals carry through gaps and offsets use the actual previous period. Pawrly generates the axis within the data's bounds. To use a calendar table instead, set `semantic.time_spine: { source: <source>.<table>, column: <date column> }`.

These metrics require exactly one time dimension with a grain. `share.over` must be a subset of the query's dimensions.

## Pre-aggregations

A model can declare rollups for common queries:

```yaml
pre_aggregations:
  - name: daily_by_status
    dimensions: [order_date.day, status]
    measures:   [revenue, order_count]
    refresh:    1h
    partition_by: order_date.month
```

Pawrly stores each pre-aggregation as a cached rollup table named `"semantic"."<model>__<preagg>"`. A covered query reads that table instead of scanning the base table. With `refresh:`, a background task rebuilds the rollup on the given cadence. Without it, Pawrly builds the rollup on first use and keeps it until invalidated. List materialized rollups with [`pawrly cache list`](./cli.md#pawrly-cache).

A rollup **covers** a query when it includes the query's measures, filtered dimensions, and dimensions at a compatible or finer grain. The compiler can then read the rollup, combine stored partials (`sum` and `count` add up; `min` and `max` extend), and truncate time grains as needed. For example, a `day` rollup can serve a `month` query.

If a rollup cannot answer a query without changing its result, the query reads the base table. This happens when it:

- joins or fans out across models (rollups serve single-model, single-fact queries),
- uses a non-additive measure — `avg`, `count_distinct`, or `custom` can't be re-aggregated from a partial,
- targets a model with `required_predicates` (RLS) — a rollup would need to carry the RLS columns, or
- passes a `--time-zone` (the rollup is pre-truncated).

A pre-aggregation that cannot be compiled or planned is logged and skipped at startup. It does not prevent the engine from starting.

## Splitting models across files

As the model set grows, move models into separate files and load them with `semantic.include`. Each included file contains only models, as a `models:` list or a bare sequence. Sources and secrets remain in the main config or its top-level includes:

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
pawrly semantic metrics           # workspace metrics with kind and description
```

The MCP tools `list_semantic_models`, `describe_semantic_model`, `list_metrics`, and `describe_metric` return the same information, including relationships, segments, and required filters or RLS params.
