use fastrace::collector::SpanContext;

/// Return the active local span context or create an uncorrelated local one.
///
/// Atelier never serializes this value into outbound HTTP/gRPC headers.
pub fn local_or_random_span_ctx() -> SpanContext {
    SpanContext::current_local_parent().unwrap_or_else(SpanContext::random)
}
