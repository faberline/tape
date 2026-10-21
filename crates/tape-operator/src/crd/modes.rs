//! Closed enums for the serving container's log format and request auth.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Log output format for the serving container (`TAPE_LOG_FORMAT`).
///
/// A closed enum for the same reason `AuthMode` is one: a typo in a
/// free-form string would fall back at boot instead of being rejected at
/// `kubectl apply` (#2582).
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    /// Human-readable formatter. The escape hatch for reading one pod's logs
    /// interactively during an incident.
    Pretty,
    /// The default in-cluster: the shared `axiom.service.log.v1` collector
    /// contract expects JSON.
    #[default]
    Json,
}

impl LogFormat {
    /// The `TAPE_LOG_FORMAT` value the serving binary expects (`pretty` | `json`).
    pub fn as_env(self) -> &'static str {
        match self {
            LogFormat::Pretty => "pretty",
            LogFormat::Json => "json",
        }
    }
}

/// Whether the data-plane API requires a bearer token.
///
/// A closed enum, not a flat string: as a `String` every value except the
/// exact literal `"required"` rendered an open data plane, so `auth: requried`
/// applied cleanly and the only signal was the *absence* of an env var in a
/// pod nobody inspects (#2765). The API server now rejects anything outside
/// this set, naming the field.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AuthMode {
    /// Open API (dev / trusted network) — an explicit opt-out, never the
    /// default. Serialized as `disabled` — NOT `off`, which YAML 1.1
    /// (kubectl / go-yaml) would parse as the boolean `false` and corrupt the
    /// CRD enum/default.
    #[serde(rename = "disabled")]
    Off,
    /// Bearer token required; the registry file comes from `tokensSecret` or
    /// `tokensSecretProviderClass`. The default, so a `Tape` that omits
    /// `spec.auth` fails startup asking for credentials instead of serving an
    /// open API silently.
    #[default]
    Required,
}

impl AuthMode {
    /// The `TAPE_AUTH` value the serving binary expects.
    ///
    /// Deliberately *not* the CRD spelling: the wire keeps `off` (see
    /// `auth::AUTH_MODE_ENV` and `tape serve --auth`), the schema uses
    /// `disabled`. An env var is never parsed as YAML, so `off` is safe there
    /// and changing it would break every existing deployment's serving args.
    pub fn as_env(self) -> &'static str {
        match self {
            AuthMode::Off => "off",
            AuthMode::Required => "required",
        }
    }
}
