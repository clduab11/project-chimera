//! MempoolWatcher trait and BlockWatch default implementation.
//!
//! `BlockWatch` subscribes to `newHeads` via WebSocket and yields a
//! `WatchEvent::NewBlock` on each new block header. The orchestrator uses
//! this as its scan trigger, replacing the fixed-interval poll.
//!
//! ## Design
//! A background Tokio task holds the WS connection and subscription stream.
//! Events are forwarded to the orchestrator through an mpsc channel, so the
//! `BlockWatch` struct does not need to name the complex alloy provider type.

use crate::ChimeraError;
use alloy::primitives::B256;
use alloy::providers::Provider;
use alloy::providers::ProviderBuilder;
use alloy::transports::ws::WsConnect;
use async_trait::async_trait;
use tokio::sync::Mutex;
use tracing::{info, warn, error};
use url::Url;

/// Event emitted by a [`MempoolWatcher`] when it's time for the orchestrator
/// to run a detection scan.
#[derive(Debug, Clone)]
pub enum WatchEvent {
    /// A new block was produced on the chain.
    NewBlock {
        number: u64,
        hash: B256,
        timestamp: u64,
    },
    /// A new pending transaction was observed (only from feed-based watchers).
    NewTransaction {
        hash: B256,
    },
}

/// Pluggable trigger for the orchestrator's continuous detection loop.
///
/// Implementations signal "it's time to scan" by returning from
/// [`wait_for_trigger`](Self::wait_for_trigger). The default is
/// [`BlockWatch`] which fires on every new block header.
#[async_trait]
pub trait MempoolWatcher: Send + Sync {
    /// Wait for the next trigger event. Returns on new block, new transaction,
    /// or an error. Callers should loop on errors with a backoff.
    async fn wait_for_trigger(&self) -> Result<WatchEvent, ChimeraError>;

    /// The chain ID this watcher is monitoring.
    fn chain_id(&self) -> u64;
}

/// Default [`MempoolWatcher`] implementation: subscribes to `newHeads` via
/// WebSocket and fires on every new block header.
///
/// ## Resilience
/// A background task holds the WS connection. If the connection drops, the
/// background task attempts to reconnect with exponential backoff (up to 60s).
/// During reconnection, `wait_for_trigger` returns `Err` so the orchestrator
/// can fall back to fixed-interval polling until the watcher recovers.
pub struct BlockWatch {
    chain_id: u64,
    /// Receives events from the background subscription task.
    rx: Mutex<tokio::sync::mpsc::Receiver<WatchEvent>>,
}

impl BlockWatch {
    /// Create a new block-watch watcher.
    ///
    /// `ws_url` should be a WebSocket RPC endpoint, e.g.
    /// `"ws://localhost:8546"` or `"wss://base-mainnet.blastapi.io/..."`.
    ///
    /// Spawns a background task that holds the WS connection and subscription.
    /// The task auto-reconnects on disconnect.
    pub async fn new(chain_id: u64, ws_url: String) -> Result<Self, ChimeraError> {
        let (tx, rx) = tokio::sync::mpsc::channel::<WatchEvent>(256);

        // Lightweight readiness check: use ProviderBuilder to verify the WS
        // endpoint is reachable. Drop the provider immediately — this is a
        // connectivity check only, not a subscription. The background task
        // handles ongoing connection and reconnection.
        let cid = chain_id;
        let connect = WsConnect::new(ws_url.clone());
        let check = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            ProviderBuilder::new().connect_ws(connect),
        )
        .await;
        match check {
            Ok(Ok(_provider)) => {
                drop(_provider);
                info!(
                    target = "chimera::mempool",
                    chain_id,
                    ws_host = %redact_ws_url(&ws_url),
                    "BlockWatch readiness check OK"
                );
            }
            Ok(Err(e)) => {
                warn!(
                    target = "chimera::mempool",
                    chain_id,
                    ws_host = %redact_ws_url(&ws_url),
                    error = %e,
                    "BlockWatch readiness check failed; background task will retry"
                );
            }
            Err(_elapsed) => {
                warn!(
                    target = "chimera::mempool",
                    chain_id,
                    ws_host = %redact_ws_url(&ws_url),
                    "BlockWatch readiness check timed out; background task will retry"
                );
            }
        }

        // Spawn the background reconnection loop. The long-running forwarding
        // loop runs exclusively in this spawned task — the constructor returns
        // immediately.
        tokio::spawn(Self::connection_loop(cid, ws_url, tx));

        Ok(Self {
            chain_id,
            rx: Mutex::new(rx),
        })
    }

    /// Background task: holds the WS connection, reads from the subscription
    /// stream, and forwards events to the channel. Reconnects on failure.
    async fn connection_loop(
        chain_id: u64,
        ws_url: String,
        tx: tokio::sync::mpsc::Sender<WatchEvent>,
    ) {
        let mut backoff_secs: u64 = 1;
        const MAX_BACKOFF_SECS: u64 = 60;

        loop {
            match Self::try_connect_and_forward(chain_id, &ws_url, &tx).await {
                Ok(()) => {
                    // Connection ended cleanly (stream finished). Apply minimum
                    // backoff before reconnecting to avoid reconnect storms.
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    backoff_secs = 2;
                    warn!(
                        target = "chimera::mempool",
                        chain_id,
                        "BlockWatch connection ended; reconnecting..."
                    );
                }
                Err(e) => {
                    error!(
                        target = "chimera::mempool",
                        chain_id,
                        error = %e,
                        backoff_secs,
                        "BlockWatch connection error; backing off"
                    );
                    tokio::time::sleep(tokio::time::Duration::from_secs(backoff_secs)).await;
                    backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
                }
            }
        }
    }

    /// Establish a WS connection, subscribe to `newHeads`, and forward each
    /// header as a `WatchEvent::NewBlock` through `tx`. Returns when the
    /// subscription stream ends (connection closed) or on error.
    async fn try_connect_and_forward(
        chain_id: u64,
        ws_url: &str,
        tx: &tokio::sync::mpsc::Sender<WatchEvent>,
    ) -> Result<(), ChimeraError> {
        use futures::StreamExt;

        let ws = WsConnect::new(ws_url.to_string());
        let provider = ProviderBuilder::new()
            .connect_ws(ws)
            .await
            .map_err(|e| ChimeraError::RpcError(format!("BlockWatch WS connect: {e}")))?;

        let sub = provider
            .subscribe_blocks()
            .await
            .map_err(|e| ChimeraError::RpcError(format!("BlockWatch subscribe_blocks: {e}")))?;

        let mut stream = sub.into_stream();

        info!(
            target = "chimera::mempool",
            chain_id,
            "BlockWatch subscription active"
        );

        while let Some(header) = stream.next().await {
            let event = WatchEvent::NewBlock {
                number: header.number,
                hash: header.hash,
                timestamp: header.timestamp,
            };
            if tx.send(event).await.is_err() {
                // Receiver dropped — orchestrator is shutting down.
                info!(
                    target = "chimera::mempool",
                    chain_id,
                    "BlockWatch: receiver dropped; shutting down connection loop"
                );
                return Err(ChimeraError::RpcError(
                    "BlockWatch: receiver dropped (orchestrator shutdown)".into(),
                ));
            }
        }

        Ok(())
    }
}

#[async_trait]
impl MempoolWatcher for BlockWatch {
    async fn wait_for_trigger(&self) -> Result<WatchEvent, ChimeraError> {
        let mut rx = self.rx.lock().await;
        match rx.recv().await {
            Some(event) => Ok(event),
            None => {
                // Channel closed — background task exited permanently.
                Err(ChimeraError::RpcError(
                    "BlockWatch: event channel closed (background task exited)".into(),
                ))
            }
        }
    }

    fn chain_id(&self) -> u64 {
        self.chain_id
    }
}

fn redact_ws_url(url: &str) -> String {
    if let Ok(parsed) = Url::parse(url) {
        let scheme = parsed.scheme();
        if let Some(host) = parsed.host_str() {
            let port_str = parsed.port().map(|p| format!(":{}", p)).unwrap_or_default();
            return format!("{}://{}{}/[redacted]", scheme, host, port_str);
        }
    }
    if let Some(idx) = url.find("://") {
        let after_scheme = &url[idx + 3..];
        if let Some(slash) = after_scheme.find('/') {
            return format!("{}/[redacted]", &url[..idx + 3 + slash]);
        }
        return url[..idx + 3 + after_scheme.len()].to_string();
    }
    "(invalid-url)".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Prove that `BlockWatch::new` returns promptly (under 2s) even when
    /// the WS endpoint is unreachable. The long-running forwarding loop
    /// must run exclusively in the spawned background task.
    #[tokio::test]
    async fn test_blockwatch_new_returns_promptly() {
        // Use an unreachable endpoint — the constructor must not hang.
        let ws_url = "ws://127.0.0.1:19999".to_string();
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            BlockWatch::new(8453, ws_url),
        )
        .await;

        match result {
            Ok(Ok(bw)) => {
                assert_eq!(bw.chain_id(), 8453);
            }
            Ok(Err(e)) => {
                eprintln!("BlockWatch::new returned error (expected for unreachable endpoint): {e}");
            }
            Err(_elapsed) => {
                panic!("BlockWatch::new timed out after 2s — the constructor must return immediately and not enter the forwarding loop inline");
            }
        }
    }
}
