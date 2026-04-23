//! Priority-ordered API error classifier with typed recovery hints.
//!
//! This module maps raw provider failure signals — HTTP status codes and
//! optional body text — into a [`FailoverReason`] enum and a composable
//! [`RecoveryHint`] struct. Both are designed to be consumed by the retry
//! and recovery layer without requiring callers to perform their own ad hoc
//! string matching.
//!
//! # Classification priority
//!
//! The classifier evaluates conditions in a fixed priority order so that the
//! most actionable signal wins when multiple conditions could match:
//!
//! 1. **401** → [`FailoverReason::AuthFailure`] — rotate credential
//! 2. **402** + "quota" in body → [`FailoverReason::QuotaExhausted`] — fall back
//! 3. **402/429** + "rate" in body → [`FailoverReason::RateLimited`] — retryable
//! 4. **402/400** + "content" in body → [`FailoverReason::ContentPolicy`] — compress
//! 5. **5xx** → [`FailoverReason::ServerError`] — retryable
//! 6. **no status** (transport failure) → [`FailoverReason::TransportError`] — retryable
//! 7. **any** + "context"/"overflow" in body → [`FailoverReason::ContextOverflow`] — compress + fall back
//! 8. Fallthrough → [`FailoverReason::Unknown`]
//!
//! # Example
//!
//! ```
//! use hymenium::classify::classify_error;
//!
//! let (reason, hint) = classify_error(Some(401), None);
//! assert!(hint.should_rotate_credential);
//! ```

// ---------------------------------------------------------------------------
// FailoverReason
// ---------------------------------------------------------------------------

/// Typed taxonomy of API provider failure modes.
///
/// Use this to branch on *why* a request failed without re-parsing status
/// codes or body strings at each call site. The enum is `#[non_exhaustive]`
/// because the taxonomy may grow as new failure modes are discovered.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub enum FailoverReason {
    /// HTTP 401 — credentials are missing, expired, or revoked.
    AuthFailure,

    /// HTTP 402 with quota semantics in the response body.
    QuotaExhausted,

    /// HTTP 402 or 429 with rate-limit semantics in the response body.
    RateLimited,

    /// HTTP 402 or 400 with content-policy semantics in the response body.
    ContentPolicy,

    /// Network, DNS, or TLS failure with no HTTP status code.
    TransportError,

    /// Disconnect inferred to be caused by an oversized context window.
    ContextOverflow,

    /// HTTP 5xx — the server acknowledged the request but reported an error.
    ServerError,

    /// Failure mode not covered by any other variant.
    Unknown,
}

// ---------------------------------------------------------------------------
// RecoveryHint
// ---------------------------------------------------------------------------

/// Composable flags that advise the recovery layer on what to try next.
///
/// Multiple flags may be set simultaneously — for example, a context overflow
/// should both compress the session and fall back to a fresh provider request.
/// All flags default to `false`.
///
/// The four-bool design is intentional: each flag is an independent, orthogonal
/// recovery axis, not a state machine. They may all be true at once.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RecoveryHint {
    /// The request can be retried after a backoff delay.
    pub retryable: bool,

    /// Reducing context size (compression or summarisation) may resolve the failure.
    pub should_compress: bool,

    /// The current credential should be rotated before the next attempt.
    pub should_rotate_credential: bool,

    /// A different provider or model should be tried.
    pub should_fallback: bool,
}

// ---------------------------------------------------------------------------
// Classifier
// ---------------------------------------------------------------------------

/// Classify a provider failure into a [`FailoverReason`] and [`RecoveryHint`].
///
/// Both `status` and `body_hint` are optional so the function can be called
/// at any granularity of available information. The function never panics.
///
/// # Arguments
///
/// * `status` — the HTTP status code, if one was received.
/// * `body_hint` — a slice of the response body or error message to search
///   for semantic signals such as `"quota"`, `"rate"`, or `"content"`.
///
/// # Example
///
/// ```
/// use hymenium::classify::classify_error;
///
/// // 429 with a "rate limit" body → RateLimited + retryable
/// let (reason, hint) = classify_error(Some(429), Some("rate limit exceeded"));
/// assert!(hint.retryable);
/// assert!(!hint.should_rotate_credential);
/// ```
pub fn classify_error(
    status: Option<u16>,
    body_hint: Option<&str>,
) -> (FailoverReason, RecoveryHint) {
    let body_lower = body_hint.map(str::to_lowercase).unwrap_or_default();

    // 1. 401 — credential problem, highest priority.
    if status == Some(401) {
        return (
            FailoverReason::AuthFailure,
            RecoveryHint {
                should_rotate_credential: true,
                ..Default::default()
            },
        );
    }

    // 2. 402 + "quota" — quota exhausted, fall back to different provider.
    if status == Some(402) && body_lower.contains("quota") {
        return (
            FailoverReason::QuotaExhausted,
            RecoveryHint {
                should_fallback: true,
                ..Default::default()
            },
        );
    }

    // 3. 402 or 429 + "rate" — rate limited, retryable after backoff.
    if matches!(status, Some(402 | 429)) && body_lower.contains("rate") {
        return (
            FailoverReason::RateLimited,
            RecoveryHint {
                retryable: true,
                ..Default::default()
            },
        );
    }

    // 4. 402 or 400 + "content" — content policy violation, compression may help.
    if matches!(status, Some(400 | 402)) && body_lower.contains("content") {
        return (
            FailoverReason::ContentPolicy,
            RecoveryHint {
                should_compress: true,
                ..Default::default()
            },
        );
    }

    // 5. 5xx — server error, retryable.
    if let Some(code) = status {
        if code >= 500 {
            return (
                FailoverReason::ServerError,
                RecoveryHint {
                    retryable: true,
                    ..Default::default()
                },
            );
        }
    }

    // 6. No status — transport error (network, DNS, TLS), retryable.
    if status.is_none() {
        // Check for context/overflow signals before falling back to TransportError.
        if body_lower.contains("context") || body_lower.contains("overflow") {
            return (
                FailoverReason::ContextOverflow,
                RecoveryHint {
                    should_compress: true,
                    should_fallback: true,
                    ..Default::default()
                },
            );
        }

        return (
            FailoverReason::TransportError,
            RecoveryHint {
                retryable: true,
                ..Default::default()
            },
        );
    }

    // 7. Any status + "context" or "overflow" in body — context overflow.
    if body_lower.contains("context") || body_lower.contains("overflow") {
        return (
            FailoverReason::ContextOverflow,
            RecoveryHint {
                should_compress: true,
                should_fallback: true,
                ..Default::default()
            },
        );
    }

    // 8. Fallthrough — unknown.
    (FailoverReason::Unknown, RecoveryHint::default())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_failure_on_401() {
        let (reason, hint) = classify_error(Some(401), None);
        assert_eq!(reason, FailoverReason::AuthFailure);
        assert!(hint.should_rotate_credential);
        assert!(!hint.retryable);
        assert!(!hint.should_compress);
        assert!(!hint.should_fallback);
    }

    #[test]
    fn auth_failure_ignores_body() {
        // Even with quota/rate body text, 401 always maps to AuthFailure.
        let (reason, hint) = classify_error(Some(401), Some("quota exceeded"));
        assert_eq!(reason, FailoverReason::AuthFailure);
        assert!(hint.should_rotate_credential);
    }

    #[test]
    fn quota_exhausted_on_402_quota_body() {
        let (reason, hint) = classify_error(Some(402), Some("quota exceeded for this period"));
        assert_eq!(reason, FailoverReason::QuotaExhausted);
        assert!(hint.should_fallback);
        assert!(!hint.retryable);
    }

    #[test]
    fn rate_limited_on_402_rate_body() {
        let (reason, hint) = classify_error(Some(402), Some("rate limit exceeded"));
        assert_eq!(reason, FailoverReason::RateLimited);
        assert!(hint.retryable);
        assert!(!hint.should_fallback);
    }

    #[test]
    fn rate_limited_on_429_rate_body() {
        let (reason, hint) = classify_error(Some(429), Some("rate limit exceeded"));
        assert_eq!(reason, FailoverReason::RateLimited);
        assert!(hint.retryable);
    }

    #[test]
    fn rate_limited_on_429_no_body() {
        // 429 without a body hint that contains "rate" — does not match rule 3.
        // Falls through to Unknown because 429 is not 5xx.
        let (reason, _hint) = classify_error(Some(429), None);
        assert_eq!(reason, FailoverReason::Unknown);
    }

    #[test]
    fn content_policy_on_402_content_body() {
        let (reason, hint) = classify_error(Some(402), Some("content policy violation"));
        assert_eq!(reason, FailoverReason::ContentPolicy);
        assert!(hint.should_compress);
        assert!(!hint.retryable);
    }

    #[test]
    fn content_policy_on_400_content_body() {
        let (reason, hint) = classify_error(Some(400), Some("content blocked by policy"));
        assert_eq!(reason, FailoverReason::ContentPolicy);
        assert!(hint.should_compress);
    }

    #[test]
    fn server_error_on_500() {
        let (reason, hint) = classify_error(Some(500), None);
        assert_eq!(reason, FailoverReason::ServerError);
        assert!(hint.retryable);
    }

    #[test]
    fn server_error_on_503() {
        let (reason, hint) = classify_error(Some(503), Some("service unavailable"));
        assert_eq!(reason, FailoverReason::ServerError);
        assert!(hint.retryable);
    }

    #[test]
    fn transport_error_on_no_status() {
        let (reason, hint) = classify_error(None, None);
        assert_eq!(reason, FailoverReason::TransportError);
        assert!(hint.retryable);
        assert!(!hint.should_compress);
        assert!(!hint.should_fallback);
    }

    #[test]
    fn context_overflow_on_no_status_context_body() {
        let (reason, hint) = classify_error(None, Some("context window exceeded"));
        assert_eq!(reason, FailoverReason::ContextOverflow);
        assert!(hint.should_compress);
        assert!(hint.should_fallback);
    }

    #[test]
    fn context_overflow_on_no_status_overflow_body() {
        let (reason, hint) = classify_error(None, Some("token overflow detected"));
        assert_eq!(reason, FailoverReason::ContextOverflow);
        assert!(hint.should_compress);
        assert!(hint.should_fallback);
    }

    #[test]
    fn context_overflow_on_status_with_context_body() {
        // A status that does not match earlier rules but body signals context overflow.
        let (reason, hint) = classify_error(Some(413), Some("context too large"));
        assert_eq!(reason, FailoverReason::ContextOverflow);
        assert!(hint.should_compress);
        assert!(hint.should_fallback);
    }

    #[test]
    fn unknown_on_unrecognised_status_and_body() {
        let (reason, hint) = classify_error(Some(418), Some("I'm a teapot"));
        assert_eq!(reason, FailoverReason::Unknown);
        assert!(!hint.retryable);
        assert!(!hint.should_compress);
        assert!(!hint.should_rotate_credential);
        assert!(!hint.should_fallback);
    }

    #[test]
    fn unknown_on_none_status_empty_body() {
        // None status with empty body → TransportError (not Unknown), but
        // confirm Unknown is reachable via a status with no matching rule.
        let (reason, _) = classify_error(Some(418), None);
        assert_eq!(reason, FailoverReason::Unknown);
    }

    #[test]
    fn recovery_hint_default_all_false() {
        let hint = RecoveryHint::default();
        assert!(!hint.retryable);
        assert!(!hint.should_compress);
        assert!(!hint.should_rotate_credential);
        assert!(!hint.should_fallback);
    }

    #[test]
    fn classify_never_panics_on_extreme_inputs() {
        // Empty strings, very long body, unusual status codes.
        classify_error(Some(0), Some(""));
        classify_error(Some(u16::MAX), Some(&"x".repeat(10_000)));
        classify_error(None, Some(""));
        classify_error(Some(200), None);
    }

    #[test]
    fn body_matching_is_case_insensitive() {
        let (reason, _) = classify_error(Some(402), Some("QUOTA limit reached"));
        assert_eq!(reason, FailoverReason::QuotaExhausted);

        let (reason, _) = classify_error(Some(429), Some("Rate Limit Exceeded"));
        assert_eq!(reason, FailoverReason::RateLimited);
    }
}
