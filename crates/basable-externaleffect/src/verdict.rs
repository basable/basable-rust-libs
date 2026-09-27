//! Judging a failed send: did the receiver decide, or is its state
//! unknowable? The classifier, the baseline classifiers, and the marker
//! errors a provider wraps its answers in at its boundary.

use std::error::Error;
use std::fmt;
use std::time::Duration;

use basable_core::BoxError;

/// The classification of a failed send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The receiver's state is unknowable — a timeout, a cancellation, a
    /// dead transport, an ack that never arrived. The effect MAY have
    /// landed: keep the marker / replay the same key, never re-send blind.
    Ambiguous,
    /// The receiver decided — a structured rejection whose answer will not
    /// change on retry. Nothing landed (or the answer is final); the caller
    /// may consume the failure.
    Definitive,
}

/// Judges one provider's send errors. It must be pure (no I/O) and must
/// classify transport failures ambiguous — `basable-effecttest` asserts
/// that floor mechanically. Definitive is a claim about the RECEIVER ("it
/// decided"), so a classifier only refines definitively-structured provider
/// answers on top of [`Classifier::transport`]; it never reclassifies a
/// transport failure as definitive.
pub struct Classifier(Box<dyn Fn(&BoxError) -> Verdict + Send + Sync>);

impl Classifier {
    /// A classifier from a judgement function.
    pub fn new(judge: impl Fn(&BoxError) -> Verdict + Send + Sync + 'static) -> Classifier {
        Classifier(Box::new(judge))
    }

    /// The baseline classifier for REVERSIBLE effects: a transport failure
    /// ([`is_transport`]) leaves the receiver state unknowable; everything
    /// else is taken as a structured answer from the receiver. That default
    /// is wrong for an irreversible effect — a truncated response or a
    /// gateway 5xx after a paid POST is a plain error, which this baseline
    /// would consume as "never landed" — so those use
    /// [`Classifier::fail_closed`] and refine ITS predicate instead. A
    /// reversible adapter refines this baseline the other way round — one
    /// more provider answer it must HOLD rather than consume, such as an
    /// "already armed" that converges and is no rejection:
    ///
    /// ```
    /// use basable_externaleffect::{Classifier, Verdict, classify_transport};
    /// let already_armed = Classifier::new(|err| {
    ///     if err.to_string().contains("already armed") {
    ///         return Verdict::Ambiguous;
    ///     }
    ///     classify_transport(err)
    /// });
    /// ```
    pub fn transport() -> Classifier {
        Classifier::new(classify_transport)
    }

    /// The classifier an IRREVERSIBLE effect must use: an error is
    /// definitive only when `is_definitive` proves the receiver did not
    /// act; every other failure — a plain error, a truncated response body,
    /// an HTTP 5xx page, a decode failure — is ambiguous, because the
    /// textbook ack-loss shape (request executed, response cut) arrives as
    /// a plain error. For a paid order or a destructive write, mis-calling
    /// that "definitive" tombstones or re-sends a landed effect; mis-calling
    /// a real rejection "ambiguous" merely holds a slot the resolver
    /// settles. `basable-effecttest` enforces the floor for irreversible
    /// adapters: a plain error must classify ambiguous.
    pub fn fail_closed(
        is_definitive: impl Fn(&BoxError) -> bool + Send + Sync + 'static,
    ) -> Classifier {
        Classifier::new(move |err| {
            if is_definitive(err) {
                Verdict::Definitive
            } else {
                Verdict::Ambiguous
            }
        })
    }

    /// [`Classifier::fail_closed`] over the [`DefinitiveError`] marker —
    /// the classifier for irreversible effects against providers that wrap
    /// their structured rejections with [`definitive`].
    pub fn fail_closed_on_definitive() -> Classifier {
        Classifier::fail_closed(|err| is_definitive(&**err))
    }

    /// The verdict for `err`.
    pub fn judge(&self, err: &BoxError) -> Verdict {
        (self.0)(err)
    }
}

impl fmt::Debug for Classifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Classifier")
    }
}

/// The baseline verdict ([`Classifier::transport`]) as a plain function,
/// for a refining classifier to fall back on.
pub fn classify_transport(err: &BoxError) -> Verdict {
    if is_transport(&**err) {
        Verdict::Ambiguous
    } else {
        Verdict::Definitive
    }
}

/// Whether `err` (anywhere in its source chain) is a transport failure —
/// the request may or may not have reached the receiver: a
/// [`TransportError`] a provider wrapped at its boundary, the [`TimedOut`]
/// and [`Cancelled`] markers a bounded dispatch produces, or an I/O error.
pub fn is_transport(err: &(dyn Error + 'static)) -> bool {
    chain(err).any(|e| {
        e.is::<TransportError>()
            || e.is::<TimedOut>()
            || e.is::<Cancelled>()
            || e.is::<std::io::Error>()
    })
}

/// Whether `err` (anywhere in its chain) carries a provider's proof of
/// non-execution.
pub fn is_definitive(err: &(dyn Error + 'static)) -> bool {
    chain(err).any(|e| e.is::<DefinitiveError>())
}

/// Wraps `err` as a proven non-execution.
pub fn definitive(err: impl Into<BoxError>) -> BoxError {
    Box::new(DefinitiveError(err.into()))
}

fn chain<'a>(err: &'a (dyn Error + 'static)) -> impl Iterator<Item = &'a (dyn Error + 'static)> {
    std::iter::successors(Some(err), |e: &&'a (dyn Error + 'static)| (*e).source())
}

/// Marks a provider answer that PROVES the effect did not execute — a
/// structured 4xx rejection, a validation failure before any send.
/// Providers wrap exactly those at their boundary ([`definitive`]) so an
/// irreversible effect's classifier can be fail-closed: definitive only on
/// this proof, ambiguous for everything else. `Display` is verbatim;
/// `source` keeps the provider error.
#[derive(Debug)]
pub struct DefinitiveError(BoxError);

impl DefinitiveError {
    /// The provider's error.
    pub fn inner(&self) -> &BoxError {
        &self.0
    }
}

impl fmt::Display for DefinitiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl Error for DefinitiveError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&*self.0)
    }
}

/// Marks a failure of the TRANSPORT — the request may or may not have
/// reached the receiver (a refused connection, a reset after the request
/// was written, a client-side timeout). Rust has no `net.Error`: a
/// provider client wraps its transport-shaped failures with this at its
/// boundary, and [`classify_transport`] holds them. `Display` is verbatim;
/// `source` keeps the provider error.
#[derive(Debug)]
pub struct TransportError(BoxError);

impl TransportError {
    /// Wraps `err` as a transport failure.
    pub fn wrap(err: impl Into<BoxError>) -> BoxError {
        Box::new(TransportError(err.into()))
    }

    /// The provider's error.
    pub fn inner(&self) -> &BoxError {
        &self.0
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl Error for TransportError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&*self.0)
    }
}

/// A dispatch that reached its bound (the smaller of the adapter's
/// `call_timeout` and the remaining ownership deadline) before the send
/// answered. The send was dropped mid-flight: ambiguous by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimedOut {
    /// The bound that elapsed.
    pub after: Duration,
}

impl fmt::Display for TimedOut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "call timed out after {:?}", self.after)
    }
}

impl Error for TimedOut {}

/// A dispatch whose context was cancelled before the send answered. The
/// send was dropped mid-flight: ambiguous by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cancelled;

impl fmt::Display for Cancelled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("call cancelled")
    }
}

impl Error for Cancelled {}
