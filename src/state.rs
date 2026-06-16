use std::path::PathBuf;
use std::pin::pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::time::Instant;

use crate::config::Config;
use crate::dbus::Login1SessionProxy;
use crate::webhook::{EventType, WebhookClient};

const FAR_FUTURE: Duration = Duration::from_secs(86400 * 365);

#[derive(Debug)]
enum State {
    Unlocked,
    PendingLock { locked_at: Instant },
    Locked { locked_at: Instant },
}

pub struct Monitor {
    state: State,
    last_webhook_sent: Option<Instant>,
    lock_webhook_sent: bool,
    config: Arc<Config>,
    config_path: PathBuf,
    webhook: WebhookClient,
}

impl Monitor {
    pub fn new(config: Arc<Config>, config_path: PathBuf, webhook: WebhookClient) -> Self {
        Self {
            state: State::Unlocked,
            last_webhook_sent: None,
            lock_webhook_sent: false,
            config,
            config_path,
            webhook,
        }
    }

    pub async fn run(&mut self, session: &Login1SessionProxy<'_>) -> anyhow::Result<()> {
        use futures_util::StreamExt;
        use tokio::signal::unix::{SignalKind, signal};

        let mut hint_stream = session.receive_locked_hint_changed().await;
        let debounce_timer = pin!(tokio::time::sleep(FAR_FUTURE));
        let mut debounce_timer = debounce_timer;
        let mut sighup = signal(SignalKind::hangup())?;

        tracing::info!(
            "Monitoring started (min_lock={}s, cooldown={}s)",
            self.config.min_lock_duration_secs,
            self.config.cooldown_secs,
        );

        loop {
            tokio::select! {
                Some(change) = hint_stream.next() => {
                    match change.get().await {
                        Ok(locked) => {
                            tracing::debug!("LockedHint changed to {locked}");
                            if locked {
                                self.handle_lock(&mut debounce_timer);
                            } else {
                                self.handle_unlock(&mut debounce_timer).await;
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Failed to read LockedHint: {e}");
                        }
                    }
                }
                () = &mut debounce_timer => {
                    self.handle_timer_fired().await;
                    debounce_timer.as_mut().reset(Instant::now() + FAR_FUTURE);
                }
                _ = sighup.recv() => {
                    self.reload_config();
                }
            }
        }
    }

    fn reload_config(&mut self) {
        match Config::load(&self.config_path) {
            Ok(new_config) => {
                tracing::info!(
                    "Config reloaded (min_lock={}s, cooldown={}s, user={:?})",
                    new_config.min_lock_duration_secs,
                    new_config.cooldown_secs,
                    new_config.display_name,
                );
                self.webhook.reload(&new_config);
                self.config = Arc::new(new_config);
            }
            Err(e) => {
                tracing::error!("Failed to reload config: {e:#}");
            }
        }
    }

    fn handle_lock(&mut self, timer: &mut std::pin::Pin<&mut tokio::time::Sleep>) {
        match self.state {
            State::Unlocked => {
                let now = Instant::now();
                tracing::info!("Lock signal received, starting debounce timer");
                self.state = State::PendingLock { locked_at: now };
                let deadline = now + Duration::from_secs(self.config.min_lock_duration_secs);
                timer.as_mut().reset(deadline);
            }
            State::PendingLock { .. } | State::Locked { .. } => {
                tracing::debug!("Duplicate lock signal, ignoring");
            }
        }
    }

    async fn handle_unlock(&mut self, timer: &mut std::pin::Pin<&mut tokio::time::Sleep>) {
        match self.state {
            State::PendingLock { locked_at } => {
                let duration = locked_at.elapsed();
                tracing::info!("Unlocked after {duration:.0?}, below threshold — no notification");
                timer.as_mut().reset(Instant::now() + FAR_FUTURE);
                self.state = State::Unlocked;
            }
            State::Locked { locked_at } => {
                let duration = locked_at.elapsed();
                tracing::info!("Unlocked after {duration:.0?}");
                self.maybe_send_webhook(EventType::Unlocked, Some(duration))
                    .await;
                self.lock_webhook_sent = false;
                self.state = State::Unlocked;
            }
            State::Unlocked => {
                tracing::debug!("Spurious unlock signal, ignoring");
            }
        }
    }

    async fn handle_timer_fired(&mut self) {
        if let State::PendingLock { locked_at } = self.state {
            tracing::info!("Lock persisted past threshold, sending notification");
            if self.maybe_send_webhook(EventType::Locked, None).await {
                self.lock_webhook_sent = true;
            }
            self.state = State::Locked { locked_at };
        }
    }

    // Returns true if the webhook was actually sent.
    async fn maybe_send_webhook(
        &mut self,
        event: EventType,
        lock_duration: Option<Duration>,
    ) -> bool {
        let force = matches!(event, EventType::Unlocked) && self.lock_webhook_sent;

        if !force {
            if let Some(last) = self.last_webhook_sent {
                let elapsed = last.elapsed();
                let cooldown = Duration::from_secs(self.config.cooldown_secs);
                if elapsed < cooldown {
                    tracing::info!(
                        "Cooldown active ({:.0?} remaining), suppressing {:?} webhook",
                        cooldown - elapsed,
                        event,
                    );
                    return false;
                }
            }
        }

        match self.webhook.send(event, lock_duration).await {
            Ok(()) => {
                self.last_webhook_sent = Some(Instant::now());
                tracing::info!("Webhook sent: {event:?}");
                true
            }
            Err(e) => {
                tracing::error!("Webhook failed: {e:#}");
                false
            }
        }
    }
}
