//! Exposes provider-independent search capabilities.
mod cache;
mod catalog;
mod engine;
mod federation;
mod fetch;
mod search_provider;

pub use cache::{WebCache, WebCacheEntry};
pub use engine::{HtmlEngine, JsonEngine, SearchEngine, SearchHit};
pub use federation::{SearchError, WebSearch};
pub use fetch::{FetchError, FetchedPage, WebFetch};
pub mod tgrep;
pub mod tgrep_registry;

pub use tgrep_registry::{ServerReadiness, TgrepRegistry};
pub(crate) enum TgrepOutcome {
    Match(String),
    NoMatch,
    Failure(TgrepFailure),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TgrepFailure {
    Unavailable,
    UnsupportedRequest { reason: String },
    Infrastructure { reason: String },
    InvalidRequest { reason: String },
}

impl TgrepOutcome {
    pub(crate) fn should_auto_fallback(&self) -> bool {
        matches!(
            self,
            Self::Failure(
                TgrepFailure::Unavailable
                    | TgrepFailure::UnsupportedRequest { .. }
                    | TgrepFailure::Infrastructure { .. }
            )
        )
    }
}

#[cfg(test)]
mod contract_tests {
    use super::{TgrepFailure, TgrepOutcome};

    #[test]
    fn no_match_is_successful_and_never_falls_back() {
        let outcome = TgrepOutcome::NoMatch;
        assert!(!outcome.should_auto_fallback());
    }

    #[test]
    fn infrastructure_failures_are_eligible_for_auto_fallback() {
        let outcome = TgrepOutcome::Failure(TgrepFailure::Infrastructure {
            reason: "process exited unexpectedly".into(),
        });
        assert!(outcome.should_auto_fallback());
    }

    #[test]
    fn unsupported_requests_are_eligible_for_auto_fallback() {
        let outcome = TgrepOutcome::Failure(TgrepFailure::UnsupportedRequest {
            reason: "offset is unsupported".into(),
        });
        assert!(outcome.should_auto_fallback());
    }

    #[test]
    fn invalid_requests_never_fall_back() {
        let outcome = TgrepOutcome::Failure(TgrepFailure::InvalidRequest {
            reason: "type must not be empty".into(),
        });
        assert!(!outcome.should_auto_fallback());
    }
}

pub(crate) use search_provider::execute as execute_semble;
