//! Dynamic filter pushdown — extension point.
//!
//! The trait surface and the optimizer-rule scaffold exist; the actual
//! rewrite (collecting build-side keys at runtime + re-issuing the
//! probe scan with an `IN(...)` filter) is not yet implemented.

/// Whether dynamic filter pushdown is enabled. Reads `OptimizerDefaults`
/// (default false in v1; flips true after the runtime rewrite lands).
pub fn dynamic_filter_pushdown_enabled(defaults: &pawrly_config::OptimizerDefaults) -> bool {
    defaults.dynamic_filter_pushdown
}

/// Inspects an `Arc<dyn TableProvider>` and returns the columns it can
/// absorb as runtime filters, if it implements
/// [`pawrly_core::DynamicFilterCapable`].
pub fn capable_columns(
    provider: &std::sync::Arc<dyn datafusion::datasource::TableProvider>,
) -> Vec<String> {
    use pawrly_core::DynamicFilterCapable;
    // Try the known concrete types. (The trait isn't object-safe via
    // `as_any`'s dyn-Any path because `DynamicFilterCapable` is a separate
    // trait; we downcast to each kind we know about. New kinds are added as
    // they implement the trait.)
    let any = provider.as_any();
    if let Some(p) = any.downcast_ref::<pawrly_sources_http::HttpTableProvider>() {
        return p.dynamic_filter_columns();
    }
    Vec::new()
}
