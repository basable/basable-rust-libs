//! The sends-only boundary of the cyclic topology.

#[derive(Debug, Default, Clone, Copy)]
pub struct Api;

impl Api {
    pub fn new() -> Self {
        Api
    }
}
