//! Kaleido self-evolving (refine) harness.
//!
//! This crate implements the pure, testable core of the prime-agent `refine`
//! harness: P0 (data layer `model` + `store`), P1 (proposal parsing /
//! validation via `proposal`, application via `apply`, and rollback via
//! `rollback`) and P2 (`plan`: the LLM planning layer behind the mockable
//! `LlmClient` abstraction). It is pure Rust with no HTTP calls; LLM calls go
//! through [`LlmClient`].
//!
//! NOTE: this is intentionally _not_ the memory harness
//! (`crates/kaleido-core/src/harness.rs`). They are unrelated features.

pub mod apply;
pub mod model;
pub mod plan;
pub mod proposal;
pub mod rollback;
pub mod store;

pub use apply::*;
pub use model::*;
pub use plan::*;
pub use proposal::*;
pub use rollback::*;
pub use store::*;