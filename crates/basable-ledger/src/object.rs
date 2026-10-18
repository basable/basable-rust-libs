//! The reader: how the engine reads the user's object. The object is the
//! user's own type, with whatever fields it needs; the trait exposes its
//! identity, its version, the source's change time and its one accounting
//! state. Nothing about money: that is [`Accounting`](crate::Accounting).

use std::fmt::Debug;
use std::hash::Hash;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::types::{ObjectId, ProcessId, Timestamp};

/// One input snapshot of one business object, of the user's own type.
///
/// The object is serialized to JSON for its snapshot diff and read back from
/// the folded JSON when the process is read again, so the type's serde form
/// is its stored form: adding a field is free, renaming one changes its key
/// and needs an upgrade of the stored diffs.
pub trait LedgerObject: Serialize + DeserializeOwned + Send + Sync + 'static {
    /// The object type's name, `lower_snake_case`; stored with every diff
    /// so the fold knows which reader to apply.
    const TYPE: &'static str;

    /// The one accounting state an object is in. A `match` over it is
    /// exhaustive, which is what makes a desired state complete.
    type State: Copy + Eq + Hash + Debug + Send + Sync + 'static;

    /// The business process the object belongs to; a root object's own id.
    fn process_id(&self) -> ProcessId;

    /// The object's id, unique within the process.
    fn object_id(&self) -> ObjectId;

    /// The parent within the same process, if any. Never changes once set.
    fn parent_id(&self) -> Option<ObjectId> {
        None
    }

    /// The producer's version: strictly increasing per object. Higher books,
    /// equal is a duplicate, lower is stale.
    fn version(&self) -> u64;

    /// When the source last changed the object; the timeliness input.
    fn changed_at(&self) -> Timestamp;

    /// The accounting state the object is in.
    fn state(&self) -> Self::State;
}
