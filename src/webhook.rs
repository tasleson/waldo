// SPDX-License-Identifier: MIT
use std::time::Duration;

use serde::Serialize;

use crate::config::Config;

#[derive(Debug, Clone, Copy)]
pub enum EventType {
    Locked,
    Unlocked,
}

#[derive(Debug, Serialize)]
pub struct WebhookPayload {
    #[serde(rename = "USER")]
    pub user: String,
    #[serde(rename = "STATUS_MSG")]
    pub status_msg: String,
}

pub struct WebhookClient {
    client: reqwest::Client,
    url: String,
    display_name: String,
    online_text: String,
    offline_text: String,
}

impl WebhookClient {
    pub fn new(config: &Config) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("failed to build HTTP client");

        Self {
            client,
            url: config.webhook_url.clone(),
            display_name: config.display_name.clone(),
            online_text: config.online_text.clone(),
            offline_text: config.offline_text.clone(),
        }
    }

    pub fn reload(&mut self, config: &Config) {
        self.url = config.webhook_url.clone();
        self.display_name = config.display_name.clone();
        self.online_text = config.online_text.clone();
        self.offline_text = config.offline_text.clone();
    }

    pub async fn send(
        &self,
        event: EventType,
        _lock_duration: Option<Duration>,
    ) -> anyhow::Result<()> {
        let status_msg = match event {
            EventType::Locked => &self.offline_text,
            EventType::Unlocked => &self.online_text,
        };
        let payload = WebhookPayload {
            user: self.display_name.clone(),
            status_msg: status_msg.clone(),
        };

        let body = serde_json::to_vec(&payload)?;
        tracing::debug!("Sending webhook: {}", String::from_utf8_lossy(&body));

        let mut url = self.url.clone();
        for _ in 0..5 {
            let response = self
                .client
                .post(&url)
                .header("Content-Type", "application/json")
                .body(body.clone())
                .send()
                .await?;

            if response.status().is_redirection() {
                if let Some(location) = response.headers().get("location") {
                    let new_url = location.to_str()?;
                    tracing::info!("Following redirect to {new_url}");
                    url = new_url.to_string();
                    continue;
                }
                anyhow::bail!("redirect without Location header");
            }

            if !response.status().is_success() {
                let status = response.status();
                let resp_body = response.text().await.unwrap_or_default();
                anyhow::bail!("webhook returned {status}: {resp_body}");
            }

            return Ok(());
        }

        anyhow::bail!("too many redirects")
    }
}
