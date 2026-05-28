//! `<source>.models` table: a small static catalog of configured models.

use std::any::Any;
use std::sync::Arc;

use arrow_array::{ArrayRef, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use async_trait::async_trait;
use datafusion::catalog::Session;
use datafusion::common::DataFusionError;
use datafusion::datasource::memory::MemorySourceConfig;
use datafusion::datasource::{TableProvider, TableType};
use datafusion::logical_expr::Expr;
use datafusion::physical_plan::ExecutionPlan;

#[derive(Clone, Debug)]
pub(crate) struct ModelRow {
    pub name: String,
    pub model: String,
    pub provider: String,
}

#[derive(Debug)]
pub(crate) struct ModelsTable {
    pub schema: SchemaRef,
    pub rows: Vec<ModelRow>,
}

impl ModelsTable {
    pub fn new(rows: Vec<ModelRow>) -> Self {
        let schema = Arc::new(Schema::new(vec![
            Field::new("name", DataType::Utf8, false),
            Field::new("model", DataType::Utf8, false),
            Field::new("provider", DataType::Utf8, false),
        ]));
        Self { schema, rows }
    }
}

#[async_trait]
impl TableProvider for ModelsTable {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }

    fn table_type(&self) -> TableType {
        TableType::Base
    }

    async fn scan(
        &self,
        _state: &dyn Session,
        projection: Option<&Vec<usize>>,
        _filters: &[Expr],
        _limit: Option<usize>,
    ) -> datafusion::common::Result<Arc<dyn ExecutionPlan>> {
        let names: Vec<&str> = self.rows.iter().map(|r| r.name.as_str()).collect();
        let models: Vec<&str> = self.rows.iter().map(|r| r.model.as_str()).collect();
        let providers: Vec<&str> = self.rows.iter().map(|r| r.provider.as_str()).collect();
        let arrays: Vec<ArrayRef> = vec![
            Arc::new(StringArray::from(names)),
            Arc::new(StringArray::from(models)),
            Arc::new(StringArray::from(providers)),
        ];
        let batch = RecordBatch::try_new(self.schema.clone(), arrays)
            .map_err(|e| DataFusionError::ArrowError(Box::new(e), None))?;

        let projected_schema = if let Some(p) = projection {
            let fields: Vec<Field> = p.iter().map(|i| self.schema.field(*i).clone()).collect();
            Arc::new(Schema::new(fields))
        } else {
            self.schema.clone()
        };
        let projected: RecordBatch = if let Some(p) = projection {
            let cols: Vec<ArrayRef> = p.iter().map(|i| batch.column(*i).clone()).collect();
            RecordBatch::try_new(projected_schema.clone(), cols)
                .map_err(|e| DataFusionError::ArrowError(Box::new(e), None))?
        } else {
            batch
        };
        let exec = MemorySourceConfig::try_new_exec(&[vec![projected]], projected_schema, None)
            .map_err(|e| DataFusionError::Plan(e.to_string()))?;
        Ok(exec)
    }
}
