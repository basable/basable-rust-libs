//! The SQLSTATE questions the frameworks ask of a `sqlx::Error`. Each is a
//! plain predicate so a caller matches on the fact, not on a string.

use sqlx::Error;

/// The five-character SQLSTATE of a database error, `None` for any other
/// kind of `sqlx::Error` (a pool timeout, a decode failure, a closed
/// connection).
pub fn of(err: &Error) -> Option<&str> {
    match err {
        Error::Database(db) => db.code().map(|c| match c {
            std::borrow::Cow::Borrowed(s) => s,
            // `code()` borrows from the error in every driver sqlx ships;
            // an owned code would need an allocation to hand out, and there
            // is none to hand out here. Treat it as absent rather than leak.
            std::borrow::Cow::Owned(_) => "",
        }),
        _ => None,
    }
    .filter(|c| !c.is_empty())
}

/// Whether the SQLSTATE class (its first two characters) is `class`.
pub fn is_class(err: &Error, class: &str) -> bool {
    of(err).is_some_and(|c| c.starts_with(class))
}

/// `23505`: a unique or primary-key constraint refused the row. The
/// frameworks' `ON CONFLICT`-free idempotency checks key on this.
pub fn is_unique_violation(err: &Error) -> bool {
    of(err) == Some("23505")
}

/// Class 23: any integrity-constraint violation (unique, foreign key, check,
/// not null). Fenced completion converts one on a status write into a loud
/// retry.
pub fn is_integrity_violation(err: &Error) -> bool {
    is_class(err, "23")
}

/// `42501`: the role may not touch the object. This is what a nanoservice
/// sees when it queries another nanoservice's table.
pub fn is_insufficient_privilege(err: &Error) -> bool {
    of(err) == Some("42501")
}

/// `40001`: a serialization failure; the transaction is safe to retry.
pub fn is_serialization_failure(err: &Error) -> bool {
    of(err) == Some("40001")
}

/// `42P01`: the relation does not exist (also what an unqualified name
/// outside the pinned `search_path` produces).
pub fn is_undefined_table(err: &Error) -> bool {
    of(err) == Some("42P01")
}
