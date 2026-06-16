# waldo

A background service that monitors screen lock/unlock events via the systemd-logind `LockedHint` session property and sends webhook notifications. Designed to post presence status updates (online/offline) to Slack or similar services.

## How it works

Waldo watches the `LockedHint` property on your logind session over D-Bus, reacting whenever the property changes. To avoid noisy notifications from brief screen locks, it applies two filters:

- **Minimum lock duration** -- a "locked" (offline) webhook is only sent if the screen stays locked for a configurable threshold (default 5 minutes). Unlocking before the threshold produces no notification.
- **Cooldown** -- after sending any webhook, further webhooks are suppressed for a configurable period (default 10 minutes). Exception: an "online" webhook always fires if a corresponding "offline" was sent, so events stay paired.

## Requirements

- Linux with systemd-logind (GNOME, KDE, or any desktop using logind for session management)
- Rust 1.85+

## Installation

```
cargo install --path .
```

## Configuration

Create `~/.config/waldo/config.toml`:

```toml
# Webhook URL to POST JSON events to
webhook_url = "https://hooks.slack.com/services/T00/B00/xxxx"

# Only send "offline" if the screen stays locked for at least this
# many seconds (default: 300)
min_lock_duration_secs = 300

# Suppress further webhooks for this many seconds after sending one
# (default: 600)
cooldown_secs = 600

# Your name, sent as the "USER" field in the webhook payload
display_name = "Tony"
```

All fields except `min_lock_duration_secs` and `cooldown_secs` are required. The two duration fields default to 300 and 600 seconds respectively if omitted.

Use `--config` to specify an alternate path:

```
waldo --config /path/to/config.toml
```

### Reloading configuration

Send SIGHUP to reload the config file without restarting:

```
kill -HUP $(pidof waldo)
```

The in-flight state (debounce timer, cooldown, lock tracking) is preserved across reloads. If the config file has errors, waldo logs the problem and continues with the previous configuration.

## Webhook payload

Waldo POSTs JSON to the configured URL on state changes:

```json
{"USER": "Tony", "STATUS_MSG": "offline"}
```

```json
{"USER": "Tony", "STATUS_MSG": "online"}
```

## Running manually

```
RUST_LOG=waldo=debug waldo
```

Set `RUST_LOG=waldo=trace` for even more detail. The default log level is `info`.

## Running as a systemd user service

Copy the service file and enable it:

```
cp waldo.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now waldo
```

The service starts after the graphical session, restarts on failure, and stops when the session ends. Logs are available via:

```
journalctl --user -u waldo -f
```
