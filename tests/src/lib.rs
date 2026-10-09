//! This crate exists only to host the integration tests in `integration/`.
//!
//! Its purpose is to consume `heirvault` the way an **external** consumer would:
//! through the crate's public ABI, with no access to internal modules. If a test
//! here compiles and passes, then the generated contract client and the
//! re-exported types are usable from an SDK, an indexer or a frontend — which
//! the in-crate unit tests cannot prove, because they can reach private items.
