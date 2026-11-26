use crate::value::Value;

/// A script-visible error category with a built-in constructor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    Error,
    TypeError,
    RangeError,
    ReferenceError,
    SyntaxError,
    EvalError,
    URIError,
}

impl ErrorKind {
    /// Prototype `name` shown by the built-in constructor and allocations.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Error => "Error",
            Self::TypeError => "TypeError",
            Self::RangeError => "RangeError",
            Self::ReferenceError => "ReferenceError",
            Self::SyntaxError => "SyntaxError",
            Self::EvalError => "EvalError",
            Self::URIError => "URIError",
        }
    }

    /// Name of the built-in owner root holding the kind's prototype. This
    /// mirrors `error_class!` registrations in `error_family`.
    pub(crate) const fn builtin_root(self) -> &'static str {
        match self {
            Self::Error => "error.prototype",
            Self::TypeError => "typeerror.prototype",
            Self::RangeError => "rangeerror.prototype",
            Self::ReferenceError => "referenceerror.prototype",
            Self::SyntaxError => "syntaxerror.prototype",
            Self::EvalError => "evalerror.prototype",
            Self::URIError => "urierror.prototype",
        }
    }
}

/// Identifies the error object a host or the VM creates. Identity is the
/// declared class, not a script-visible `.name`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorClass {
    Builtin(ErrorKind),
    /// A host module's declared error subclass. The module and export name are
    /// checked when the host links its declarations.
    Host {
        module: &'static str,
        name: &'static str,
    },
}

impl From<ErrorKind> for ErrorClass {
    fn from(kind: ErrorKind) -> Self {
        Self::Builtin(kind)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum VmError {
    #[error("invalid bytecode build: {0}")]
    InvalidBuild(String),
    #[error("invalid VM snapshot: {0}")]
    InvalidSnapshot(String),
    #[error("missing host import binding: {0}")]
    MissingImport(String),
    #[error("missing host operation: {0}")]
    MissingOperation(String),
    #[error("unknown host operation: {0}")]
    UnknownOperation(String),
    #[error("host call failed: {0}")]
    Host(String),
    #[error("host contract fault: {0}")]
    HostFault(String),
    #[error("invalid VM state: {0}")]
    InvalidState(String),
    #[error("script threw: {0:?}")]
    Thrown(Value),
}

impl From<std::convert::Infallible> for VmError {
    fn from(error: std::convert::Infallible) -> Self {
        match error {}
    }
}
