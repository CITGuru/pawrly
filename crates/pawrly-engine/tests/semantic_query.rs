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
use pawrly_core::{EngineService, QueryStream};
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
    })
    .await
    .expect("engine");
    Arc::new(engine)
}

async fn collect(stream: QueryStream) -> Vec<arrow_array::RecordBatch> {
    let mut stream = stream;
    let mut out = Vec::new();
    while let Some(batch) = stream.next().await {
        out.push(batch.expect("batch"));
    }
    out
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
