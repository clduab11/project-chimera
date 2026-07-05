//! Prometheus metrics for Project Chimera.
//!
//! Exports the following metrics:
//! - `chimera_candidates_seen_total`: Counter of liquidation candidates from detector
//! - `chimera_sims_run_total`: Counter of simulations run
//! - `chimera_sim_latency_seconds`: Histogram of simulation latency
//! - `chimera_profit_usd`: Histogram of estimated profit in USD
//! - `chimera_revert_total`: Counter of reverts
//! - `chimera_breaker_state`: Gauge of breaker state (0=ok, 1=tripped)
//! - `chimera_gas_used`: Histogram of gas used per liquidation
//! - `chimera_l1_fee_wei`: Gauge of L1 data fee in wei
//! - `chimera_daily_net_usd`: Gauge of rolling 24h net USD
//! - `chimera_weekly_net_usd`: Gauge of rolling 7d net USD

use prometheus::{Gauge, GaugeVec, HistogramOpts, HistogramVec, IntCounterVec, Opts};

pub struct Metrics {
    pub candidates_seen: IntCounterVec,
    pub sims_run: IntCounterVec,
    pub sim_latency: HistogramVec,
    pub profit_usd: HistogramVec,
    pub reverts: IntCounterVec,
    pub breaker_state: GaugeVec,
    pub gas_used: HistogramVec,
    pub l1_fee_wei: Gauge,
    pub daily_net_usd: GaugeVec,
    pub weekly_net_usd: GaugeVec,
    pub sweep_total: IntCounterVec,
    pub sweep_skipped_breaker: IntCounterVec,
    pub sweep_amount_wei: Gauge,
}

impl Metrics {
    pub fn new() -> Self {
        // Register on the global default registry so `prometheus::gather()` in
        // `start_metrics_server` actually exports these metrics.
        let r = prometheus::default_registry();
        let candidates_seen = IntCounterVec::new(
            Opts::new(
                "chimera_candidates_seen_total",
                "Total liquidation candidates seen",
            ),
            &["chain"],
        )
        .unwrap();
        let sims_run = IntCounterVec::new(
            Opts::new("chimera_sims_run_total", "Total simulations run"),
            &["result"],
        )
        .unwrap();
        let sim_latency = HistogramVec::new(
            HistogramOpts::new(
                "chimera_sim_latency_seconds",
                "Simulation latency in seconds",
            )
            .buckets(vec![0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0]),
            &[],
        )
        .unwrap();
        let profit_usd = HistogramVec::new(
            HistogramOpts::new("chimera_profit_usd", "Estimated profit in USD")
                .buckets(vec![1.0, 10.0, 50.0, 100.0, 250.0, 500.0, 1000.0]),
            &[],
        )
        .unwrap();
        let reverts = IntCounterVec::new(
            Opts::new("chimera_revert_total", "Total reverts"),
            &["reason"],
        )
        .unwrap();
        let breaker_state = GaugeVec::new(
            Opts::new("chimera_breaker_state", "Breaker state: 0=ok, 1=tripped"),
            &["chain"],
        )
        .unwrap();
        let gas_used = HistogramVec::new(
            HistogramOpts::new("chimera_gas_used", "Gas used per liquidation").buckets(vec![
                100_000.0,
                200_000.0,
                500_000.0,
                1_000_000.0,
                2_000_000.0,
            ]),
            &[],
        )
        .unwrap();
        let l1_fee_wei = Gauge::new("chimera_l1_fee_wei", "L1 data fee in wei").unwrap();
        let daily_net_usd = GaugeVec::new(
            Opts::new("chimera_daily_net_usd", "Rolling 24h net USD"),
            &["chain"],
        )
        .unwrap();
        let weekly_net_usd = GaugeVec::new(
            Opts::new("chimera_weekly_net_usd", "Rolling 7d net USD"),
            &["chain"],
        )
        .unwrap();

        let sweep_total = IntCounterVec::new(
            Opts::new("chimera_sweep_total", "Total sweep/refund operations"),
            &["type"],
        )
        .unwrap();
        let sweep_skipped_breaker = IntCounterVec::new(
            Opts::new(
                "chimera_sweep_skipped_breaker",
                "Sweep/refund operations skipped due to breaker",
            ),
            &["type"],
        )
        .unwrap();
        let sweep_amount_wei =
            Gauge::new("chimera_sweep_amount_wei", "Last sweep amount in wei").unwrap();

        // Register all metrics
        r.register(Box::new(candidates_seen.clone())).ok();
        r.register(Box::new(sims_run.clone())).ok();
        r.register(Box::new(sim_latency.clone())).ok();
        r.register(Box::new(profit_usd.clone())).ok();
        r.register(Box::new(reverts.clone())).ok();
        r.register(Box::new(breaker_state.clone())).ok();
        r.register(Box::new(gas_used.clone())).ok();
        r.register(Box::new(l1_fee_wei.clone())).ok();
        r.register(Box::new(daily_net_usd.clone())).ok();
        r.register(Box::new(weekly_net_usd.clone())).ok();
        r.register(Box::new(sweep_total.clone())).ok();
        r.register(Box::new(sweep_skipped_breaker.clone())).ok();
        r.register(Box::new(sweep_amount_wei.clone())).ok();

        Self {
            candidates_seen,
            sims_run,
            sim_latency,
            profit_usd,
            reverts,
            breaker_state,
            gas_used,
            l1_fee_wei,
            daily_net_usd,
            weekly_net_usd,
            sweep_total,
            sweep_skipped_breaker,
            sweep_amount_wei,
        }
    }

    pub fn observe_candidate(&self, chain: &str) {
        self.candidates_seen.with_label_values(&[chain]).inc();
    }

    pub fn observe_candidates(&self, chain: &str, count: usize) {
        self.candidates_seen
            .with_label_values(&[chain])
            .inc_by(count as u64);
    }

    pub fn observe_sim(
        &self,
        result: &str,
        latency_secs: f64,
        gas: u64,
        l1_fee_wei: u64,
        profit_usd: f64,
    ) {
        self.sims_run.with_label_values(&[result]).inc();
        self.sim_latency
            .with_label_values(&[] as &[&str])
            .observe(latency_secs);
        self.gas_used
            .with_label_values(&[] as &[&str])
            .observe(gas as f64);
        self.l1_fee_wei.set(l1_fee_wei as f64);
        if result == "success" {
            self.profit_usd
                .with_label_values(&[] as &[&str])
                .observe(profit_usd);
        }
    }

    pub fn set_breaker_state(&self, chain: &str, tripped: bool) {
        self.breaker_state
            .with_label_values(&[chain])
            .set(if tripped { 1.0 } else { 0.0 });
    }

    pub fn set_daily_net_usd(&self, chain: &str, v: f64) {
        self.daily_net_usd.with_label_values(&[chain]).set(v);
    }

    pub fn set_weekly_net_usd(&self, chain: &str, v: f64) {
        self.weekly_net_usd.with_label_values(&[chain]).set(v);
    }

    pub fn observe_sweep(&self, sweep_type: &str, amount_wei: f64) {
        self.sweep_total.with_label_values(&[sweep_type]).inc();
        self.sweep_amount_wei.set(amount_wei);
    }

    pub fn observe_sweep_skipped(&self, sweep_type: &str) {
        self.sweep_skipped_breaker
            .with_label_values(&[sweep_type])
            .inc();
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Start the Prometheus metrics HTTP server on the given port.
/// Returns a handle that can be used to stop the server.
pub async fn start_metrics_server(port: u16) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    use prometheus::Encoder;
    use std::net::SocketAddr;
    use tokio::io::AsyncWriteExt;

    let addr: SocketAddr = ([0, 0, 0, 0], port).into();
    let listener = tokio::net::TcpListener::bind(addr).await?;

    let handle = tokio::spawn(async move {
        loop {
            if let Ok((mut stream, _)) = listener.accept().await {
                let encoder = prometheus::TextEncoder::new();
                let metric_families = prometheus::gather();
                let mut buffer = Vec::new();
                if encoder.encode(&metric_families, &mut buffer).is_ok() {
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/plain\r\n\r\n",
                        buffer.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.write_all(&buffer).await;
                }
            }
        }
    });

    Ok(handle)
}
