use casper_types::TimeDiff;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    sync::{
        mpsc::{channel, Receiver, Sender},
        Mutex,
    },
    time::sleep,
};
use tokio_util::sync::CancellationToken;

/// An object that allows killing connections if a period of inactivity has been detected.
/// Whenever the binary port processes a message request for a given connection, the
/// KeepAliveMonitor::tick() method should be called. The KeepAliveMonitor has the responsibility
/// keep track when the last tick happened. If more time than configured happened since last "tick",
/// we send a posion pill via cancelling a CancellationToken.
pub(super) struct KeepAliveMonitor {
    /// Address of the endpoint which the KeepAliveMonitor needs to observe
    cancellation_token: CancellationToken,
    /// Time the check job sleeps between checks
    keepalive_check_interval: TimeDiff,
    /// Time we wait for a tick message to be enqueued.
    tick_message_enqueue_timeout: Duration,
    /// The amount of inactivity on a binary port connection which will force a close from the
    /// keepalive mechanism
    keepalive_no_activity_timeout: TimeDiff,
    /// Internal state of KeepAliveMonitor, it is the instant of time when the last bytes were
    /// observed from `bind_address`
    last_message_seen_at: Arc<Mutex<Option<Instant>>>,
    /// This receiver should get messages every time there is something happening on the datasource
    /// that we are monitoring for inactivity.
    receiver: Arc<Mutex<Receiver<()>>>,
    /// Internal queue which collects "ticks". A "tick" happens every time we observe any message
    /// from `receiver` field.
    sender: Sender<()>,
}

impl KeepAliveMonitor {
    /// The `tick` should be called every time the binary port processes a message request for a
    /// given connection. Internally, not every `tick` might actually update the
    /// `last_message_seen_at`. We are using a channel to send tick messages. If the channel is
    /// full, the tick message is dropped - but that would happen only in a situation where a lock
    /// of `tick` are being called - we don't really care about being millisecond-precise in this
    /// mechanism, we want to be able to detect staleness of binary port connections.
    pub(super) async fn tick(&self) {
        let _ = self
            .sender
            .send_timeout((), self.tick_message_enqueue_timeout)
            .await;
    }

    /// Assembles a new `KeepAliveMonitor`. It still doesn't collect data, you need to call the
    /// `start` function for that.
    pub(super) fn new(
        keepalive_check_interval: TimeDiff,
        keepalive_no_activity_timeout: TimeDiff,
        tick_message_enqueue_timeout: TimeDiff,
        tick_message_queue_size: usize,
    ) -> Self {
        let cancellation_token = CancellationToken::new();
        let last_message_seen_at = Arc::new(Mutex::new(None));
        let (tx, rx) = channel(tick_message_queue_size);
        KeepAliveMonitor {
            cancellation_token,
            keepalive_check_interval,
            tick_message_enqueue_timeout: Duration::from(tick_message_enqueue_timeout),
            keepalive_no_activity_timeout,
            last_message_seen_at,
            receiver: Arc::new(Mutex::new(rx)),
            sender: tx,
        }
    }

    pub(super) fn get_cancellation_token(&self) -> CancellationToken {
        self.cancellation_token.clone()
    }

    /// Spawns tasks responsible for asynchronous checks of the monitored datasource.
    /// If a configured time of inactivity is detected, `cancellation_token` is cancelled.
    pub(super) async fn start(&self) {
        self.spawn_last_seen_checker_task(
            self.last_message_seen_at.clone(),
            self.keepalive_check_interval,
            self.keepalive_no_activity_timeout,
            self.cancellation_token.clone(),
        );
        self.spawn_data_observing_task();
    }

    fn spawn_data_observing_task(&self) {
        let receiver = self.receiver.clone();
        let last_message_seen_at = self.last_message_seen_at.clone();
        tokio::spawn(async move {
            let mut guard = receiver.lock().await;
            while (guard.recv().await).is_some() {
                let now = Some(Instant::now());
                let mut guard = last_message_seen_at.lock().await;
                *guard = now;
                drop(guard);
            }
        });
    }

    fn spawn_last_seen_checker_task(
        &self,
        last_activity_holder: Arc<Mutex<Option<Instant>>>,
        keepalive_check_interval: TimeDiff,
        no_message_timeout: TimeDiff,
        cancellation_token: CancellationToken,
    ) {
        let no_message_timeout_duration = Duration::from(no_message_timeout);
        let keepalive_check_interval_duration = Duration::from(keepalive_check_interval);
        tokio::spawn(async move {
            loop {
                sleep(keepalive_check_interval_duration).await;
                let mut guard = last_activity_holder.lock().await;
                match &mut *guard {
                    Some(last_seen_at) => {
                        if last_seen_at.elapsed() > no_message_timeout_duration {
                            cancellation_token.cancel();
                            break;
                        }
                    }
                    None => {
                        // This scenario shouldn't happen often. It means that there was no data
                        // observed before the first check happened.
                        // We are setting last_seen_at value to now so that the keep alive can fail
                        // in the off chance that the process
                        // which is producing data hung before first message was produced
                        *guard = Some(Instant::now());
                    }
                }
                drop(guard);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::KeepAliveMonitor;
    use casper_types::TimeDiff;
    use std::{sync::Arc, time::Duration};
    use tokio::{select, time::sleep};

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn should_cancel_when_first_check_finds_no_activity() {
        let monitor = KeepAliveMonitor::new(
            TimeDiff::from_seconds(1),
            TimeDiff::from_seconds(2),
            TimeDiff::from_millis(20),
            5,
        );
        monitor.start().await;
        let cancellation_token = monitor.get_cancellation_token();
        select! {
            _ = cancellation_token.cancelled() => {},
            _ = sleep(Duration::from_secs(10)) => {
                unreachable!()
            },
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn should_not_cancel_if_endpoint_produces_data() {
        let monitor = Arc::new(KeepAliveMonitor::new(
            TimeDiff::from_seconds(10),
            TimeDiff::from_seconds(30),
            TimeDiff::from_millis(20),
            5,
        ));
        mock_server(monitor.clone(), 1);
        monitor.start().await;
        let cancellation_token = monitor.get_cancellation_token();
        select! {
            _ = cancellation_token.cancelled() => {
                unreachable!()
            },
            _ = sleep(Duration::from_secs(15)) => {
            },
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn should_cancel_if_no_activity_for_prolonged_period_of_time() {
        let monitor = Arc::new(KeepAliveMonitor::new(
            TimeDiff::from_seconds(1),
            TimeDiff::from_seconds(3),
            TimeDiff::from_millis(20),
            5,
        ));
        mock_server(monitor.clone(), 1500); //1500 seconds of interval to make sure that the monitor won't see activity
        monitor.start().await;
        let cancellation_token = monitor.get_cancellation_token();
        select! {
            _ = cancellation_token.cancelled() => {
            },
            _ = sleep(Duration::from_secs(15)) => {
                unreachable!()
            },
        }
    }

    fn mock_server(monitor: Arc<KeepAliveMonitor>, interval_in_seconds: u64) {
        tokio::spawn(async move {
            monitor.tick().await;
            sleep(Duration::from_secs(interval_in_seconds)).await;
        });
    }
}
