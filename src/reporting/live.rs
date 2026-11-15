//! Live view of a pass (`[Regency] live`): updates are streamed to the endpoint
//! while the pass runs (`ReportLive`, best effort) and kept in a bounded
//! [`RunLog`] sent with the outcome.

use std::time::Duration;

use regent_sdk::hosts::live::{LiveSink, LiveUpdate, RunLog};
use regent_sdk::proto::ReportLiveRequest;
use regent_sdk::proto::grpc::reporting::reporting_client::ReportingClient;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use super::tls;
use crate::config::RegencyConfig;

/// Updates waiting for the forwarder (the pass never waits: beyond it, dropped).
const SINK_CAPACITY: usize = 4096;
/// Batches waiting for the endpoint (beyond it, dropped).
const BATCH_QUEUE: usize = 64;
/// A batch is sent when it holds this many updates…
const BATCH_MAX: usize = 128;
/// …or when its oldest update waited this long.
const BATCH_DELAY: Duration = Duration::from_millis(200);
/// Once the pass is over, how long the last batches may take to leave.
const FINISH_GRACE: Duration = Duration::from_secs(5);

/// One pass's live view; [`LivePass::finish`] once the pass is over (after the
/// task dropped its sink).
pub struct LivePass {
    collector: JoinHandle<RunLog>,
}

impl LivePass {
    /// The sink to give to the task, and the pass collecting its updates.
    pub fn start(config: &RegencyConfig, host_id: &str) -> (LiveSink, Self) {
        let (sink, updates) = LiveSink::channel(SINK_CAPACITY);
        let config = config.clone();
        let host_id = host_id.to_string();
        let collector = tokio::spawn(collect(config, host_id, updates));
        (sink, Self { collector })
    }

    /// Wait for the last updates; returns the pass's log.
    pub async fn finish(self) -> RunLog {
        self.collector.await.unwrap_or_else(|details| {
            warn!(%details, "Live view collector failed");
            RunLog::new(0)
        })
    }
}

async fn collect(
    config: RegencyConfig,
    host_id: String,
    mut updates: mpsc::Receiver<LiveUpdate>,
) -> RunLog {
    let mut log = RunLog::new(config.run_log_max_bytes);
    let keep_log = config.run_log_max_bytes > 0;
    let (batches, sender) = if config.live {
        let (tx, rx) = mpsc::channel(BATCH_QUEUE);
        (
            Some(tx),
            Some(tokio::spawn(send(config.clone(), host_id, rx))),
        )
    } else {
        (None, None)
    };
    let mut batch = ReportLiveRequest::new();
    let mut deadline: Option<tokio::time::Instant> = None;
    loop {
        let update = match deadline {
            None => updates.recv().await,
            Some(at) => tokio::select! {
                update = updates.recv() => update,
                _ = tokio::time::sleep_until(at) => {
                    flush(batches.as_ref(), &mut batch);
                    deadline = None;
                    continue;
                }
            },
        };
        let Some(update) = update else { break };
        if batches.is_some() {
            batch.updates_mut().push(update.to_proto());
            if batch.updates().len() >= BATCH_MAX {
                flush(batches.as_ref(), &mut batch);
                deadline = None;
            } else if deadline.is_none() {
                deadline = Some(tokio::time::Instant::now() + BATCH_DELAY);
            }
        }
        if keep_log {
            log.push(update);
        }
    }
    flush(batches.as_ref(), &mut batch);
    drop(batches);
    if let Some(sender) = sender
        && tokio::time::timeout(FINISH_GRACE, sender).await.is_err()
    {
        debug!("Live view: last updates not sent in time");
    }
    if !keep_log {
        return RunLog::new(0);
    }
    log
}

/// Hand the batch to the sender, or drop it if the endpoint does not keep up.
fn flush(batches: Option<&mpsc::Sender<ReportLiveRequest>>, batch: &mut ReportLiveRequest) {
    if batch.updates().is_empty() {
        return;
    }
    let full = std::mem::replace(batch, ReportLiveRequest::new());
    if let Some(batches) = batches
        && batches.try_send(full).is_err()
    {
        debug!("Live view: endpoint not keeping up, batch dropped");
    }
}

/// One `ReportLive` call for the whole pass; any failure ends the live view of
/// this pass (the outcome and its log still go through the spool).
async fn send(config: RegencyConfig, host_id: String, batches: mpsc::Receiver<ReportLiveRequest>) {
    let result = async {
        let identity = tls::client_identity(&config, &host_id).await?;
        let channel = tls::channel_with_timeout(&config, Some(identity), None).await?;
        ReportingClient::new(channel)
            .report_live(tokio_stream::wrappers::ReceiverStream::new(batches))
            .await
            .map(|response| response.into_inner().accepted())
            .map_err(|status| format!("{} ({:?})", status.message(), status.code()))
    }
    .await;
    match result {
        Ok(accepted) => debug!(accepted, "Live view sent"),
        Err(details) => debug!(details, "Live view not sent"),
    }
}
