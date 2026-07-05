//! Mempool watcher module — triggers candidate detection on new block/event
//! instead of a fixed polling interval.
//!
//! ## Design
//! - [`MempoolWatcher`] trait: a pluggable async trigger for the orchestrator scan loop.
//! - [`BlockWatch`] (default): subscribes to `newHeads` via WebSocket, returns
//!   immediately on each new block header.
//! - [`SequencerFeed`]: wraps a block-watch fallback and optionally a feed source,
//!   falling back to block-watch when the feed is unavailable or errors.

pub mod feed;
pub mod watcher;

pub use feed::SequencerFeed;
pub use watcher::{BlockWatch, MempoolWatcher, WatchEvent};
