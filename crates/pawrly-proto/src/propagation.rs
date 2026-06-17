//! W3C trace-context propagation over tonic gRPC metadata.
//!
//! [`inject_context`] writes the active context's `traceparent` onto an outgoing
//! request; [`extract_context`] reads it back on the server. Both go through the
//! globally installed propagator, which is a no-op until `pawrly-telemetry`
//! installs the W3C propagator — so with OTel disabled, injection writes nothing
//! and extraction returns an empty (non-remote) context.

use opentelemetry::Context;
use opentelemetry::propagation::{Extractor, Injector};
use tonic::metadata::{Ascii, KeyRef, MetadataKey, MetadataMap, MetadataValue};

/// Read-only view of request metadata for the propagator.
struct MetadataExtractor<'a>(&'a MetadataMap);

impl Extractor for MetadataExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|v| v.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0
            .keys()
            .filter_map(|k| match k {
                KeyRef::Ascii(k) => Some(k.as_str()),
                KeyRef::Binary(_) => None,
            })
            .collect()
    }
}

/// Mutable view of request metadata for the propagator. Non-ASCII keys/values
/// produced by a propagator are silently dropped rather than panicking.
struct MetadataInjector<'a>(&'a mut MetadataMap);

impl Injector for MetadataInjector<'_> {
    fn set(&mut self, key: &str, value: String) {
        if let Ok(name) = MetadataKey::<Ascii>::from_bytes(key.as_bytes())
            && let Ok(val) = MetadataValue::try_from(value.as_str())
        {
            self.0.insert(name, val);
        }
    }
}

/// Extract a remote W3C trace context from request metadata.
pub fn extract_context(meta: &MetadataMap) -> Context {
    opentelemetry::global::get_text_map_propagator(|p| p.extract(&MetadataExtractor(meta)))
}

/// Inject the given context's `traceparent` into outgoing request metadata.
pub fn inject_context(cx: &Context, meta: &mut MetadataMap) {
    opentelemetry::global::get_text_map_propagator(|p| {
        p.inject_context(cx, &mut MetadataInjector(meta));
    });
}
