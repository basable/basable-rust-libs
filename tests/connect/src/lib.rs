//! The generated crate of the connect test: the buffa message types under
//! `proto` and the Connect service traits and clients under `connect`,
//! both regenerated from `proto/` on every build (`tools/proto.bzl`) and
//! mounted here by package. A tenant's `crates/proto/src/lib.rs` has the
//! same shape, one module pair per proto package.

#![allow(missing_docs)]

pub mod proto {
    pub mod basable {
        pub mod connecttest {
            pub mod v1 {
                include!("../generated/basable_connecttest_v1/buffa/basable.connecttest.v1.rs");
            }
        }
    }
}

pub mod connect {
    pub mod basable {
        pub mod connecttest {
            pub mod v1 {
                include!("../generated/basable_connecttest_v1/connect/basable.connecttest.v1.rs");
            }
        }
    }
}
