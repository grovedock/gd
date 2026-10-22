//! Grovedock domain kernel.
//!
//! Everything here is pure domain logic over the ports in [`ports`]; no transport,
//! database or cloud SDK is referenced. Infrastructure lives in `grovedock-adapters`.

pub mod acl;
pub mod chunked;
pub mod delta;
mod derived;
pub mod env;
mod error;
pub mod flags;
pub mod gitai;
mod id;
pub mod object;
pub mod path;
pub mod ports;
mod repo;
pub mod tree;

pub use derived::GraphInfo;
pub use derived::{Derivation, Generation, RootTree};
pub use error::{Error, Result};
pub use id::Id;
pub use repo::{Repo, SYSTEM_REF_PREFIX, paths_overlap, validate_ref_name};
