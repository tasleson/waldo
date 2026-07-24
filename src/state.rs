// SPDX-License-Identifier: MIT
use std::path::PathBuf;
use std::pin::pin;
use std::sync::Arc;
use std::time::Duration;

use chrono::{Datelike, Local, Timelike, Weekday};
use tokio::time::Instant;

use crate::config::Config;
use crate::dbus::Login1SessionProxy;
use crate::persist::{ReportedStatus, StateFile};
use crate::webhook::{EventType, WebhookClient};

fn is_weekend() -> bool {
    matches!(Local::now().weekday(), Weekday::Sat | Weekday::Sun)
}

fn is_after_work_hours() -> bool {
    let now = Local::now();
    now.hour() > 17 || (now.hour() == 17 && now.minute() >= 30)
}

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
    config: Arc<Config>,
    config_path: PathBuf,
    webhook: WebhookClient,
    state_file: StateFile,
}

impl Monitor {
    pub fn new(
        config: Arc<Config>,
        config_path: PathBuf,
        webhook: WebhookClient,
        state_file: StateFile,
    ) -> Self {
        Self {
            state: State::Unlocked,
            last_webhook_sent: None,
            config,
            config_path,
            webhook,
            state_file,
        }
    }

    /// Compare the persisted last-reported status against the actual session
    /// state and send a webhook if the remote side is out of date. This covers
    /// restarts and reboots, where no LockedHint change will ever fire.
    pub async fn startup(&mut self, locked: bool) {
        let actual = if locked {
            ReportedStatus::Offline
        } else {
            ReportedStatus::Online
        };
        if locked {
            self.state = State::Locked {
                locked_at: Instant::now(),
            };
        }

        let last = self.state_file.last_reported();
        if last == Some(actual) {
            tracing::info!("Startup: remote already knows we are {actual:?}");
            return;
        }

        tracing::info!("Startup: last reported {last:?} but we are {actual:?}, reconciling");
        let event = if locked {
            EventType::Locked
        } else {
            EventType::Unlocked
        };
        self.maybe_send_webhook(event, None, true).await;
    }

    pub async fn run(&mut self, session: &Login1SessionProxy<'_>) -> anyhow::Result<()> {
        use futures_util::StreamExt;
        use tokio::signal::unix::{SignalKind, signal};

        let mut hint_stream = session.receive_locked_hint_changed().await;
        let debounce_timer = pin!(tokio::time::sleep(FAR_FUTURE));
        let mut debounce_timer = debounce_timer;
        let mut sighup = signal(SignalKind::hangup())?;
        let mut sigterm = signal(SignalKind::terminate())?;
        let mut sigint = signal(SignalKind::interrupt())?;

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
                                self.handle_lock(&mut debounce_timer).await;
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
                _ = sigterm.recv() => {
                    self.handle_shutdown("SIGTERM").await;
                    return Ok(());
                }
                _ = sigint.recv() => {
                    self.handle_shutdown("SIGINT").await;
                    return Ok(());
                }
            }
        }
    }

    /// On logout, reboot, or manual stop, tell the remote side we are offline
    /// if it currently believes we are online.
    async fn handle_shutdown(&mut self, signal_name: &str) {
        tracing::info!("Received {signal_name}, shutting down");
        if self.state_file.last_reported() == Some(ReportedStatus::Online) {
            tracing::info!("Reporting offline before exit");
            self.maybe_send_webhook(EventType::Locked, None, true).await;
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

    async fn handle_lock(&mut self, timer: &mut std::pin::Pin<&mut tokio::time::Sleep>) {
        match self.state {
            State::Unlocked => {
                let now = Instant::now();
                if !is_weekend() && is_after_work_hours() {
                    tracing::info!(
                        "Lock signal received after hours, sending notification immediately"
                    );
                    self.maybe_send_webhook(EventType::Locked, None, true).await;
                    self.state = State::Locked { locked_at: now };
                } else {
                    tracing::info!("Lock signal received, starting debounce timer");
                    self.state = State::PendingLock { locked_at: now };
                    let deadline = now + Duration::from_secs(self.config.min_lock_duration_secs);
                    timer.as_mut().reset(deadline);
                }
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
                self.maybe_send_webhook(EventType::Unlocked, Some(duration), false)
                    .await;
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
            self.maybe_send_webhook(EventType::Locked, None, false)
                .await;
            self.state = State::Locked { locked_at };
        }
    }

    async fn maybe_send_webhook(
        &mut self,
        event: EventType,
        lock_duration: Option<Duration>,
        force: bool,
    ) {
        if is_weekend() {
            tracing::info!("Weekend — suppressing {event:?} webhook");
            return;
        }

        // An "online" webhook always fires if the remote was last told
        // "offline", so events stay paired.
        let force = force
            || (matches!(event, EventType::Unlocked)
                && self.state_file.last_reported() == Some(ReportedStatus::Offline));

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
                    return;
                }
            }
        }

        match self.webhook.send(event, lock_duration).await {
            Ok(()) => {
                self.last_webhook_sent = Some(Instant::now());
                self.state_file.record(match event {
                    EventType::Locked => ReportedStatus::Offline,
                    EventType::Unlocked => ReportedStatus::Online,
                });
                tracing::info!("Webhook sent: {event:?}");
            }
            Err(e) => {
                tracing::error!("Webhook failed: {e:#}");
            }
        }
    }
}
