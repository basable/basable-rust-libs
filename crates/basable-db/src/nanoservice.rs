//! The markers a nanoservice crate puts on a unit type so the framework can
//! name its role and schema at the type level.

/// A nanoservice, by name. `NAME` is the crate's snake_case name and doubles
/// as the suffix of its role and schema (`nano_<NAME>`), so it follows the
/// registry naming rule ([`basable_core::names::validate_type_name`]): the
/// scaffolder refuses anything else before a crate exists.
pub trait Nanoservice {
    /// The nanoservice's name, as declared in `routing.yaml`.
    const NAME: &'static str;
}

/// A nanoservice that owns data: a schema, a role and therefore a pool. A
/// nanoservice that owns nothing does not implement this, and
/// [`crate::NanoPool`] requires it, so a pool for such a nanoservice is a
/// compile error rather than an empty schema.
pub trait Stateful: Nanoservice {
    /// The Postgres role the nanoservice's connections run as.
    fn role() -> String {
        format!("nano_{}", Self::NAME)
    }

    /// The schema the role owns and every connection's `search_path` names.
    fn schema() -> String {
        format!("nano_{}", Self::NAME)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Orders;
    impl Nanoservice for Orders {
        const NAME: &'static str = "orders";
    }
    impl Stateful for Orders {}

    #[test]
    fn role_and_schema_follow_the_name() {
        assert_eq!(Orders::role(), "nano_orders");
        assert_eq!(Orders::schema(), "nano_orders");
    }
}
