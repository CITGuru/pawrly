//! The UDTF DataFusion plans the mangled function name into, plus positional
//! argument binding and the `TableProvider` that runs the executor.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use arrow_schema::{Schema, SchemaRef};
use async_trait::async_trait;
use datafusion::catalog::{Session, TableFunctionImpl};
use datafusion::common::{DataFusionError, Result};
use datafusion::datasource::memory::MemorySourceConfig;
use datafusion::datasource::{TableProvider, TableType};
use datafusion::logical_expr::Expr;
use datafusion::physical_plan::ExecutionPlan;
use datafusion::scalar::ScalarValue;
use pawrly_core::FunctionDef;

use super::RegisteredFunction;

/// The `TableFunctionImpl` registered under the mangled name. Its `call` binds
/// the literal args and hands back a [`FunctionCallTable`].
pub(crate) struct PawrlyFunctionUdtf {
    pub func: Arc<RegisteredFunction>,
}

impl std::fmt::Debug for PawrlyFunctionUdtf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PawrlyFunctionUdtf")
            .field("mangled", &self.func.mangled)
            .finish()
    }
}

impl TableFunctionImpl for PawrlyFunctionUdtf {
    // Sync by trait contract; the returned provider's async `scan` does the work.
    fn call(&self, args: &[Expr]) -> Result<Arc<dyn TableProvider>> {
        let bound = bind_args(&self.func.def, args)?;
        Ok(Arc::new(FunctionCallTable {
            func: self.func.clone(),
            bound,
        }))
    }
}

/// Positionally bind literal call args to the declaration's params. DataFusion
/// plans UDTF args against an empty schema, so column refs can never resolve —
/// we just produce a clearer message.
fn bind_args(def: &FunctionDef, args: &[Expr]) -> Result<BTreeMap<String, String>> {
    let qn = format!("{}.{}", def.namespace, def.name);
    if args.len() > def.args.len() {
        return Err(DataFusionError::Plan(format!(
            "function `{qn}` takes at most {} argument(s), got {}",
            def.args.len(),
            args.len()
        )));
    }

    let mut bound: BTreeMap<String, String> = BTreeMap::new();
    for (i, expr) in args.iter().enumerate() {
        let param = &def.args[i];
        match literal_value(expr) {
            Some(Some(s)) => {
                bound.insert(param.name.clone(), s);
            }
            // A typed NULL is "not provided": default/required apply below.
            Some(None) => {}
            None => {
                return Err(DataFusionError::Plan(format!(
                    "function `{qn}` argument {} (`{}`) must be a literal value",
                    i + 1,
                    param.name
                )));
            }
        }
    }

    // Fill defaults, then enforce required.
    for (i, param) in def.args.iter().enumerate() {
        if bound.contains_key(&param.name) {
            continue;
        }
        if let Some(d) = &param.default {
            bound.insert(param.name.clone(), d.clone());
        } else if param.required {
            return Err(DataFusionError::Plan(format!(
                "function `{qn}` requires argument `{}` (position {})",
                param.name,
                i + 1
            )));
        }
    }
    Ok(bound)
}

/// `Some(Some(s))` = a bound literal; `Some(None)` = a typed null (not provided);
/// `None` = not a literal (column ref / expression).
fn literal_value(expr: &Expr) -> Option<Option<String>> {
    match expr {
        Expr::Literal(sv, _) => Some(scalar_to_string(sv)),
        Expr::Negative(inner) => match inner.as_ref() {
            Expr::Literal(sv, _) => Some(scalar_to_string(sv).map(|s| format!("-{s}"))),
            _ => None,
        },
        _ => None,
    }
}

/// Typed nulls and unhandled types return `None` ("not provided").
fn scalar_to_string(sv: &ScalarValue) -> Option<String> {
    match sv {
        ScalarValue::Utf8(Some(s)) | ScalarValue::LargeUtf8(Some(s)) => Some(s.clone()),
        ScalarValue::Int8(Some(n)) => Some(n.to_string()),
        ScalarValue::Int16(Some(n)) => Some(n.to_string()),
        ScalarValue::Int32(Some(n)) => Some(n.to_string()),
        ScalarValue::Int64(Some(n)) => Some(n.to_string()),
        ScalarValue::UInt8(Some(n)) => Some(n.to_string()),
        ScalarValue::UInt16(Some(n)) => Some(n.to_string()),
        ScalarValue::UInt32(Some(n)) => Some(n.to_string()),
        ScalarValue::UInt64(Some(n)) => Some(n.to_string()),
        ScalarValue::Float32(Some(n)) => Some(n.to_string()),
        ScalarValue::Float64(Some(n)) => Some(n.to_string()),
        ScalarValue::Boolean(Some(b)) => Some(b.to_string()),
        _ => None,
    }
}

/// `TableProvider` over a bound function call. Schema is static (from the
/// declaration). No filter-pushdown: params come from call args, so `WHERE`
/// applies on top of the result.
struct FunctionCallTable {
    func: Arc<RegisteredFunction>,
    bound: BTreeMap<String, String>,
}

impl std::fmt::Debug for FunctionCallTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FunctionCallTable")
            .field("function", &self.func.mangled)
            .finish()
    }
}

#[async_trait]
impl TableProvider for FunctionCallTable {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn schema(&self) -> SchemaRef {
        self.func.schema.clone()
    }

    fn table_type(&self) -> TableType {
        TableType::Base
    }

    async fn scan(
        &self,
        _state: &dyn Session,
        projection: Option<&Vec<usize>>,
        _filters: &[Expr],
        limit: Option<usize>,
    ) -> Result<Arc<dyn ExecutionPlan>> {
        let batch = self.func.executor.invoke(&self.bound, limit).await?;

        let (schema, batch) = match projection {
            Some(p) => {
                let s: SchemaRef = Arc::new(Schema::new(
                    p.iter()
                        .map(|i| self.func.schema.field(*i).clone())
                        .collect::<Vec<_>>(),
                ));
                let b = batch
                    .project(p)
                    .map_err(|e| DataFusionError::ArrowError(Box::new(e), None))?;
                (s, b)
            }
            None => (self.func.schema.clone(), batch),
        };

        let exec = MemorySourceConfig::try_new_exec(&[vec![batch]], schema, None)
            .map_err(|e| DataFusionError::Plan(e.to_string()))?;
        Ok(exec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use datafusion::logical_expr::{col, lit};
    use pawrly_core::{FunctionArg, FunctionColumn, FunctionKind};

    fn arg(name: &str, required: bool, default: Option<&str>) -> FunctionArg {
        FunctionArg {
            name: name.into(),
            r#type: "varchar".into(),
            required,
            default: default.map(str::to_string),
            description: None,
            tool_arg: None,
        }
    }

    fn def(args: Vec<FunctionArg>) -> FunctionDef {
        FunctionDef {
            namespace: "gh".into(),
            name: "search".into(),
            kind: FunctionKind::Http,
            description: None,
            wiki: None,
            examples: vec![],
            args,
            returns: vec![FunctionColumn {
                name: "x".into(),
                r#type: "varchar".into(),
                source: None,
                description: None,
            }],
            connection: serde_json::Value::Null,
            body: serde_json::Value::Null,
            source: None,
            builtin: false,
            cache: Default::default(),
            safety: None,
        }
    }

    #[test]
    fn positional_binding_with_defaults() {
        let d = def(vec![arg("q", true, None), arg("limit", false, Some("50"))]);
        let bound = bind_args(&d, &[lit("is:open")]).unwrap();
        assert_eq!(bound.get("q").unwrap(), "is:open");
        assert_eq!(bound.get("limit").unwrap(), "50"); // default filled
    }

    #[test]
    fn int_and_bool_and_negative_literals() {
        let d = def(vec![
            arg("a", false, None),
            arg("b", false, None),
            arg("c", false, None),
        ]);
        let bound = bind_args(&d, &[lit(42_i64), lit(true), lit(-7_i64)]).unwrap();
        assert_eq!(bound.get("a").unwrap(), "42");
        assert_eq!(bound.get("b").unwrap(), "true");
        assert_eq!(bound.get("c").unwrap(), "-7");
    }

    #[test]
    fn missing_required_names_param_and_position() {
        let d = def(vec![arg("q", true, None)]);
        let err = bind_args(&d, &[]).unwrap_err().to_string();
        assert!(err.contains("requires argument `q`"), "{err}");
        assert!(err.contains("position 1"), "{err}");
    }

    #[test]
    fn too_many_args_errors() {
        let d = def(vec![arg("q", false, None)]);
        let err = bind_args(&d, &[lit("a"), lit("b")])
            .unwrap_err()
            .to_string();
        assert!(err.contains("at most 1 argument"), "{err}");
    }

    #[test]
    fn column_ref_arg_is_rejected() {
        let d = def(vec![arg("q", false, None)]);
        let err = bind_args(&d, &[col("some_column")])
            .unwrap_err()
            .to_string();
        assert!(err.contains("must be a literal value"), "{err}");
    }

    #[test]
    fn typed_null_is_not_provided_then_required_fails() {
        let d = def(vec![arg("q", true, None)]);
        let err = bind_args(&d, &[lit(ScalarValue::Utf8(None))])
            .unwrap_err()
            .to_string();
        assert!(err.contains("requires argument `q`"), "{err}");
    }
}
