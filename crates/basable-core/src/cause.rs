//! `Cause`: the error a framework call carries when the caller needs the
//! source chain and nothing else — the Rust spelling of Go's `error` value
//! in the adapter and reconciler signatures.
//!
//! A `Cause` is what a reconciler returns from a failed pass (`Retry{cause}`)
//! and what an external-effect `Send` reports; the classifier walks its
//! source chain to decide whether the receiver decided (definitive) or its
//! state is unknowable (ambiguous). It is a plain box: no code, no HTTP
//! status. The boundary's typed error is [`crate::AppError`].
//!
//! Like `anyhow::Error`, `Cause` does not itself implement `std::error::Error`
//! — that is what lets every error convert into it with `?` (the reflexive
//! `From<Cause> for Cause` would otherwise collide). It derefs to the boxed
//! error, so `source()` and downcasting are one `*` away.

use std::error::Error;
use std::fmt;
use std::ops::Deref;

/// A boxed error with its source chain.
pub struct Cause(Box<dyn Error + Send + Sync + 'static>);

impl Cause {
    /// Wraps any error.
    pub fn new<E: Error + Send + Sync + 'static>(err: E) -> Cause {
        Cause(Box::new(err))
    }

    /// A cause that is only a message.
    pub fn msg(message: impl Into<String>) -> Cause {
        Cause(Box::new(Message(message.into())))
    }

    /// A cause wrapping another with context, the `fmt.Errorf("%s: %w")`
    /// shape: `Display` prints `context: inner`, and the inner error stays
    /// reachable through the source chain.
    pub fn context(context: impl Into<String>, inner: Cause) -> Cause {
        Cause(Box::new(Context {
            context: context.into(),
            inner,
        }))
    }

    /// The outermost error.
    pub fn inner(&self) -> &(dyn Error + Send + Sync + 'static) {
        self.0.as_ref()
    }

    /// The source chain, outermost first, like `errors.Is`'s walk.
    pub fn chain(&self) -> impl Iterator<Item = &(dyn Error + 'static)> {
        let mut next: Option<&(dyn Error + 'static)> = Some(self.0.as_ref());
        std::iter::from_fn(move || {
            let cur = next?;
            next = cur.source();
            Some(cur)
        })
    }

    /// Whether any error in the chain is a `T` — `errors.As` as a predicate.
    pub fn is<T: Error + 'static>(&self) -> bool {
        self.find::<T>().is_some()
    }

    /// The first `T` in the chain, outermost first — `errors.As`.
    pub fn find<T: Error + 'static>(&self) -> Option<&T> {
        self.chain().find_map(|e| e.downcast_ref::<T>())
    }

    /// Consumes the cause and returns the box.
    pub fn into_inner(self) -> Box<dyn Error + Send + Sync + 'static> {
        self.0
    }
}

impl fmt::Debug for Cause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl fmt::Display for Cause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl Deref for Cause {
    type Target = dyn Error + Send + Sync + 'static;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}

impl<E: Error + Send + Sync + 'static> From<E> for Cause {
    fn from(err: E) -> Cause {
        Cause::new(err)
    }
}

#[derive(Debug)]
struct Message(String);

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for Message {}

#[derive(Debug)]
struct Context {
    context: String,
    inner: Cause,
}

impl fmt::Display for Context {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.context, self.inner)
    }
}

impl Error for Context {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.inner.inner() as &(dyn Error + 'static))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Timeout;
    impl fmt::Display for Timeout {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("timed out")
        }
    }
    impl Error for Timeout {}

    #[test]
    fn context_wraps_and_the_chain_stays_reachable() {
        let c = Cause::context(
            "putting record",
            Cause::context("dns api", Cause::new(Timeout)),
        );
        assert_eq!(c.to_string(), "putting record: dns api: timed out");
        assert_eq!(c.chain().count(), 3);
        assert!(c.is::<Timeout>());
        assert!(c.find::<Timeout>().is_some());
        assert!(!c.is::<std::fmt::Error>());
    }

    #[test]
    fn a_message_is_a_leaf() {
        let c = Cause::msg("nothing to do");
        assert_eq!(c.to_string(), "nothing to do");
        assert_eq!(c.chain().count(), 1);
    }

    #[test]
    fn any_error_converts() {
        fn fails() -> Result<(), Cause> {
            let n: i32 = "x".parse()?;
            let _ = n;
            Ok(())
        }
        let err = fails().unwrap_err();
        assert!(err.is::<std::num::ParseIntError>());
    }
}
