//! `AppError`: the application error with the sixteen Connect codes.
//!
//! The Go original (`golang/controller/lib/messages/apperror`) has nine
//! codes of its own that the server framework maps onto gRPC statuses. This
//! port carries the wire vocabulary directly — the Connect protocol's error
//! codes, which are the gRPC codes — because a tenant's API boundary is
//! connect-rust and `basable-connect` translates an `AppError` one to one.
//! The Go constructors keep their names where the meaning is the same
//! (`not_found`, `already_exists`, `internal`, `failed_precondition`) and
//! take the Connect name where Go's was local (`invalid_input` →
//! [`AppError::invalid_argument`], `unauthorized` →
//! [`AppError::unauthenticated`], `not_implemented` →
//! [`AppError::unimplemented`], `conflict` → [`AppError::failed_precondition`],
//! which is also what Go's server mapped `CodeConflict` to).

use std::error::Error;
use std::fmt;

/// Any error, boxed with its source chain: the standard library's own
/// spelling of "an error the caller will only display or walk". A framework
/// call returns it when no consumer decides on the error's kind (a
/// reconciler's retry cause, a provider's transport failure), and
/// [`AppError`] holds one as its source. Where a consumer DOES decide, the
/// error is a typed enum instead ([`crate::names::InvalidName`],
/// [`crate::labels::InvalidLabel`]). `?` converts any `std::error::Error`,
/// a `String` or a `&str` into it.
pub type BoxError = Box<dyn Error + Send + Sync + 'static>;

/// The Connect error codes (identical to gRPC's), with the HTTP status the
/// Connect protocol assigns each in the unary error response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Code {
    /// The operation was canceled, typically by the caller.
    Canceled = 1,
    /// An unknown error; the catch-all for errors that carry no code.
    Unknown = 2,
    /// The client specified an invalid argument.
    InvalidArgument = 3,
    /// The deadline expired before the operation could complete.
    DeadlineExceeded = 4,
    /// A requested entity was not found.
    NotFound = 5,
    /// The entity a client tried to create already exists.
    AlreadyExists = 6,
    /// The caller does not have permission to execute the operation.
    PermissionDenied = 7,
    /// A resource has been exhausted (a quota, a rate limit, disk).
    ResourceExhausted = 8,
    /// The system is not in a state required for the operation.
    FailedPrecondition = 9,
    /// The operation was aborted, typically a concurrency issue.
    Aborted = 10,
    /// The operation was attempted past the valid range.
    OutOfRange = 11,
    /// The operation is not implemented or not supported.
    Unimplemented = 12,
    /// An internal error: an invariant the system expected to hold broke.
    Internal = 13,
    /// The service is currently unavailable; retrying may succeed.
    Unavailable = 14,
    /// Unrecoverable data loss or corruption.
    DataLoss = 15,
    /// The request does not have valid authentication credentials.
    Unauthenticated = 16,
}

impl Code {
    /// Every code, in wire order.
    pub const ALL: [Code; 16] = [
        Code::Canceled,
        Code::Unknown,
        Code::InvalidArgument,
        Code::DeadlineExceeded,
        Code::NotFound,
        Code::AlreadyExists,
        Code::PermissionDenied,
        Code::ResourceExhausted,
        Code::FailedPrecondition,
        Code::Aborted,
        Code::OutOfRange,
        Code::Unimplemented,
        Code::Internal,
        Code::Unavailable,
        Code::DataLoss,
        Code::Unauthenticated,
    ];

    /// The Connect protocol's `snake_case` name, as it appears in the JSON
    /// error body (`"code": "not_found"`).
    pub fn name(self) -> &'static str {
        match self {
            Code::Canceled => "canceled",
            Code::Unknown => "unknown",
            Code::InvalidArgument => "invalid_argument",
            Code::DeadlineExceeded => "deadline_exceeded",
            Code::NotFound => "not_found",
            Code::AlreadyExists => "already_exists",
            Code::PermissionDenied => "permission_denied",
            Code::ResourceExhausted => "resource_exhausted",
            Code::FailedPrecondition => "failed_precondition",
            Code::Aborted => "aborted",
            Code::OutOfRange => "out_of_range",
            Code::Unimplemented => "unimplemented",
            Code::Internal => "internal",
            Code::Unavailable => "unavailable",
            Code::DataLoss => "data_loss",
            Code::Unauthenticated => "unauthenticated",
        }
    }

    /// The code for a Connect name, or `None`.
    pub fn from_name(name: &str) -> Option<Code> {
        Code::ALL.into_iter().find(|c| c.name() == name)
    }

    /// The HTTP status the Connect protocol uses for this code in a unary
    /// error response.
    pub fn http_status(self) -> u16 {
        match self {
            Code::Canceled => 499,
            Code::Unknown | Code::Internal | Code::DataLoss => 500,
            Code::InvalidArgument | Code::OutOfRange => 400,
            Code::DeadlineExceeded => 504,
            Code::NotFound => 404,
            Code::AlreadyExists | Code::Aborted => 409,
            Code::PermissionDenied => 403,
            Code::ResourceExhausted => 429,
            Code::FailedPrecondition => 412,
            Code::Unimplemented => 501,
            Code::Unavailable => 503,
            Code::Unauthenticated => 401,
        }
    }

    /// The numeric gRPC status value.
    pub fn as_u8(self) -> u8 {
        self as u8
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The stable discriminator a FRONTEND matches on to start the hosted
/// card-setup flow instead of showing a generic error. A payment-required
/// failure is a [`Code::FailedPrecondition`] whose message CONTAINS this
/// text; the Go original had the same magic string for the same reason (the
/// gateway renders the code as a plain status with no structured detail).
pub const PAYMENT_REQUIRED_MESSAGE: &str = "payment method required";

/// The canonical application error: a code, a message, and optionally the
/// error that caused it, reachable through [`Error::source`].
#[derive(Debug)]
pub struct AppError {
    code: Code,
    message: String,
    source: Option<BoxError>,
}

impl AppError {
    /// An error with a code and a message and no source.
    pub fn new(code: Code, message: impl Into<String>) -> AppError {
        AppError {
            code,
            message: message.into(),
            source: None,
        }
    }

    /// An error with a code and a message wrapping the error that caused it,
    /// the `Wrap*` constructors of the Go original.
    pub fn wrap(code: Code, message: impl Into<String>, source: impl Into<BoxError>) -> AppError {
        AppError {
            code,
            message: message.into(),
            source: Some(source.into()),
        }
    }

    /// The code.
    pub fn code(&self) -> Code {
        self.code
    }

    /// The message without the source.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Attaches the error that caused this one.
    pub fn with_source(mut self, source: impl Into<BoxError>) -> AppError {
        self.source = Some(source.into());
        self
    }

    /// The code of any error: the first `AppError` in its source chain (the
    /// error itself included), or [`Code::Internal`] when there is none —
    /// `apperror.GetCode`. The chain is walked through [`Error::source`], so
    /// an `AppError` wrapped by another crate's error keeps its code.
    pub fn code_of(err: &(dyn Error + 'static)) -> Code {
        AppError::find(err).map_or(Code::Internal, |e| e.code)
    }

    /// The first `AppError` in an error's source chain, the error itself
    /// included — `apperror.GetError`.
    pub fn find<'a>(err: &'a (dyn Error + 'static)) -> Option<&'a AppError> {
        std::iter::successors(Some(err), |&e| e.source()).find_map(|e| e.downcast_ref::<AppError>())
    }

    /// Whether this is the payment-required failure the frontend recognises.
    pub fn is_payment_required(&self) -> bool {
        self.code == Code::FailedPrecondition && self.message.contains(PAYMENT_REQUIRED_MESSAGE)
    }
}

macro_rules! constructors {
    ($($(#[$doc:meta])* $name:ident => $code:ident),* $(,)?) => {
        impl AppError {
            $(
                $(#[$doc])*
                pub fn $name(message: impl Into<String>) -> AppError {
                    AppError::new(Code::$code, message)
                }
            )*
        }
    };
}

constructors! {
    /// [`Code::Canceled`].
    canceled => Canceled,
    /// [`Code::Unknown`].
    unknown => Unknown,
    /// [`Code::InvalidArgument`] — Go's `InvalidInput`.
    invalid_argument => InvalidArgument,
    /// [`Code::DeadlineExceeded`].
    deadline_exceeded => DeadlineExceeded,
    /// [`Code::NotFound`].
    not_found => NotFound,
    /// [`Code::AlreadyExists`].
    already_exists => AlreadyExists,
    /// [`Code::PermissionDenied`].
    permission_denied => PermissionDenied,
    /// [`Code::ResourceExhausted`].
    resource_exhausted => ResourceExhausted,
    /// [`Code::FailedPrecondition`] — also Go's `Conflict`.
    failed_precondition => FailedPrecondition,
    /// [`Code::Aborted`].
    aborted => Aborted,
    /// [`Code::OutOfRange`].
    out_of_range => OutOfRange,
    /// [`Code::Unimplemented`] — Go's `NotImplemented`.
    unimplemented => Unimplemented,
    /// [`Code::Internal`].
    internal => Internal,
    /// [`Code::Unavailable`].
    unavailable => Unavailable,
    /// [`Code::DataLoss`].
    data_loss => DataLoss,
    /// [`Code::Unauthenticated`] — Go's `Unauthorized`.
    unauthenticated => Unauthenticated,
}

impl AppError {
    /// The payment-required failure: a [`Code::FailedPrecondition`] whose
    /// message starts with [`PAYMENT_REQUIRED_MESSAGE`], so the frontend can
    /// recognise it. `detail` says what needed the payment method.
    pub fn payment_required(detail: impl AsRef<str>) -> AppError {
        let detail = detail.as_ref();
        let message = if detail.is_empty() {
            PAYMENT_REQUIRED_MESSAGE.to_owned()
        } else {
            format!("{PAYMENT_REQUIRED_MESSAGE}: {detail}")
        };
        AppError::new(Code::FailedPrecondition, message)
    }
}

/// `Display` includes the source, against the usual advice, because the
/// platform logs errors as `%err` and carries no error reporter that would
/// print the chain; the message alone would drop the cause from every log
/// line. [`AppError::message`] is the message by itself.
impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.source {
            Some(source) => write!(f, "{}: {}", self.message, source),
            None => f.write_str(&self.message),
        }
    }
}

impl Error for AppError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.as_ref().map(|e| &**e as &(dyn Error + 'static))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sixteen_codes_with_unique_names_and_wire_values() {
        assert_eq!(Code::ALL.len(), 16);
        for (i, c) in Code::ALL.iter().enumerate() {
            assert_eq!(c.as_u8() as usize, i + 1);
            assert_eq!(Code::from_name(c.name()), Some(*c));
        }
        assert_eq!(Code::from_name("teapot"), None);
        assert_eq!(Code::NotFound.http_status(), 404);
        assert_eq!(Code::Unauthenticated.http_status(), 401);
        assert_eq!(Code::FailedPrecondition.http_status(), 412);
    }

    #[test]
    fn display_and_source_follow_the_go_shape() {
        let plain = AppError::not_found("tenant t1 not found");
        assert_eq!(plain.to_string(), "tenant t1 not found");
        assert!(plain.source().is_none());

        let wrapped = AppError::wrap(Code::Internal, "loading tenant", "connection reset");
        assert_eq!(wrapped.to_string(), "loading tenant: connection reset");
        assert_eq!(wrapped.source().unwrap().to_string(), "connection reset");
    }

    #[test]
    fn code_of_walks_the_source_chain_and_defaults_to_internal() {
        // Another crate's error wrapping an AppError: found through source().
        #[derive(Debug)]
        struct Creating(AppError);
        impl fmt::Display for Creating {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("creating")
            }
        }
        impl Error for Creating {
            fn source(&self) -> Option<&(dyn Error + 'static)> {
                Some(&self.0)
            }
        }

        let chain = Creating(AppError::invalid_argument("bad name"));
        assert_eq!(AppError::code_of(&chain), Code::InvalidArgument);
        assert_eq!(AppError::find(&chain).unwrap().message(), "bad name");

        let direct = AppError::not_found("gone");
        assert_eq!(AppError::code_of(&direct), Code::NotFound);

        let plain: BoxError = "plain".into();
        assert_eq!(AppError::code_of(&*plain), Code::Internal);
        let parse: BoxError = "x".parse::<i32>().unwrap_err().into();
        assert_eq!(AppError::code_of(&*parse), Code::Internal);
    }

    #[test]
    fn payment_required_is_recognisable() {
        let e = AppError::payment_required("creating a project above the free tier");
        assert_eq!(e.code(), Code::FailedPrecondition);
        assert!(e.is_payment_required());
        assert!(e.message().starts_with(PAYMENT_REQUIRED_MESSAGE));
        assert!(!AppError::failed_precondition("no rail").is_payment_required());
    }
}
