//! The messages of the cyclic topology.

/// Asks `a`; `depth` is how many more hops the answer takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ping {
    pub depth: u32,
}

/// Asks `b`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pong {
    pub depth: u32,
}

/// The number of handler calls the answer went through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Count {
    pub hops: u32,
}

/// A fail-fast error fan-out: the handler named by `fail_at` fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub fail_at: Option<&'static str>,
}
