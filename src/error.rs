//! Error definitions

use std::error::Error;
use std::fmt::{Display, Formatter, Result as FmtResult};
use std::sync::PoisonError;

use trace_error::*;

/// Shorthand result type for `EventError`s
pub type EventResult<T> = TraceResult<T, EventError>;

/// Error variants for event emitters
#[derive(Debug)]
pub enum EventError {
    /// Converted from a `PoisonError<T>` to remove the `T`.
    ///
    /// Still means the same thing, that a lock was poisoned by a panicking thread.
    PoisonError,
}

impl Display for EventError {
    fn fmt(&self, f: &mut Formatter) -> FmtResult {
        write!(f, "{}", self.description())
    }
}

impl Error for EventError {
    fn description(&self) -> &str {
        match *self {
            EventError::PoisonError => "Poison Error",
        }
    }
}

impl<T> From<PoisonError<T>> for EventError {
    fn from(_: PoisonError<T>) -> EventError {
        EventError::PoisonError
    }
}