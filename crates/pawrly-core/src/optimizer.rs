//! Dynamic filter pushdown extension point.
//!
//! Sources opt in by implementing [`DynamicFilterCapable`]. The optimizer
//! rule (`DynamicFilterRule` in `pawrly-engine`) only inspects sources that
//! implement this trait when deciding whether to inject a runtime
//! `IN(...)` filter into the probe side of a hash join.
//!
//! The trait and the toggle exist. The runtime rewrite (collecting
//! build-side keys, materialising `IN`-list bytes, re-issuing the underlying
//! source scan with the augmented filter) is not yet implemented.

/// A `TableProvider` that can absorb a dynamic `<column> IN (...)` filter
/// at runtime, after the build side of a hash join completes.
///
/// `dynamic_filter_columns` returns the names of columns the source can
/// accept as runtime filters. The optimizer matches join keys against this
/// set when deciding whether to inject a `DynamicFilterExec` operator.
pub trait DynamicFilterCapable {
    /// The columns this source can absorb as runtime `IN(...)` filters.
    fn dynamic_filter_columns(&self) -> Vec<String>;
}
