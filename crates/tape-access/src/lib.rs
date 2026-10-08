//! Who may call tape: bearer authentication through the shared
//! `service-auth` contract and per-topic authorization.

mod authorization;

pub use authorization::{
    authorize, AuthConfig, AUTH_MODE_ENV, LEGACY_TOKENS_ENV, TOKEN_REGISTRY_FILE_ENV,
};
