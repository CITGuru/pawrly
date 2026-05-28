//! `<source>.chat(model, prompt) -> varchar` UDF.
//!
//! Each row triggers one POST /chat/completions request to the configured
//! OpenAI-compatible endpoint. We block on the async call inside the sync
//! UDF using `tokio::task::block_in_place + Handle::current().block_on(...)`,
//! which requires the multi-threaded runtime that the engine always uses.

use std::any::Any;
use std::sync::Arc;

use arrow_array::{Array, ArrayRef, StringArray};
use arrow_schema::DataType;
use datafusion::common::DataFusionError;
use datafusion::logical_expr::{ColumnarValue, ScalarUDF, ScalarUDFImpl, Signature, Volatility};
use serde_json::json;

#[derive(Debug)]
pub(crate) struct ChatUdf {
    name: String,
    signature: Signature,
    base_url: url::Url,
    api_key: String,
    client: reqwest::Client,
}

impl PartialEq for ChatUdf {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.signature == other.signature
            && self.base_url == other.base_url
    }
}

impl Eq for ChatUdf {}

impl std::hash::Hash for ChatUdf {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.name.hash(state);
        self.base_url.as_str().hash(state);
    }
}

impl ChatUdf {
    pub fn build(
        source_name: &str,
        base_url: url::Url,
        api_key: String,
        client: reqwest::Client,
    ) -> ScalarUDF {
        let udf = Self {
            name: format!("{source_name}.chat"),
            signature: Signature::exact(vec![DataType::Utf8, DataType::Utf8], Volatility::Volatile),
            base_url,
            api_key,
            client,
        };
        ScalarUDF::new_from_impl(udf)
    }
}

impl ScalarUDFImpl for ChatUdf {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn return_type(&self, _arg_types: &[DataType]) -> datafusion::common::Result<DataType> {
        Ok(DataType::Utf8)
    }

    fn invoke_with_args(
        &self,
        args: datafusion::logical_expr::ScalarFunctionArgs,
    ) -> datafusion::common::Result<ColumnarValue> {
        let arrays: Vec<ArrayRef> = args
            .args
            .iter()
            .map(|a| match a {
                ColumnarValue::Array(arr) => arr.clone(),
                ColumnarValue::Scalar(s) => {
                    s.to_array_of_size(args.number_rows).unwrap_or_else(|_| {
                        Arc::new(StringArray::from(vec![None as Option<&str>])) as ArrayRef
                    })
                }
            })
            .collect();

        if arrays.len() != 2 {
            return Err(DataFusionError::Plan(
                "chat() requires (model, prompt)".into(),
            ));
        }
        let models: &StringArray = arrays[0]
            .as_any()
            .downcast_ref()
            .ok_or_else(|| DataFusionError::Plan("model must be Utf8".into()))?;
        let prompts: &StringArray = arrays[1]
            .as_any()
            .downcast_ref()
            .ok_or_else(|| DataFusionError::Plan("prompt must be Utf8".into()))?;

        let mut out: Vec<Option<String>> = Vec::with_capacity(args.number_rows);
        for i in 0..args.number_rows {
            if models.is_null(i) || prompts.is_null(i) {
                out.push(None);
                continue;
            }
            let model = models.value(i).to_string();
            let prompt = prompts.value(i).to_string();
            let response =
                block_on_request(&self.client, &self.base_url, &self.api_key, &model, &prompt)?;
            out.push(Some(response));
        }
        Ok(ColumnarValue::Array(Arc::new(StringArray::from(out))))
    }
}

fn block_on_request(
    client: &reqwest::Client,
    base_url: &url::Url,
    api_key: &str,
    model: &str,
    prompt: &str,
) -> datafusion::common::Result<String> {
    let url = base_url
        .join("v1/chat/completions")
        .map_err(|e| DataFusionError::Plan(format!("bad base_url: {e}")))?;
    let body = json!({
        "model": model,
        "messages": [{"role": "user", "content": prompt}]
    });
    let req = client.post(url).bearer_auth(api_key).json(&body);

    let handle = tokio::runtime::Handle::current();
    let resp = tokio::task::block_in_place(|| {
        handle.block_on(async {
            let r = req.send().await.map_err(|e| {
                DataFusionError::External(Box::new(std::io::Error::other(format!("ai http: {e}"))))
            })?;
            let v: serde_json::Value = r.json().await.map_err(|e| {
                DataFusionError::External(Box::new(std::io::Error::other(format!("ai parse: {e}"))))
            })?;
            Ok::<serde_json::Value, DataFusionError>(v)
        })
    })?;

    let content = resp
        .pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    Ok(content)
}
