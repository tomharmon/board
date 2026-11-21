//! Machine-readable rejections. Changing diagnostic wording must not change HTTP or CLI behavior.
use anyhow::Error;
use rusqlite::{Error as SqlError, ErrorCode};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Invalid,
    Missing,
    Forbidden,
    Conflict,
    Gone,
    Busy,
    Internal,
    Timeout,
}

#[derive(Debug)]
pub struct Rejection {
    kind: Kind,
    code: &'static str,
    detail: String,
}

impl Kind {
    pub fn error(self, code: &'static str, detail: impl Into<String>) -> Rejection {
        Rejection {
            kind: self,
            code,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.detail)
    }
}

impl std::error::Error for Rejection {}

pub fn classify(err: &Error) -> (Kind, &'static str) {
    use Kind::*;
    if let Some(r) = err.downcast_ref::<Rejection>() {
        return (r.kind, r.code);
    }
    if err.is::<crate::automation::NothingReady>() {
        return (Conflict, "nothing_ready");
    }
    if let Some(sql) = err.downcast_ref::<SqlError>() {
        return match sql {
            SqlError::QueryReturnedNoRows => (Missing, "not_found"),
            SqlError::SqliteFailure(e, _) => match e.code {
                ErrorCode::ConstraintViolation => (Conflict, "constraint_conflict"),
                ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => (Busy, "database_busy"),
                _ => (Internal, "database_error"),
            },
            _ => (Internal, "database_error"),
        };
    }
    if err.is::<std::io::Error>() {
        return (Internal, "io_error");
    }
    (Invalid, "invalid_request")
}
