//! The tape journal context: the use cases every serving path drives
//! ([`application`]) and the HTTP data plane, OpenAPI document, and spec
//! document ([`interfaces`]). The journal model itself is the shared kernel
//! (`tape-shared-kernel`).

pub mod application;
pub mod interfaces;
