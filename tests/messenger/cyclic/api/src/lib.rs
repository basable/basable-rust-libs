//! The sends-only boundary of the cyclic topology.

use basable_app::Component;

#[derive(Debug, Default, Clone, Copy)]
pub struct Api;

impl Api {
    pub fn new() -> Self {
        Api
    }
}

/// Sends-only and no loops: the default.
impl<R> Component<R> for Api {}
