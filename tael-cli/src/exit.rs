//! Category-encoded process exit codes.
//!
//! An agent driving tael in a shell pipeline should be able to branch on *why*
//! a command failed without parsing stderr. Every failure therefore maps to a
//! stable code, and the JSON error body on stdout stays the detailed
//! explanation rather than the only signal.
//!
//! | Code | Meaning                                                    |
//! |------|------------------------------------------------------------|
//! | 0    | Success                                                     |
//! | 1    | Unclassified failure                                        |
//! | 2    | Query succeeded but matched nothing                         |
//! | 3    | Malformed query, filter, or argument                        |
//! | 4    | Server unreachable                                          |
//! | 5    | Authentication or authorization failure                     |
//! | 6    | A `--exit-on` condition tripped (see `tael watch`)          |
//!
//! Code 2 deserves a note: an empty result set is not an error, and the command
//! still prints a well-formed empty response. It gets a distinct code purely so
//! `tael query traces --status error && echo "found errors"` does the obvious
//! thing, the way `grep` behaves.

use std::fmt;

/// Why a command stopped, mapped to a process exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCategory {
    Success,
    Failure,
    NoResults,
    BadQuery,
    Unreachable,
    Unauthorized,
    ConditionMet,
}

impl ExitCategory {
    pub fn code(self) -> i32 {
        match self {
            ExitCategory::Success => 0,
            ExitCategory::Failure => 1,
            ExitCategory::NoResults => 2,
            ExitCategory::BadQuery => 3,
            ExitCategory::Unreachable => 4,
            ExitCategory::Unauthorized => 5,
            ExitCategory::ConditionMet => 6,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ExitCategory::Success => "success",
            ExitCategory::Failure => "failure",
            ExitCategory::NoResults => "no_results",
            ExitCategory::BadQuery => "bad_query",
            ExitCategory::Unreachable => "unreachable",
            ExitCategory::Unauthorized => "unauthorized",
            ExitCategory::ConditionMet => "condition_met",
        }
    }
}

/// An error that carries its exit category.
///
/// Commands return plain `anyhow::Error` for the common case; wrapping in this
/// type is how a call site says "this specific failure has a code".
#[derive(Debug)]
pub struct CategorizedError {
    pub category: ExitCategory,
    pub message: String,
}

impl fmt::Display for CategorizedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CategorizedError {}

impl CategorizedError {
    pub fn new(category: ExitCategory, message: impl Into<String>) -> Self {
        Self {
            category,
            message: message.into(),
        }
    }

    /// A query that ran fine and matched nothing. Silent by design — the
    /// command has already printed its (empty) result.
    pub fn no_results() -> anyhow::Error {
        Self::new(ExitCategory::NoResults, String::new()).into()
    }
}

/// Classify an error into an exit category.
///
/// An explicit [`CategorizedError`] anywhere in the chain wins. Otherwise the
/// category is inferred from a `reqwest` transport or status error, so the
/// dozens of call sites that just use `?` on an HTTP call still produce useful
/// codes without each one classifying by hand.
pub fn categorize(err: &anyhow::Error) -> ExitCategory {
    for cause in err.chain() {
        if let Some(categorized) = cause.downcast_ref::<CategorizedError>() {
            return categorized.category;
        }
        if let Some(req) = cause.downcast_ref::<reqwest::Error>() {
            if req.is_connect() || req.is_timeout() {
                return ExitCategory::Unreachable;
            }
            if let Some(status) = req.status() {
                return match status.as_u16() {
                    401 | 403 => ExitCategory::Unauthorized,
                    400 | 422 => ExitCategory::BadQuery,
                    404 => ExitCategory::NoResults,
                    503 => ExitCategory::Unreachable,
                    _ => ExitCategory::Failure,
                };
            }
        }
    }
    ExitCategory::Failure
}

/// Run the CLI and translate its result into a process exit.
///
/// Errors print as a single JSON object on stderr so an agent parsing tael's
/// output gets the same shape from a failure as from a success, then the
/// process exits with the matching category code.
pub fn finish(result: anyhow::Result<()>) -> std::process::ExitCode {
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            let category = categorize(&err);
            let message = err.to_string();
            if !message.is_empty() {
                eprintln!(
                    "{}",
                    serde_json::json!({
                        "error": message,
                        "category": category.as_str(),
                        "exit_code": category.code(),
                    })
                );
            }
            std::process::ExitCode::from(category.code() as u8)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_stable() {
        // These are a published contract for scripts and agents; changing one
        // silently breaks callers branching on it.
        assert_eq!(ExitCategory::Success.code(), 0);
        assert_eq!(ExitCategory::Failure.code(), 1);
        assert_eq!(ExitCategory::NoResults.code(), 2);
        assert_eq!(ExitCategory::BadQuery.code(), 3);
        assert_eq!(ExitCategory::Unreachable.code(), 4);
        assert_eq!(ExitCategory::Unauthorized.code(), 5);
        assert_eq!(ExitCategory::ConditionMet.code(), 6);
    }

    #[test]
    fn explicit_category_survives_context_wrapping() {
        let err = anyhow::Error::from(CategorizedError::new(ExitCategory::BadQuery, "bad filter"))
            .context("while querying traces");
        assert_eq!(categorize(&err), ExitCategory::BadQuery);
    }

    #[test]
    fn unclassified_errors_fall_back_to_failure() {
        let err = anyhow::anyhow!("something went wrong");
        assert_eq!(categorize(&err), ExitCategory::Failure);
    }
}
