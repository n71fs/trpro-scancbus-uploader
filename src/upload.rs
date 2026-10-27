use crate::Config;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::process::{Command, Stdio};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
use trunk_recorder_plugin::{Attempt, CallRecord, ConcludedCall, Host};

pub struct Uploader {
    config: Config,
    agent: ureq::Agent,
    host: Host,
    stopping: Arc<AtomicBool>,
    uploaded: Mutex<HashSet<String>>,
}

impl Uploader {
    pub fn new(config: Config, host: Host, stopping: Arc<AtomicBool>) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(config.http_timeout_seconds)))
            .http_status_as_error(false)
            // Never forward a token-bearing POST to another URL.
            .max_redirects(0)
            .build()
            .into();
        Self {
            config,
            agent,
            host,
            stopping,
            uploaded: Mutex::new(HashSet::new()),
        }
    }

    pub fn upload(&self, call: &ConcludedCall) -> Attempt {
        if self.stopping.load(Ordering::Relaxed) {
            return Attempt::Retry("recorder stopping; save upload for next start".into());
        }
        let Some(audio) = call.files.m4a.as_ref().filter(|p| p.is_file()) else {
            return Attempt::Fail("M4A audio is missing; cannot upload this call".into());
        };
        let Some(name) = audio.file_name().and_then(|s| s.to_str()) else {
            return Attempt::Fail("audio filename is not valid UTF-8".into());
        };
        let cdn_url = audio_url(&self.config, name);
        let already_uploaded = self.uploaded.lock().unwrap().contains(&call.path);
        if !already_uploaded {
            // Match the old script: a public CDN hit avoids an unnecessary Spaces PUT.
            let exists = self
                .agent
                .head(&cdn_url)
                .call()
                .is_ok_and(|r| r.status().as_u16() == 200);
            if exists {
                self.host
                    .info(format!("CDN already has {name}, skipping upload"));
            } else {
                self.host
                    .info(format!("Uploading {name} to DigitalOcean Spaces"));
                if let Err(outcome) = self.put_audio(audio, name) {
                    return outcome;
                }
            }
            let mut uploaded = self.uploaded.lock().unwrap();
            // Bound the stage cache even if many calls exhaust their retries.
            // A cache miss safely falls back to the CDN check / same object PUT.
            if uploaded.len() >= 10_000 {
                uploaded.clear();
            }
            uploaded.insert(call.path.clone());
        }
        if self.stopping.load(Ordering::Relaxed) {
            return Attempt::Retry("recorder stopping; save API import for next start".into());
        }
        let body = payload(&self.config, &call.call, name);
        let response = match self.agent.post(&self.config.api_url).send_json(&body) {
            Ok(r) => r,
            // Don't log error strings/bodies: either could contain credentials.
            Err(_) => {
                return Attempt::Retry("ScanCBus request failed (network/TLS/timeout)".into());
            }
        };
        let status = response.status().as_u16();
        if status != 200 {
            let outcome = http_outcome(status);
            if matches!(outcome, Attempt::Fail(_)) {
                self.uploaded.lock().unwrap().remove(&call.path);
            }
            return outcome;
        }
        let header = |key: &str| {
            response
                .headers()
                .get(key)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
        };
        let result = if header("X-Transmission-Replaced") == "1" {
            "REPLACED"
        } else if header("X-Transmission-Deduped") == "1" {
            "DEDUPED"
        } else {
            "NEW"
        };
        // Transmission IDs are normally numeric. Bound and sanitize server input.
        let id: String = header("X-Transmission-Id")
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .take(80)
            .collect();
        let id = if id.is_empty() { "unknown" } else { &id };
        self.host.info(format!(
            "API RESULT: {result} id={id} tg={} quality={}",
            call.call.talkgroup,
            quality(&call.call)
        ));
        self.uploaded.lock().unwrap().remove(&call.path);
        Attempt::Done { url: cdn_url }
    }

    fn put_audio(&self, audio: &std::path::Path, name: &str) -> Result<(), Attempt> {
        let mut command = Command::new(&self.config.s3cmd_bin);
        if !self.config.s3cmd_config.is_empty() {
            command.arg("--config").arg(&self.config.s3cmd_config);
        }
        let prefix = self.config.spaces_prefix.trim_matches('/');
        let key = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}/{name}")
        };
        // Arguments are passed directly, never through a shell.
        let mut child = command
            .args(["put", "--acl-public", "--"])
            .arg(audio)
            .arg(format!("s3://{}/{key}", self.config.spaces_bucket))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| {
                Attempt::Fail(
                    "Cannot start s3cmd; check its executable path and permissions.".into(),
                )
            })?;
        let deadline = Instant::now() + Duration::from_secs(self.config.upload_timeout_seconds);
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => return Ok(()),
                Ok(Some(_)) => {
                    return Err(Attempt::Retry(
                        "Spaces upload failed; check s3cmd credentials, bucket, and connectivity."
                            .into(),
                    ));
                }
                Ok(None) => {}
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(Attempt::Retry(
                        "Cannot wait for the Spaces upload process.".into(),
                    ));
                }
            }
            if self.stopping.load(Ordering::Relaxed) || Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Attempt::Retry(
                    "Spaces upload interrupted or timed out.".into(),
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

pub fn http_outcome(status: u16) -> Attempt {
    match status {
        408 | 425 | 429 | 500..=599 => Attempt::Retry(format!("ScanCBus HTTP {status}")),
        _ => Attempt::Fail(format!(
            "ScanCBus rejected import: HTTP {status}; check token, URL, and payload."
        )),
    }
}

pub fn audio_url(config: &Config, name: &str) -> String {
    let mut url = url::Url::parse(&config.cdn_base).expect("validated CDN URL");
    {
        let mut segments = url.path_segments_mut().expect("HTTP URL has path segments");
        segments.pop_if_empty();
        for part in config.spaces_prefix.split('/').filter(|p| !p.is_empty()) {
            segments.push(part);
        }
        segments.push(name);
    }
    url.to_string()
}

pub fn quality(call: &CallRecord) -> i64 {
    let duration: f64 = call.freq_list.iter().map(|f| f.len).sum();
    let duration = if duration > 0.0 {
        duration
    } else if call.call_length > 0.0 {
        call.call_length
    } else {
        1.0
    };
    let errors: f64 = call.freq_list.iter().map(|f| f.error_count as f64).sum();
    let spikes: f64 = call.freq_list.iter().map(|f| f.spike_count as f64).sum();
    let freq_error = (call.freq_error as f64).abs();
    (100.0
        - (errors / duration * 0.05).min(60.0)
        - (spikes / duration * 2.0).min(20.0)
        - (freq_error / 50.0).min(20.0))
    .max(0.0) as i64
}

pub fn normalize_sources(raw: &Value) -> Vec<Value> {
    raw.as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            item.as_i64()
                .or_else(|| item.get("src").and_then(Value::as_i64))
                .map(|src| json!({"src": src}))
        })
        .collect()
}

pub fn payload(config: &Config, call: &CallRecord, filename: &str) -> Value {
    let length = (call.call_length as i64).max(1);
    let sources = serde_json::to_value(&call.src_list).unwrap_or(Value::Null);
    // Use URL path construction for filenames and the directory so spaces are escaped.
    let mut base = audio_url(config, "");
    if !base.ends_with('/') {
        base.push('/');
    }
    json!({
        "auth_token": config.auth_token,
        "system": config.system_name,
        "source": config.source_description,
        "talkgroup": call.talkgroup,
        "start_time": call.start_time,
        "stop_time": call.start_time.saturating_add(length),
        "freq": call.freq,
        "audio_filename": filename,
        "audio_file_url_path": base,
        "audio_file_type": "m4a",
        "audio_file_play_length": length,
        "has_audio": true,
        "srcList": normalize_sources(&sources),
        "quality_score": quality(call),
        "emergency": call.emergency,
    })
}
