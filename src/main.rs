mod upload;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use trunk_recorder_plugin::{
    CallQueue, ConcludedCall, Host, Manifest, NoConfig, Plugin, QueueOptions, Setup, format, topic,
};

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "camelCase")]
struct Config {
    /// ScanCBus import URL
    #[schemars(url)]
    api_url: String,
    /// ScanCBus authentication token
    #[schemars(extend("x-secret" = true, "x-required" = true))]
    auth_token: String,
    /// System name sent to ScanCBus
    system_name: String,
    /// Source description
    source_description: String,
    /// Spaces bucket
    spaces_bucket: String,
    /// Spaces object prefix
    spaces_prefix: String,
    /// Public CDN base URL
    #[schemars(url)]
    cdn_base: String,
    /// s3cmd executable
    s3cmd_bin: String,
    /// s3cmd configuration file (optional)
    /// Leave empty to use s3cmd's default ~/.s3cfg. Store Spaces credentials there, not here.
    s3cmd_config: String,
    /// HTTP timeout in seconds
    #[schemars(range(min = 1, max = 300))]
    http_timeout_seconds: u64,
    /// Spaces upload timeout in seconds
    #[schemars(range(min = 1, max = 3600))]
    upload_timeout_seconds: u64,
    /// Retry delays in seconds
    /// Each entry adds one retry after the initial attempt. Empty disables retries.
    retry_delays_seconds: Vec<u64>,
    /// Concurrent uploads
    #[schemars(range(min = 1, max = 16))]
    workers: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            api_url: "https://scancbus.com/api_v2/import_transmission/".into(),
            auth_token: String::new(),
            system_name: "MARCS/COIRS".into(),
            source_description: "lptp".into(),
            spaces_bucket: "scancbus".into(),
            spaces_prefix: "audio_files".into(),
            cdn_base: "https://scancbus.nyc3.cdn.digitaloceanspaces.com/".into(),
            s3cmd_bin: "s3cmd".into(),
            s3cmd_config: String::new(),
            http_timeout_seconds: 5,
            upload_timeout_seconds: 60,
            retry_delays_seconds: vec![10, 60, 300, 900],
            workers: 2,
        }
    }
}

impl Config {
    fn validate(&self) -> Result<(), String> {
        if self.auth_token.trim().is_empty() {
            return Err("Set the ScanCBus authentication token.".into());
        }
        for value in [&self.api_url, &self.cdn_base] {
            let url = url::Url::parse(value)
                .map_err(|_| "API and CDN URLs must be absolute HTTP(S) URLs.")?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(
                    "Use HTTP(S) API/CDN URLs without credentials, query strings, or fragments."
                        .into(),
                );
            }
        }
        if self.spaces_bucket.trim().is_empty()
            || self.system_name.trim().is_empty()
            || self.s3cmd_bin.trim().is_empty()
        {
            return Err("Set the Spaces bucket, system name, and s3cmd executable.".into());
        }
        if !(1..=16).contains(&self.workers)
            || !(1..=300).contains(&self.http_timeout_seconds)
            || !(1..=3600).contains(&self.upload_timeout_seconds)
            || self
                .retry_delays_seconds
                .iter()
                .any(|s| *s == 0 || *s > 86400)
        {
            return Err("Workers must be 1–16; HTTP timeout 1–300s; upload timeout 1–3600s; retry delays 1–86400s.".into());
        }
        Ok(())
    }
}

struct ScanCBus {
    queue: CallQueue,
    stopping: Arc<AtomicBool>,
}

impl Plugin for ScanCBus {
    type Config = Config;
    type SystemConfig = NoConfig;

    fn manifest() -> Manifest {
        Manifest {
            name: "ScanCBus Uploader".into(),
            subscribe: vec![topic::CALL_CONCLUDED.into()],
            audio_formats: vec![format::M4A.into()],
            ..trunk_recorder_plugin::manifest!()
        }
    }

    fn start(host: Host, setup: Setup<Config, NoConfig>) -> Result<Self, String> {
        setup.config.validate()?;
        if !setup.has_format(format::M4A) {
            return Err("ScanCBus needs M4A audio. Install ffmpeg and restart recording.".into());
        }
        std::fs::create_dir_all(&setup.data_dir)
            .map_err(|_| "Cannot create the plugin data directory.")?;
        let stopping = Arc::new(AtomicBool::new(false));
        let opts = QueueOptions {
            retry_after: setup
                .config
                .retry_delays_seconds
                .iter()
                .copied()
                .map(Duration::from_secs)
                .collect(),
            threads: setup.config.workers,
            noun: "upload",
            endpoint: Some("ScanCBus / DigitalOcean Spaces".into()),
            ..QueueOptions::saved_in(&setup.data_dir)
        };
        let uploader = upload::Uploader::new(setup.config, host.clone(), stopping.clone());
        let queue = CallQueue::start(host, opts, move |call| uploader.upload(call));
        Ok(Self { queue, stopping })
    }

    fn call_concluded(&mut self, call: ConcludedCall) {
        self.queue.push(call);
    }

    fn shutdown(&mut self, grace: Duration) {
        // Give quick uploads a chance to finish, then interrupt a slow s3cmd
        // so the SDK can save that call before the host kills this process.
        let stopping = self.stopping.clone();
        let (finished, done) = std::sync::mpsc::channel::<()>();
        let cancel = std::thread::spawn(move || {
            if done.recv_timeout(grace.mul_f64(0.2)).is_err() {
                stopping.store(true, Ordering::Relaxed);
            }
        });
        self.queue.shutdown(grace);
        let _ = finished.send(());
        let _ = cancel.join();
    }
}

fn main() {
    trunk_recorder_plugin::run::<ScanCBus>();
}

#[cfg(test)]
mod tests;
