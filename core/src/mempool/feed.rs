//! SequencerFeed — pluggable sequencer-tier mempool watcher that falls back
//! to block-watch when the feed is unavailable.
//!
//! A `SequencerFeed` wraps a [`BlockWatch`](super::BlockWatch) as its fallback
//! and optionally a feed source closure. When the feed produces events, those
//! are returned; on error or if no feed is configured, `BlockWatch` is used.

use crate::mempool::{BlockWatch, MempoolWatcher, WatchEvent};
use crate::ChimeraError;
use async_trait::async_trait;
use std::{pin::Pin, sync::Arc};
use tracing::{info, warn};

/// A pluggable feed source that produces [`WatchEvent`] values.
///
/// Implementors might stream pending transactions from a sequencer's mempool
/// endpoint, poll a custom API, or bridge proprietary feeds. The closure
/// pattern keeps this trivially pluggable without requiring an additional trait.
pub type FeedSource = Arc<
    dyn Fn() -> Pin<Box<dyn std::future::Future<Output = Result<WatchEvent, ChimeraError>> + Send>>
        + Send
        + Sync,
>;

/// A [`MempoolWatcher`] that attempts to use a fast feed first, falling back
/// to [`BlockWatch`] when the feed is unavailable, errors, or is not configured.
pub struct SequencerFeed {
    chain_id: u64,
    block_watch: BlockWatch,
    feed: Option<FeedSource>,
}

impl SequencerFeed {
    /// Create a `SequencerFeed` that wraps the given `BlockWatch` as a
    /// fallback and attempts the `feed` source first on each trigger.
    ///
    /// Pass `None` for `feed` to always use block-watch (useful for
    /// deployment where no feed is available yet).
    pub async fn new(
        chain_id: u64,
        ws_url: String,
        feed: Option<FeedSource>,
    ) -> Result<Self, ChimeraError> {
        let block_watch = BlockWatch::new(chain_id, ws_url).await?;
        Ok(Self {
            chain_id,
            block_watch,
            feed,
        })
    }

    /// Create a `SequencerFeed` from an already-constructed `BlockWatch`.
    pub fn from_block_watch(block_watch: BlockWatch, feed: Option<FeedSource>) -> Self {
        let chain_id = block_watch.chain_id();
        Self {
            chain_id,
            block_watch,
            feed,
        }
    }
}

#[async_trait]
impl MempoolWatcher for SequencerFeed {
    async fn wait_for_trigger(&self) -> Result<WatchEvent, ChimeraError> {
        // Try the fast feed first if configured.
        if let Some(ref feed) = self.feed {
            let feed_future = feed();
            match feed_future.await {
                Ok(event) => {
                    return Ok(event);
                }
                Err(e) => {
                    warn!(
                        target = "chimera::mempool",
                        chain_id = self.chain_id,
                        error = %e,
                        "SequencerFeed: feed error, falling back to BlockWatch"
                    );
                    // Fall through to block-watch below.
                }
            }
        }

        // Fallback: block-watch fires on each new block header.
        info!(
            target = "chimera::mempool",
            chain_id = self.chain_id,
            "SequencerFeed: using BlockWatch fallback"
        );
        self.block_watch.wait_for_trigger().await
    }

    fn chain_id(&self) -> u64 {
        self.chain_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trivial test feed that always returns a synthetic event.
    async fn test_feed() -> Result<WatchEvent, ChimeraError> {
        Ok(WatchEvent::NewTransaction {
            hash: alloy::primitives::B256::ZERO,
        })
    }

    /// Verifies that `FeedSource` can be constructed from a closure.
    #[test]
    fn test_feed_source_constructible() {
        let source: FeedSource = Arc::new(|| Box::pin(test_feed()));
        let _ = source; // just verify type compiles
    }
}
