//! Generated protobuf types (buffa, under `proto`) and Connect service stubs
//! (under `connect`), mounted from the build's output tree one module pair
//! per proto package. Edit `proto/**/*.proto`, never these modules. The
//! scaffolder adds a nanoservice's package between the markers.

#![allow(missing_docs, clippy::all)]

pub mod proto {
    pub mod health {
        pub mod v1 {
            include!("../generated/health_v1/buffa/health.v1.rs");
        }
    }
    pub mod basable {
        pub mod config {
            pub mod v1 {
                include!("../generated/basable_config_v1/buffa/basable.config.v1.rs");
            }
        }
    }
    // basable:proto-modules-begin
    pub mod catalog {
        pub mod v1 {
            include!("../generated/catalog_v1/buffa/catalog.v1.rs");
        }
    }
    pub mod order {
        pub mod v1 {
            include!("../generated/order_v1/buffa/order.v1.rs");
        }
    }
    // basable:proto-modules-end
}

pub mod connect {
    pub mod health {
        pub mod v1 {
            include!("../generated/health_v1/connect/health.v1.rs");
        }
    }
    // basable:connect-modules-begin
    pub mod catalog {
        pub mod v1 {
            include!("../generated/catalog_v1/connect/catalog.v1.rs");
        }
    }
    pub mod order {
        pub mod v1 {
            include!("../generated/order_v1/connect/order.v1.rs");
        }
    }
    // basable:connect-modules-end
}
