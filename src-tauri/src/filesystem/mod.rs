//! Authorized roots, safe paths, hashing, and read-only filesystem primitives.

mod bundle;
pub(crate) mod durable;
mod objects;
mod paths;

pub use bundle::{
    BundleCaps, BundleHashError, BundleStats, HashedBundle, hash_bundle, validate_bundle_symlinks,
};
pub use objects::{
    ObjectManifest, ObjectPublication, ObjectStore, ObjectStoreError, copy_bundle_exact,
};
pub use paths::{
    AuthorizedPath, AuthorizedRoot, EntryKind, MetadataFingerprint, PathIdentity, PathObservation,
    PathPolicyError,
};
