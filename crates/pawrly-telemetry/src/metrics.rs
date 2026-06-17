//! Metric instrument facade.
//!
//! Thin accessors over the global OTel meter so call sites stay clean:
//! `pawrly_telemetry::metrics::query_duration().record(ms, &attrs)`. Each
//! instrument is built once on first use; until [`crate::init`] installs a meter
//! provider the global meter is a no-op, so these cost nothing when telemetry is
//! off.
//!
//! This module depends only on the lightweight `opentelemetry` facade — never
//! the SDK/exporters — so the engine and source crates can emit metrics without
//! pulling the heavy exporter tree. Keep attributes low-cardinality: never SQL,
//! full URLs, query ids, or param values.

use std::sync::OnceLock;

use opentelemetry::metrics::{Counter, Histogram, Meter, UpDownCounter};

/// The instrumentation scope; matches the tracer/meter name used in `init`.
const SCOPE: &str = "pawrly";

fn meter() -> Meter {
    opentelemetry::global::meter(SCOPE)
}

/// Total queries executed, by `interface`, `status`, and `error_code`.
pub fn query_total() -> &'static Counter<u64> {
    static I: OnceLock<Counter<u64>> = OnceLock::new();
    I.get_or_init(|| {
        meter()
            .u64_counter("pawrly.query.total")
            .with_description("Total queries executed.")
            .with_unit("{query}")
            .build()
    })
}

/// Wall-clock query duration in milliseconds, by `interface` and `status`.
pub fn query_duration() -> &'static Histogram<f64> {
    static I: OnceLock<Histogram<f64>> = OnceLock::new();
    I.get_or_init(|| {
        meter()
            .f64_histogram("pawrly.query.duration")
            .with_description("Query execution time.")
            .with_unit("ms")
            .build()
    })
}

/// Rows returned per query, by `interface`.
pub fn query_rows_returned() -> &'static Histogram<u64> {
    static I: OnceLock<Histogram<u64>> = OnceLock::new();
    I.get_or_init(|| {
        meter()
            .u64_histogram("pawrly.query.rows_returned")
            .with_description("Rows returned per query.")
            .with_unit("{row}")
            .build()
    })
}

/// In-flight queries, by `interface`. Incremented at start, decremented when
/// the result stream completes or is dropped.
pub fn query_active() -> &'static UpDownCounter<i64> {
    static I: OnceLock<UpDownCounter<i64>> = OnceLock::new();
    I.get_or_init(|| {
        meter()
            .i64_up_down_counter("pawrly.query.active")
            .with_description("Queries currently executing.")
            .with_unit("{query}")
            .build()
    })
}

/// Semantic-model compile time in milliseconds, by `model`.
pub fn semantic_compile_duration() -> &'static Histogram<f64> {
    static I: OnceLock<Histogram<f64>> = OnceLock::new();
    I.get_or_init(|| {
        meter()
            .f64_histogram("pawrly.semantic.compile.duration")
            .with_description("Semantic query compile time.")
            .with_unit("ms")
            .build()
    })
}

/// Cache lookups, by `outcome` (`hit` / `miss` / `refetch`).
pub fn cache_requests() -> &'static Counter<u64> {
    static I: OnceLock<Counter<u64>> = OnceLock::new();
    I.get_or_init(|| {
        meter()
            .u64_counter("pawrly.cache.requests")
            .with_description("Cache lookups by outcome.")
            .with_unit("{request}")
            .build()
    })
}

/// Cache refresh time in milliseconds, by `source` and `status`.
pub fn cache_refresh_duration() -> &'static Histogram<f64> {
    static I: OnceLock<Histogram<f64>> = OnceLock::new();
    I.get_or_init(|| {
        meter()
            .f64_histogram("pawrly.cache.refresh.duration")
            .with_description("Cache refresh time.")
            .with_unit("ms")
            .build()
    })
}

/// Outbound source requests, by `source`, `kind`, and response status code.
pub fn source_request_total() -> &'static Counter<u64> {
    static I: OnceLock<Counter<u64>> = OnceLock::new();
    I.get_or_init(|| {
        meter()
            .u64_counter("pawrly.source.request.total")
            .with_description("Outbound requests to sources.")
            .with_unit("{request}")
            .build()
    })
}

/// Outbound source request duration in milliseconds, by `source`, `kind`, and
/// `status`.
pub fn source_request_duration() -> &'static Histogram<f64> {
    static I: OnceLock<Histogram<f64>> = OnceLock::new();
    I.get_or_init(|| {
        meter()
            .f64_histogram("pawrly.source.request.duration")
            .with_description("Outbound source request time.")
            .with_unit("ms")
            .build()
    })
}

/// Activity records dropped because the recorder's queue was full (back-pressure
/// is never applied to a query).
pub fn activity_dropped() -> &'static Counter<u64> {
    static I: OnceLock<Counter<u64>> = OnceLock::new();
    I.get_or_init(|| {
        meter()
            .u64_counter("pawrly.activity.dropped")
            .with_description("Activity records dropped due to a full queue.")
            .with_unit("{record}")
            .build()
    })
}

/// SQL redaction had to degrade (a redacting mode could not fully apply); the
/// stored capture is reduced, never the raw text.
pub fn redaction_failed() -> &'static Counter<u64> {
    static I: OnceLock<Counter<u64>> = OnceLock::new();
    I.get_or_init(|| {
        meter()
            .u64_counter("pawrly.activity.redaction_failed")
            .with_description("Activity SQL redaction degraded due to a parse failure.")
            .with_unit("{record}")
            .build()
    })
}

#[cfg(test)]
mod tests {
    use opentelemetry::KeyValue;

    use super::*;

    /// With no meter provider installed the global meter is a no-op: building
    /// and recording on every instrument must succeed without panicking, and the
    /// accessors must be reusable (each caches its instrument).
    #[test]
    fn instruments_are_noop_without_a_provider() {
        let attrs = [KeyValue::new("status", "ok")];
        query_total().add(1, &attrs);
        query_duration().record(1.0, &attrs);
        query_rows_returned().record(1, &[]);
        query_active().add(1, &[]);
        query_active().add(-1, &[]);
        semantic_compile_duration().record(1.0, &[]);
        cache_requests().add(1, &[]);
        cache_refresh_duration().record(1.0, &[]);
        source_request_total().add(1, &[]);
        source_request_duration().record(1.0, &[]);
        activity_dropped().add(1, &[]);
        redaction_failed().add(1, &[]);

        // Second call returns the cached instrument and still records.
        query_total().add(1, &attrs);
    }
}
