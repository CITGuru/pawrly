---
name: pawrly-semantic-model
description: Author or update Pawrly semantic models in pawrly.yaml — dimensions, measures, relationships, segments, row-level security, and pre-aggregations — so agents query a curated, governed business vocabulary (orders.revenue by orders.status) instead of raw SQL. Use when adding a model, exposing a new metric/dimension, wiring cross-model joins or RLS, or tuning pre-aggregations. To run an existing model, use the `pawrly` skill's semantic_query flow.
version: 0.0.1
---

# Pawrly: semantic models

The semantic layer defines **business models** — named dimensions, measures, and relationships over your raw tables — so they can be queried structurally and safely instead of with hand-written SQL. Models live under `semantic:` in `pawrly.yaml`. Full reference: [docs/semantic.md](https://github.com/CITGuru/pawrly/blob/main/docs/semantic.md). Examples: [examples/semantic/](https://github.com/CITGuru/pawrly/tree/main/examples/semantic) (single file), [examples/semantic-multi-file/](https://github.com/CITGuru/pawrly/tree/main/examples/semantic-multi-file).

## When to use it

- Expose a curated metric/dimension vocabulary to agents and humans.
- Add governance: row-level security, row caps, and **fan-out protection** raw SQL can't give.
- Curate views over an attached database (whose raw `tables:` you can't rename).

## Workflow

1. **Anchor on one table.** `source: <source>.<table>` + a `primary_key`.
2. **Declare dimensions** (group/filter by) and **measures** (aggregations).
3. **Add relationships** for cross-model joins; **segments** for reusable filter sets; **`safety:`** for RLS/row caps; **pre_aggregations** for hot rollups.
4. **Validate, then query:**
   ```bash
   pawrly validate
   pawrly semantic list                 # models with dimension/measure counts
   pawrly semantic describe <model>     # full spec
   pawrly semantic query <model>.<measure> --by <model>.<dimension> --where '...'
   ```
5. Confirm agents see it: the `describe_semantic_model` and `semantic_query` MCP tools now surface the model (see the `pawrly` skill for the query flow).

Add the schema header for editor validation:
```yaml
# yaml-language-server: $schema=./schemas/pawrly.schema.json
```

## Defining a model

```yaml
semantic:
  models:
    - name: orders
      description: One row per order placed.
      source: data.orders          # <source>.<table>
      primary_key: [id]

      dimensions:                   # something you group or filter by; expr is SQL over the table
        - { name: status,     expr: status,     type: string }
        - { name: order_date, expr: ordered_at, type: time, grains: [day, week, month, quarter, year] }

      measures:                     # an aggregation
        - { name: order_count,  agg: count_distinct, expr: id }
        - { name: revenue,      agg: sum, expr: total_amount, format: "$#,##0.00" }
        - { name: paid_revenue, agg: sum, expr: total_amount, filters: ["status = 'paid'"] }
        - name: aov              # custom SQL aggregate
          agg: { custom: { sql: "SUM(total_amount) / NULLIF(COUNT(DISTINCT id), 0)" } }
          expr: total_amount
```

- **Dimension `type`**: `string | number | time | bool`. For `time`, list valid `grains` (`hour, day, week, month, quarter, year`) so a query can ask `orders.order_date.month`.
- **Measure `agg`**: `sum | count | count_distinct | avg | min | max` or a `custom` SQL aggregate. `filters:` compile to `FILTER (WHERE …)` (measure-scoped); `format` is a display hint.

## Relationships & cross-model queries

```yaml
    - name: orders
      relationships:
        - { name: customer, kind: many_to_one, target: customers, on: "this.customer_id = customers.id" }
    - name: customers
      source: data.customers
      primary_key: [id]
      dimensions: [{ name: region, expr: region, type: string }]
      measures:   [{ name: customer_count, agg: count_distinct, expr: id }]
```
`this` = the declaring model; the target is referenced by model name. One query can then mix `orders.revenue` by `customers.region`. The compiler walks the relationship graph and emits joins (`many_to_one`/`one_to_one` inner, `one_to_many` outer).

**Correctness guarantees** — the compiler refuses rather than returns a wrong number:
- grouping a measure across a `one_to_many` edge (fan-out / chasm trap) → `PAWRLY_SEMANTIC_FANOUT`.
- measures spanning two+ fact models → aggregate-locality (each fact pre-aggregated in its own CTE, `FULL OUTER JOIN`ed) so neither is over-counted.
- an unreachable model → `PAWRLY_SEMANTIC_DISCONNECTED`; two equal-length join paths → `PAWRLY_SEMANTIC_AMBIGUOUS_PATH`.

## Row-level security

```yaml
    - name: orders
      safety:
        required_predicates:
          - "tenant_id = ${param:tenant_id}"   # AND-ed into EVERY query for this model
        max_rows: 1000000
```
`${param:NAME}` is bound from query params as an **escaped SQL literal** (injection-safe). A missing required param **refuses the query before any scan** (`PAWRLY_SAFETY_UNBOUND_PARAM`). Same block also takes `require_filters_on`, `require_at_least_one_filter`, `timeout`. Bind at query time: `--param tenant_id=acme` (CLI) or `"params": { "tenant_id": "acme" }` (MCP).

## Segments (named reusable filter sets)

```yaml
    - name: orders
      segments:
        - name: recent_paid
          filters:
            - { member: orders.status,     op: equals, values: [paid] }
            - { member: orders.order_date, op: gte,    values: ["2026-01-01"] }
```
Apply with `--segment orders.recent_paid` (CLI) or `"segments": ["orders.recent_paid"]` (MCP). Predicates live in trusted config (auditable) and are returned by `describe_semantic_model` so agents can discover and compose them.

## Pre-aggregations (performance)

```yaml
    - name: orders
      pre_aggregations:
        - name: daily_by_status
          dimensions: [order_date.day, status]
          measures:   [revenue, order_count]
          refresh:    1h               # background refresher; omit = lazy build on first use
          partition_by: order_date.month
```
The engine materializes a rollup (`"semantic"."<model>__<preagg>"`); a covered query reads it transparently. A rollup **covers** a query when it groups by ≥ the query's dimensions (at compatible/finer grain), aggregates ≥ its measures, and carries every filtered dimension. Rollups are **not** used (query falls through to the live table, same result) when it: joins or fans out across models, uses a non-additive measure (`avg`, `count_distinct`, `custom`), targets a model with RLS `required_predicates`, or passes a `--time-zone`. An uncompilable pre-aggregation is skipped at startup and logged — it never blocks boot. Inspect rollups with `pawrly cache list`.

## Splitting across files

```yaml
semantic:
  include:
    - ./models/*.yaml      # each file holds ONLY models (a models: list or bare sequence)
  models:
    - name: inline_model   # inline models still allowed, merged alongside
```
Everything merges before validation, so a model in one file can reference a `source:` or relationship target declared elsewhere.

## Authoring rules

- Names are `snake_case` and stable; a member is `model.dimension[.grain]` or `model.measure`.
- Bump behavior carefully: adding/renaming dimensions, measures, or changing semantics is user-visible — keep names stable and document changes in the same change.
- Only mark RLS/required filters that the data governance actually needs; an unbound required param blocks every query.
- Prefer `count_distinct`/additive measures where pre-aggregations matter; non-additive measures can't be served from a rollup.

## Deliverable

Report: the config path edited, the model/members added or changed, `validate` and `semantic describe` output, any RLS params required to query it, and a sample `semantic query` that exercises it.
