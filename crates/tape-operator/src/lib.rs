//! K8s operator for tape: a `Tape` custom resource ([`crd`]) plus a
//! reconcile loop ([`reconcile`]) that renders ([`render`]) tape's single
//! raft-group topology — ServiceAccount, headless + client Services,
//! PodDisruptionBudget, and the downward-API StatefulSet raft-runtime consumes.
//! Behind the `operator` feature; the service image enables it because that
//! same image also runs the checked-in operator Deployment.
//! [`render`] constructs the desired child objects without cluster I/O;
//! [`reconcile`] connects Tape's `ManagedService` implementation to the shared
//! Kubernetes controller.
//!
//! ```text
//! Tape (tape.dev/v1alpha1)  --reconcile-->  ServiceAccount, StatefulSet,
//!                                           headless + client Service,
//!                                           PodDisruptionBudget
//! ```

pub mod crd;
pub mod reconcile;
pub mod render;

pub use crd::{
    crd_yaml, AuthMode, LocalObjectReference, LogFormat, Tape, TapeBackupSpec, TapePlacement,
    TapeSpec, TapeStatus, Toleration,
};
pub use reconcile::run;
