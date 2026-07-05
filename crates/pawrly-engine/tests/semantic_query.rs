//! Acceptance: define a semantic model over a fixture file source and run a
//! structured `semantic_query` end-to-end through `LocalEngine`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::sync::Arc;

use arrow_array::{Int64Array, StringArray};
use futures::StreamExt as _;
use pawrly_core::semantic::{FilterOp, SemanticFilter, SemanticOrder, SemanticQuery};
use pawrly_core::{EngineService, QueryHandle};
use pawrly_engine::{LocalEngine, LocalEngineConfig};

async fn build_engine() -> Arc<dyn EngineService> {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().to_path_buf();
    std::fs::write(
        dir.join("orders.csv"),
        "id,status,total_amount,ordered_at\n\
         1,paid,100,2026-01-15\n\
         2,paid,200,2026-01-20\n\
         3,refunded,50,2026-02-10\n\
         4,paid,300,2026-02-15\n",
    )
    .unwrap();

    let yaml = format!(
        r#"version: 1
sources:
  - name: data
    kind: file
    config:
      path: "{path}"
semantic:
  models:
    - name: orders
      description: One row per order placed.
      source: data.orders
      primary_key: [id]
      dimensions:
        - {{ name: status,     expr: status,     type: string }}
        - {{ name: order_date, expr: ordered_at, type: time, grains: [day, month] }}
      measures:
        - {{ name: revenue,      agg: sum,            expr: total_amount }}
        - {{ name: paid_revenue, agg: sum,            expr: total_amount, filters: ["status = 'paid'"] }}
        - {{ name: order_count,  agg: count_distinct, expr: id }}
"#,
        path = dir.join("orders.csv").display(),
    );

    let secrets = pawrly_secrets::StaticStore::new();
    let cfg = pawrly_config::load_str(&yaml, &secrets).expect("config parse");
    // Keep the temp dir alive for the engine's lifetime by leaking it; the OS
    // reclaims it when the test process exits.
    std::mem::forget(tmp);
    let engine = LocalEngine::new(LocalEngineConfig {
        config: cfg,
        workspace_dir: dir,
        duckdb_pool_size: None,
        home: None,
    })
    .await
    .expect("engine");
    Arc::new(engine)
}

/// Two file-source tables (`orders`, `customers`) with a many-to-one
/// relationship, plus a model carrying an RLS `required_predicates`.
async fn build_joined_engine() -> Arc<dyn EngineService> {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().to_path_buf();
    std::fs::write(
        dir.join("orders.csv"),
        "id,status,total_amount,ordered_at,customer_id\n\
         1,paid,100,2026-01-15,10\n\
         2,paid,200,2026-01-20,10\n\
         3,refunded,50,2026-02-10,20\n\
         4,paid,300,2026-02-15,20\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("customers.csv"),
        "id,region\n\
         10,US\n\
         20,EU\n",
    )
    .unwrap();

    let yaml = format!(
        r#"version: 1
sources:
  - name: data
    kind: file
    config:
      path: "{dir}/*.csv"
semantic:
  models:
    - name: orders
      source: data.orders
      primary_key: [id]
      dimensions:
        - {{ name: status, expr: status, type: string }}
      measures:
        - {{ name: revenue, agg: sum, expr: total_amount }}
      relationships:
        - {{ name: customer, kind: many_to_one, target: customers, on: "this.customer_id = customers.id" }}
    - name: customers
      source: data.customers
      primary_key: [id]
      dimensions:
        - {{ name: region, expr: region, type: string }}
      measures:
        - {{ name: customer_count, agg: count_distinct, expr: id }}
"#,
        dir = dir.display(),
    );

    let secrets = pawrly_secrets::StaticStore::new();
    let cfg = pawrly_config::load_str(&yaml, &secrets).expect("config parse");
    std::mem::forget(tmp);
    let engine = LocalEngine::new(LocalEngineConfig {
        config: cfg,
        workspace_dir: dir,
        duckdb_pool_size: None,
        home: None,
    })
    .await
    .expect("engine");
    Arc::new(engine)
}

async fn collect(handle: QueryHandle) -> Vec<arrow_array::RecordBatch> {
    let mut stream = handle.stream;
    let mut out = Vec::new();
    while let Some(batch) = stream.next().await {
        out.push(batch.expect("batch"));
    }
    out
}

/// Concatenate a result into a single batch. A `FULL OUTER JOIN` (the
/// aggregate-locality shape) can emit empty leading batches and spread rows
/// across several, so tests must not assume all rows land in `batches[0]`.
fn one_batch(batches: &[arrow_array::RecordBatch]) -> arrow_array::RecordBatch {
    let schema = batches.first().expect("at least one batch").schema();
    datafusion::arrow::compute::concat_batches(&schema, batches).expect("concat")
}

#[tokio::test]
async fn list_and_describe_models() {
    let svc = build_engine().await;

    let models = svc.list_semantic_models().await.expect("list");
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].name, "orders");
    assert_eq!(models[0].source, "data.orders");
    assert_eq!(models[0].dimension_count, 2);
    assert_eq!(models[0].measure_count, 3);

    let desc = svc
        .describe_semantic_model("orders")
        .await
        .expect("describe");
    assert_eq!(desc.dimensions.len(), 2);
    assert_eq!(desc.measures.len(), 3);
    assert!(desc.measures.iter().any(|m| m.name == "paid_revenue"));

    // Unknown model is a SemanticPlan error, not a panic.
    let err = svc.describe_semantic_model("nope").await.unwrap_err();
    assert_eq!(err.code(), "PAWRLY_SEMANTIC_PLAN");
}

#[tokio::test]
async fn grouped_revenue_by_status() {
    let svc = build_engine().await;

    let q = SemanticQuery {
        measures: vec!["orders.revenue".into()],
        dimensions: vec!["orders.status".into()],
        order_by: vec![SemanticOrder {
            member: "orders.status".into(),
            direction: pawrly_core::semantic::OrderDir::Asc,
        }],
        ..Default::default()
    };

    let batches = collect(svc.semantic_query(q).await.expect("compile+run")).await;
    assert_eq!(batches.len(), 1);
    let batch = &batches[0];
    assert_eq!(batch.num_rows(), 2);

    let status = batch
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("status col");
    let revenue = batch
        .column(1)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("revenue col");

    assert_eq!(status.value(0), "paid");
    assert_eq!(revenue.value(0), 600); // 100 + 200 + 300
    assert_eq!(status.value(1), "refunded");
    assert_eq!(revenue.value(1), 50);
}

#[tokio::test]
async fn measure_filter_and_where_clause() {
    let svc = build_engine().await;

    // paid_revenue carries a measure-scoped FILTER; the query-level filter
    // narrows to a single status.
    let q = SemanticQuery {
        measures: vec!["orders.paid_revenue".into(), "orders.order_count".into()],
        filters: vec![SemanticFilter {
            member: "orders.status".into(),
            op: FilterOp::Equals,
            values: vec!["paid".into()],
        }],
        ..Default::default()
    };

    let batches = collect(svc.semantic_query(q).await.expect("compile+run")).await;
    let batch = &batches[0];
    assert_eq!(batch.num_rows(), 1);

    let paid_revenue = batch
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("paid_revenue col");
    let order_count = batch
        .column(1)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("order_count col");
    assert_eq!(paid_revenue.value(0), 600);
    assert_eq!(order_count.value(0), 3);
}

#[tokio::test]
async fn cross_model_join_runs() {
    let svc = build_joined_engine().await;

    // revenue by customer region — a real INNER JOIN across two file tables.
    let q = SemanticQuery {
        measures: vec!["orders.revenue".into()],
        dimensions: vec!["customers.region".into()],
        order_by: vec![SemanticOrder {
            member: "customers.region".into(),
            direction: pawrly_core::semantic::OrderDir::Asc,
        }],
        ..Default::default()
    };

    let batches = collect(svc.semantic_query(q).await.expect("compile+run")).await;
    let batch = &batches[0];
    assert_eq!(batch.num_rows(), 2);

    let region = batch
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("region col");
    let revenue = batch
        .column(1)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("revenue col");

    // EU = order 3 (50) + order 4 (300) = 350; US = 100 + 200 = 300.
    assert_eq!(region.value(0), "EU");
    assert_eq!(revenue.value(0), 350);
    assert_eq!(region.value(1), "US");
    assert_eq!(revenue.value(1), 300);
}

/// Two fact tables at different grains: `orders` (one row per order) and
/// `order_items` (many rows per order), joined by a `one_to_many` relationship.
/// The item counts are uneven so a fan-out bug would visibly inflate revenue.
async fn build_facts_engine() -> Arc<dyn EngineService> {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().to_path_buf();
    std::fs::write(
        dir.join("orders.csv"),
        "id,status,total_amount\n\
         1,paid,100\n\
         2,paid,200\n\
         3,refunded,50\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("order_items.csv"),
        "order_id,sku,quantity\n\
         1,A,2\n\
         1,B,1\n\
         2,A,4\n\
         2,C,1\n\
         2,B,2\n\
         3,A,5\n",
    )
    .unwrap();

    let yaml = format!(
        r#"version: 1
sources:
  - name: data
    kind: file
    config:
      path: "{dir}/*.csv"
semantic:
  models:
    - name: orders
      source: data.orders
      primary_key: [id]
      dimensions:
        - {{ name: status, expr: status, type: string }}
      measures:
        - {{ name: revenue, agg: sum, expr: total_amount }}
      relationships:
        - {{ name: items, kind: one_to_many, target: order_items, on: "this.id = order_items.order_id" }}
    - name: order_items
      source: data.order_items
      primary_key: [order_id, sku]
      dimensions:
        - {{ name: sku, expr: sku, type: string }}
      measures:
        - {{ name: qty, agg: sum, expr: quantity }}
"#,
        dir = dir.display(),
    );

    let secrets = pawrly_secrets::StaticStore::new();
    let cfg = pawrly_config::load_str(&yaml, &secrets).expect("config parse");
    std::mem::forget(tmp);
    let engine = LocalEngine::new(LocalEngineConfig {
        config: cfg,
        workspace_dir: dir,
        duckdb_pool_size: None,
        home: None,
    })
    .await
    .expect("engine");
    Arc::new(engine)
}

/// A single model with a `(status, country)` pre-aggregation, so a query
/// grouped by status alone must re-aggregate the stored partials.
async fn build_preagg_engine() -> Arc<dyn EngineService> {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().to_path_buf();
    std::fs::write(
        dir.join("orders.csv"),
        "id,status,country,total_amount\n\
         1,paid,US,100\n\
         2,paid,CA,200\n\
         3,refunded,US,50\n\
         4,paid,US,300\n",
    )
    .unwrap();

    let yaml = format!(
        r#"version: 1
sources:
  - name: data
    kind: file
    config:
      path: "{dir}/*.csv"
semantic:
  models:
    - name: orders
      source: data.orders
      primary_key: [id]
      dimensions:
        - {{ name: status,  expr: status,  type: string }}
        - {{ name: country, expr: country, type: string }}
      measures:
        - {{ name: revenue,  agg: sum,   expr: total_amount }}
        - {{ name: orders_n, agg: count, expr: id }}
      pre_aggregations:
        - name: by_sc
          dimensions: [status, country]
          measures: [revenue, orders_n]
"#,
        dir = dir.display(),
    );

    let secrets = pawrly_secrets::StaticStore::new();
    let cfg = pawrly_config::load_str(&yaml, &secrets).expect("config parse");
    std::mem::forget(tmp);
    let engine = LocalEngine::new(LocalEngineConfig {
        config: cfg,
        workspace_dir: dir,
        duckdb_pool_size: None,
        home: None,
    })
    .await
    .expect("engine");
    Arc::new(engine)
}

#[tokio::test]
async fn preagg_rollup_serves_query_and_reaggregates() {
    let svc = build_preagg_engine().await;

    let q = SemanticQuery {
        measures: vec!["orders.revenue".into(), "orders.orders_n".into()],
        dimensions: vec!["orders.status".into()],
        order_by: vec![SemanticOrder {
            member: "orders.status".into(),
            direction: pawrly_core::semantic::OrderDir::Asc,
        }],
        ..Default::default()
    };

    let batches = collect(svc.semantic_query(q).await.expect("compile+run")).await;
    let batch = one_batch(&batches);
    assert_eq!(batch.num_rows(), 2);

    let status = batch
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("status col");
    let revenue = batch
        .column(1)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("revenue col");
    let orders_n = batch
        .column(2)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("orders_n col");

    // paid spans (US 100+300, CA 200) → 600, count 3; refunded → 50, count 1.
    assert_eq!(status.value(0), "paid");
    assert_eq!(revenue.value(0), 600);
    assert_eq!(orders_n.value(0), 3);
    assert_eq!(status.value(1), "refunded");
    assert_eq!(revenue.value(1), 50);
    assert_eq!(orders_n.value(1), 1);

    // The rollup was materialized — proving the query read it, not the base.
    let entries = svc.cache_entries().await.expect("cache entries");
    assert!(
        entries
            .iter()
            .any(|e| e.name.schema == "semantic" && e.name.table == "orders__by_sc"),
        "expected a materialized rollup entry, got {:?}",
        entries
            .iter()
            .map(|e| e.name.to_string())
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn multi_fact_aggregate_locality_does_not_over_count() {
    let svc = build_facts_engine().await;

    // revenue (one-per-order) and qty (many-per-order) grouped by status. A
    // single GROUP BY over the joined tables would multiply each order's
    // revenue by its item count; aggregate-locality aggregates each fact at its
    // own grain so neither is inflated.
    let q = SemanticQuery {
        measures: vec!["orders.revenue".into(), "order_items.qty".into()],
        dimensions: vec!["orders.status".into()],
        order_by: vec![SemanticOrder {
            member: "orders.status".into(),
            direction: pawrly_core::semantic::OrderDir::Asc,
        }],
        ..Default::default()
    };

    let batches = collect(svc.semantic_query(q).await.expect("compile+run")).await;
    let batch = one_batch(&batches);
    assert_eq!(batch.num_rows(), 2);

    let status = batch
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("status col");
    let revenue = batch
        .column(1)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("revenue col");
    let qty = batch
        .column(2)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("qty col");

    // paid: revenue 100+200 = 300 (NOT 100*2 + 200*3 = 800); qty 3+7 = 10.
    assert_eq!(status.value(0), "paid");
    assert_eq!(revenue.value(0), 300, "revenue must not fan out");
    assert_eq!(qty.value(0), 10);
    // refunded: revenue 50; qty 5.
    assert_eq!(status.value(1), "refunded");
    assert_eq!(revenue.value(1), 50);
    assert_eq!(qty.value(1), 5);
}

#[tokio::test]
async fn measure_having_filter_runs() {
    let svc = build_facts_engine().await;

    // A measure-member filter must compile to HAVING and execute: keep only the
    // status groups whose total revenue exceeds 100 (paid=300 stays, refunded
    // =50 drops).
    let q = SemanticQuery {
        measures: vec!["orders.revenue".into()],
        dimensions: vec!["orders.status".into()],
        filters: vec![SemanticFilter {
            member: "orders.revenue".into(),
            op: FilterOp::Gt,
            values: vec!["100".into()],
        }],
        ..Default::default()
    };

    let batches = collect(svc.semantic_query(q).await.expect("compile+run")).await;
    let batch = one_batch(&batches);
    assert_eq!(batch.num_rows(), 1);
    let status = batch
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("status col");
    let revenue = batch
        .column(1)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("revenue col");
    assert_eq!(status.value(0), "paid");
    assert_eq!(revenue.value(0), 300);
}

#[tokio::test]
async fn multi_fact_outer_measure_filter_runs() {
    let svc = build_facts_engine().await;

    // Multi-fact query with a measure threshold: the filter applies over the
    // joined CTEs (keep status groups with revenue > 100).
    let q = SemanticQuery {
        measures: vec!["orders.revenue".into(), "order_items.qty".into()],
        dimensions: vec!["orders.status".into()],
        filters: vec![SemanticFilter {
            member: "orders.revenue".into(),
            op: FilterOp::Gt,
            values: vec!["100".into()],
        }],
        ..Default::default()
    };

    let batches = collect(svc.semantic_query(q).await.expect("compile+run")).await;
    let batch = one_batch(&batches);
    assert_eq!(
        batch.num_rows(),
        1,
        "only the paid group survives revenue > 100"
    );
    let status = batch
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("status col");
    let revenue = batch
        .column(1)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("revenue col");
    let qty = batch
        .column(2)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("qty col");
    assert_eq!(status.value(0), "paid");
    assert_eq!(revenue.value(0), 300);
    assert_eq!(qty.value(0), 10);
}

#[tokio::test]
async fn fan_out_dimension_is_rejected() {
    let svc = build_facts_engine().await;

    // Grouping order revenue by item SKU is the chasm trap: an order's revenue
    // cannot be attributed to a SKU. The compiler must refuse, not over-count.
    let q = SemanticQuery {
        measures: vec!["orders.revenue".into()],
        dimensions: vec!["order_items.sku".into()],
        ..Default::default()
    };

    let err = svc.semantic_query(q).await.err().expect("must reject");
    let msg = err.to_string();
    assert!(msg.contains("fans out"), "unexpected error: {msg}");
}

/// A single model guarded by an RLS `required_predicates` referencing a param.
async fn build_rls_engine() -> Arc<dyn EngineService> {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().to_path_buf();
    std::fs::write(
        dir.join("orders.csv"),
        "id,status,total_amount,ordered_at\n\
         1,paid,100,2026-01-15\n\
         2,paid,200,2026-01-20\n\
         3,refunded,50,2026-02-10\n",
    )
    .unwrap();

    let yaml = format!(
        r#"version: 1
sources:
  - name: data
    kind: file
    config:
      path: "{path}"
semantic:
  models:
    - name: orders
      source: data.orders
      primary_key: [id]
      dimensions:
        - {{ name: status, expr: status, type: string }}
      measures:
        - {{ name: revenue, agg: sum, expr: total_amount }}
      safety:
        required_predicates:
          - "status = ${{param:status}}"
"#,
        path = dir.join("orders.csv").display(),
    );

    let secrets = pawrly_secrets::StaticStore::new();
    let cfg = pawrly_config::load_str(&yaml, &secrets).expect("config parse");
    std::mem::forget(tmp);
    let engine = LocalEngine::new(LocalEngineConfig {
        config: cfg,
        workspace_dir: dir,
        duckdb_pool_size: None,
        home: None,
    })
    .await
    .expect("engine");
    Arc::new(engine)
}

#[tokio::test]
async fn rls_param_filters_rows() {
    let svc = build_rls_engine().await;

    let mut q = SemanticQuery {
        measures: vec!["orders.revenue".into()],
        ..Default::default()
    };
    q.params.insert("status".into(), "paid".into());

    let batches = collect(svc.semantic_query(q).await.expect("compile+run")).await;
    let revenue = batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("revenue col");
    // Only paid rows survive the RLS predicate: 100 + 200 = 300.
    assert_eq!(revenue.value(0), 300);
}

#[tokio::test]
async fn rls_unbound_param_refused() {
    let svc = build_rls_engine().await;

    // No `status` param supplied → refused before any scan, as a Safety error.
    let q = SemanticQuery {
        measures: vec!["orders.revenue".into()],
        ..Default::default()
    };
    match svc.semantic_query(q).await {
        Err(e) => assert_eq!(e.code(), "PAWRLY_SAFETY_UNBOUND_PARAM"),
        Ok(_) => panic!("expected an unbound-param safety error"),
    }
}
