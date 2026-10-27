use super::*;
use crate::upload::{Uploader, audio_url, http_outcome, normalize_sources, payload, quality};
use serde_json::{Value, json};
use trunk_recorder_plugin::testing::{self, MockServer};
use trunk_recorder_plugin::{Attempt, EXIT_CONFIG, HostMessage, Outcome};

fn config(server: &MockServer) -> Config {
    Config {
        api_url: format!("{}/import", server.url()),
        cdn_base: format!("{}/", server.url()),
        auth_token: "test-secret-never-log".into(),
        ..Config::default()
    }
}

#[cfg(unix)]
fn fake_s3(dir: &std::path::Path, body: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("fake-s3cmd");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path.to_string_lossy().into_owned()
}

#[test]
fn payload_preserves_script_fields_and_quality() {
    let dir = testing::temp_dir("payload");
    let mut c = testing::call(&dir, "sys1", 101).call;
    c.call_length = 3.9;
    c.freq_list[0].len = 4.0;
    c.freq_list[0].error_count = 400;
    c.freq_list[0].spike_count = 8;
    c.freq_error = -500;
    c.emergency = true;
    let config = Config {
        auth_token: "key".into(),
        ..Config::default()
    };
    assert_eq!(quality(&c), 81); // 100 - 5 - 4 - 10
    assert_eq!(
        payload(&config, &c, "call.m4a"),
        json!({
            "auth_token":"key", "system":"MARCS/COIRS", "source":"lptp", "talkgroup":101,
            "start_time":c.start_time, "stop_time":c.start_time+3, "freq":851012500u64,
            "audio_filename":"call.m4a", "audio_file_url_path":"https://scancbus.nyc3.cdn.digitaloceanspaces.com/audio_files/",
            "audio_file_type":"m4a", "audio_file_play_length":3, "has_audio":true,
            "srcList":[{"src":1234}], "quality_score":81, "emergency":true
        })
    );
    c.call_length = 0.0;
    c.freq_list.clear();
    assert_eq!(payload(&config, &c, "x")["audio_file_play_length"], 1);
    assert_eq!(quality(&c), 90);
    c.freq_list.push(trunk_recorder_plugin::FreqEntry {
        len: 1.0,
        error_count: 999999,
        spike_count: 999999,
        ..Default::default()
    });
    c.freq_error = 999999;
    assert_eq!(quality(&c), 0);
}

#[test]
fn sources_and_encoded_urls() {
    assert_eq!(
        normalize_sources(&json!([123, {"src":456, "tag":"ignored"}, "bad", {}, 1.5, true])),
        json!([{"src":123},{"src":456}]).as_array().unwrap().clone()
    );
    assert!(normalize_sources(&Value::Null).is_empty());
    let config = Config::default();
    assert!(audio_url(&config, "a #?.m4a").ends_with("audio_files/a%20%23%3F.m4a"));
}

#[test]
fn classifies_api_failures() {
    for code in [408, 425, 429, 500, 503] {
        assert!(matches!(http_outcome(code), Attempt::Retry(_)));
    }
    for code in [201, 301, 400, 401, 403, 404, 422] {
        assert!(matches!(http_outcome(code), Attempt::Fail(_)));
    }
}

#[test]
fn handshake_requires_token_and_m4a() {
    let dir = testing::temp_dir("handshake");
    let h = testing::hello(&dir, json!({}));
    let out = testing::run::<ScanCBus>([HostMessage::Hello(h)]);
    assert_eq!(out.exit_code, EXIT_CONFIG);
    assert!(out.status().unwrap().1.contains("token"));
    let mut h = testing::hello(&dir, json!({"authToken":"secret"}));
    h.audio_formats = vec!["wav".into()];
    let out = testing::run::<ScanCBus>([HostMessage::Hello(h)]);
    assert_eq!(out.exit_code, EXIT_CONFIG);
    assert!(out.status().unwrap().1.contains("ffmpeg"));
    let h = testing::hello(&dir, json!({"authToken":"secret"}));
    assert!(testing::run::<ScanCBus>([HostMessage::Hello(h)]).ready());
}

#[test]
fn cdn_hit_skips_s3_and_posts_metadata() {
    let dir = testing::temp_dir("cdn-hit");
    let server = MockServer::start(|_| (200, String::new()));
    let cfg = Config {
        s3cmd_bin: "/does/not/exist".into(),
        ..config(&server)
    };
    let call = testing::call(&dir, "sys1", 5);
    let out = testing::run::<ScanCBus>([
        HostMessage::Hello(testing::hello(&dir, serde_json::to_value(cfg).unwrap())),
        HostMessage::CallConcluded(call.clone()),
    ]);
    assert_eq!(out.results()[0].1, Outcome::Ok);
    assert_eq!(out.results()[0].0, call.path);
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method, "HEAD");
    assert_eq!(requests[1].method, "POST");
    let body: Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert_eq!(body["talkgroup"], 5);
    assert_eq!(body["srcList"], json!([{"src":1234}]));
    assert!(
        out.logs()
            .iter()
            .any(|(_, msg)| msg.contains("API RESULT: NEW"))
    );
    assert!(!format!("{:?}", out.messages).contains("test-secret-never-log"));
}

#[cfg(unix)]
#[test]
fn spaces_arguments_and_api_retry_do_not_repeat_put() {
    let dir = testing::temp_dir("s3-retry");
    let args = dir.join("args");
    let fake = fake_s3(
        &dir,
        &format!("printf '%s\\n' \"$@\" >> '{}'", args.display()),
    );
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = attempts.clone();
    let server = MockServer::start(move |req| {
        if req.method == "HEAD" {
            return (404, String::new());
        }
        if count.fetch_add(1, Ordering::Relaxed) == 0 {
            (503, "temporary".into())
        } else {
            (200, String::new())
        }
    });
    let cfg = Config {
        s3cmd_bin: fake,
        s3cmd_config: "/tmp/with spaces.s3cfg".into(),
        ..config(&server)
    };
    let (host, captured) = testing::capture();
    let uploader = Uploader::new(cfg, host.clone(), Arc::new(AtomicBool::new(false)));
    let mut queue = CallQueue::start(
        host,
        QueueOptions {
            retry_after: vec![Duration::from_millis(10)],
            threads: 1,
            ..QueueOptions::saved_in(&dir)
        },
        move |c| uploader.upload(c),
    );
    let call = testing::call(&dir, "sys1", 9);
    queue.push(call.clone());
    wait_until(|| !captured.output().results().is_empty());
    queue.shutdown(Duration::from_secs(1));
    assert_eq!(captured.output().results()[0].1, Outcome::Ok);
    assert_eq!(attempts.load(Ordering::Relaxed), 2);
    let lines = std::fs::read_to_string(args).unwrap();
    let expected = format!(
        "--config\n/tmp/with spaces.s3cfg\nput\n--acl-public\n--\n{}\ns3://scancbus/audio_files/{}\n",
        call.files.m4a.as_ref().unwrap().display(),
        call.files
            .m4a
            .as_ref()
            .unwrap()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
    );
    assert_eq!(lines, expected);
    assert!(
        captured
            .output()
            .metrics()
            .iter()
            .any(|m| m.retrying == Some(1))
    );
}

#[test]
fn pending_upload_survives_shutdown_and_resumes() {
    let dir = testing::temp_dir("resume");
    let down = MockServer::start(|r| {
        if r.method == "HEAD" {
            (200, String::new())
        } else {
            (503, String::new())
        }
    });
    let call = testing::call(&dir, "sys1", 15);
    let out = testing::run::<ScanCBus>([
        HostMessage::Hello(testing::hello(
            &dir,
            serde_json::to_value(config(&down)).unwrap(),
        )),
        HostMessage::CallConcluded(call.clone()),
    ]);
    assert!(out.results().is_empty());
    let saved = dir.join("data/queue.jsonl");
    let stored = std::fs::read_to_string(&saved).unwrap();
    assert!(stored.contains(&call.path));
    assert!(!stored.contains("test-secret-never-log"));
    let up = MockServer::start(|_| (200, String::new()));
    let out = testing::run::<ScanCBus>([HostMessage::Hello(testing::hello(
        &dir,
        serde_json::to_value(config(&up)).unwrap(),
    ))]);
    assert_eq!(out.results()[0].0, call.path);
    assert_eq!(out.results()[0].1, Outcome::Ok);
    assert!(!saved.exists());
}

#[test]
fn permanent_rejection_and_missing_audio_fail() {
    let dir = testing::temp_dir("fail");
    let server = MockServer::start(|r| {
        if r.method == "HEAD" {
            (200, String::new())
        } else {
            (401, "test-secret-never-log".into())
        }
    });
    let (host, captured) = testing::capture();
    let uploader = Uploader::new(config(&server), host, Arc::new(AtomicBool::new(false)));
    let mut call = testing::call(&dir, "sys1", 1);
    assert!(matches!(uploader.upload(&call), Attempt::Fail(_)));
    assert_eq!(
        server
            .requests()
            .iter()
            .filter(|r| r.method == "POST")
            .count(),
        1
    );
    call.files.m4a = None;
    assert!(matches!(uploader.upload(&call), Attempt::Fail(_)));
    assert!(!format!("{:?}", captured.output()).contains("test-secret-never-log"));
}

#[cfg(unix)]
#[test]
fn failed_spaces_upload_retries_before_api_import() {
    let dir = testing::temp_dir("s3-fail");
    let server = MockServer::start(|_| (404, String::new()));
    let cfg = Config {
        s3cmd_bin: fake_s3(&dir, "exit 1"),
        ..config(&server)
    };
    let (host, _) = testing::capture();
    let uploader = Uploader::new(cfg, host, Arc::new(AtomicBool::new(false)));
    assert!(matches!(
        uploader.upload(&testing::call(&dir, "sys1", 1)),
        Attempt::Retry(_)
    ));
    assert!(server.requests().iter().all(|r| r.method == "HEAD"));
}

#[cfg(unix)]
#[test]
fn shutdown_interrupts_slow_spaces_upload_and_saves_call() {
    let dir = testing::temp_dir("slow-s3");
    let server = MockServer::start(|_| (404, String::new()));
    // exec avoids leaving a shell child when the executable is interrupted.
    let cfg = Config {
        s3cmd_bin: fake_s3(&dir, "exec sleep 30"),
        ..config(&server)
    };
    let call = testing::call(&dir, "sys1", 1);
    let (host, captured) = testing::capture();
    let mut plugin = ScanCBus::start(
        host,
        Setup {
            config: cfg.clone(),
            systems: vec![trunk_recorder_plugin::System {
                index: 0,
                short_name: "sys1".into(),
                kind: "p25".into(),
                name: String::new(),
                fcc: Default::default(),
                fcc_call_sign: String::new(),
                fcc_name: String::new(),
                sites: vec![],
                config: None,
                settings: cfg.clone(),
            }],
            host: Default::default(),
            capture_dir: dir.clone(),
            data_dir: dir.join("data"),
            audio_formats: vec!["m4a".into()],
        },
    )
    .unwrap();
    plugin.call_concluded(call.clone());
    wait_until(|| !server.requests().is_empty());
    let start = std::time::Instant::now();
    plugin.shutdown(Duration::from_secs(1));
    assert!(start.elapsed() < Duration::from_secs(2));
    assert!(captured.output().results().is_empty());
    assert!(
        std::fs::read_to_string(dir.join("data/queue.jsonl"))
            .unwrap()
            .contains(&call.path)
    );
}

fn wait_until(f: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !f() {
        assert!(std::time::Instant::now() < deadline, "condition timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn retry_exhaustion_reports_failure() {
    let dir = testing::temp_dir("exhaustion");
    let server = MockServer::start(|r| {
        if r.method == "HEAD" {
            (200, String::new())
        } else {
            (503, String::new())
        }
    });
    let (host, captured) = testing::capture();
    let uploader = Uploader::new(
        config(&server),
        host.clone(),
        Arc::new(AtomicBool::new(false)),
    );
    let mut queue = CallQueue::start(
        host,
        QueueOptions {
            retry_after: vec![Duration::from_millis(10), Duration::from_millis(10)],
            threads: 1,
            ..QueueOptions::saved_in(&dir)
        },
        move |c| uploader.upload(c),
    );
    queue.push(testing::call(&dir, "sys1", 1));
    wait_until(|| !captured.output().results().is_empty());
    queue.shutdown(Duration::from_secs(1));
    assert_eq!(captured.output().results()[0].1, Outcome::Failed);
    assert_eq!(
        server
            .requests()
            .iter()
            .filter(|r| r.method == "POST")
            .count(),
        3
    );
}

#[test]
fn transmission_response_headers_are_reported() {
    use std::io::{BufRead, BufReader, Read, Write};
    for (headers, expected) in [
        ("X-Transmission-Deduped: 1\r\n", "DEDUPED"),
        (
            "X-Transmission-Deduped: 1\r\nX-Transmission-Replaced: 1\r\n",
            "REPLACED",
        ),
    ] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let server_url = format!("http://{}/", listener.local_addr().unwrap());
        let thread = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                let mut length = 0;
                loop {
                    line.clear();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse::<usize>().unwrap();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\nX-Transmission-Id: 42\r\n{headers}\r\n").unwrap();
            }
        });
        let (host, captured) = testing::capture();
        let cfg = Config {
            api_url: server_url.clone(),
            cdn_base: server_url,
            auth_token: "secret".into(),
            ..Config::default()
        };
        let uploader = Uploader::new(cfg, host, Arc::new(AtomicBool::new(false)));
        let dir = testing::temp_dir("headers");
        assert!(matches!(
            uploader.upload(&testing::call(&dir, "sys1", 1)),
            Attempt::Done { .. }
        ));
        thread.join().unwrap();
        assert!(
            captured
                .output()
                .logs()
                .iter()
                .any(|(_, message)| message.contains(&format!("API RESULT: {expected} id=42")))
        );
    }
}

#[test]
fn invalid_configuration_is_rejected() {
    for cfg in [
        Config {
            api_url: "file:///tmp/api".into(),
            ..Config::default()
        },
        Config {
            cdn_base: "https://user:password@example.com/".into(),
            ..Config::default()
        },
        Config {
            workers: 0,
            ..Config::default()
        },
        Config {
            retry_delays_seconds: vec![0],
            ..Config::default()
        },
    ] {
        assert!(
            Config {
                auth_token: "secret".into(),
                ..cfg
            }
            .validate()
            .is_err()
        );
    }
}

#[test]
fn systems_use_resolved_settings_and_inherit_when_absent() {
    let dir = testing::temp_dir("system-settings");
    let global = MockServer::start(|_| (200, String::new()));
    let other = MockServer::start(|_| (200, String::new()));
    let mut hello = testing::hello(&dir, serde_json::to_value(config(&global)).unwrap());
    hello.systems.push(trunk_recorder_plugin::SystemInfo {
        index: 7,
        short_name: "other".into(),
        kind: "conventional".into(),
        settings: serde_json::to_value(Config {
            auth_token: "other-secret".into(),
            system_name: "Other system".into(),
            source_description: "Other source".into(),
            spaces_prefix: "other audio".into(),
            ..config(&other)
        })
        .unwrap(),
        ..Default::default()
    });
    let out = testing::run::<ScanCBus>([
        HostMessage::Hello(hello),
        HostMessage::CallConcluded(testing::call(&dir, "sys1", 1)),
        HostMessage::CallConcluded(testing::call(&dir, "other", 2)),
    ]);
    assert_eq!(out.results().len(), 2);
    assert!(out.results().iter().all(|r| r.1 == Outcome::Ok));
    for (server, token, system, source, prefix, tg) in [
        (
            &global,
            "test-secret-never-log",
            "MARCS/COIRS",
            "lptp",
            "audio_files",
            1,
        ),
        (
            &other,
            "other-secret",
            "Other system",
            "Other source",
            "other%20audio",
            2,
        ),
    ] {
        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        let body: Value =
            serde_json::from_slice(&requests.iter().find(|r| r.method == "POST").unwrap().body)
                .unwrap();
        assert_eq!(body["auth_token"], token);
        assert_eq!(body["system"], system);
        assert_eq!(body["source"], source);
        assert_eq!(body["talkgroup"], tg);
        assert_eq!(
            body["audio_file_url_path"],
            format!("{}/{prefix}/", server.url())
        );
    }
    assert!(!format!("{:?}", out.messages).contains("other-secret"));
}

#[test]
fn validates_effective_system_settings_instead_of_unused_global_destination() {
    let dir = testing::temp_dir("validate-system");
    let server = MockServer::start(|_| (200, String::new()));
    let mut hello = testing::hello(&dir, json!({}));
    hello.systems[0].settings = serde_json::to_value(config(&server)).unwrap();
    assert!(testing::run::<ScanCBus>([HostMessage::Hello(hello.clone())]).ready());
    hello.config = serde_json::to_value(config(&server)).unwrap();
    hello.systems[0].settings["authToken"] = json!("");
    let out = testing::run::<ScanCBus>([HostMessage::Hello(hello)]);
    assert_eq!(out.exit_code, EXIT_CONFIG);
    assert!(out.status().unwrap().1.contains("sys1"));
    assert!(server.requests().is_empty());
}

#[test]
fn saved_calls_use_current_overrides_and_stable_system_names() {
    let dir = testing::temp_dir("resume-system");
    let server = MockServer::start(|_| (200, String::new()));
    let call = testing::call(&dir, "sys1", 1);
    std::fs::create_dir_all(dir.join("data")).unwrap();
    std::fs::write(
        dir.join("data/queue.jsonl"),
        format!("{}\n", serde_json::to_string(&call).unwrap()),
    )
    .unwrap();
    let mut hello = testing::hello(&dir, json!({"authToken": "global-secret"}));
    hello.systems[0].index = 12;
    hello.systems[0].settings = serde_json::to_value(Config {
        system_name: "New destination".into(),
        ..config(&server)
    })
    .unwrap();
    let out = testing::run::<ScanCBus>([HostMessage::Hello(hello)]);
    assert_eq!(out.results()[0].1, Outcome::Ok);
    let requests = server.requests();
    let body: Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert_eq!(body["system"], "New destination");
    assert!(!dir.join("data/queue.jsonl").exists());
}

#[test]
fn unknown_system_fails_without_using_global_credentials() {
    let dir = testing::temp_dir("unknown-system");
    let server = MockServer::start(|_| (200, String::new()));
    let out = testing::run::<ScanCBus>([
        HostMessage::Hello(testing::hello(
            &dir,
            serde_json::to_value(config(&server)).unwrap(),
        )),
        HostMessage::CallConcluded(testing::call(&dir, "removed", 1)),
    ]);
    assert_eq!(out.results()[0].1, Outcome::Failed);
    assert!(server.requests().is_empty());
}

#[cfg(unix)]
#[test]
fn system_override_controls_spaces_command_and_destination() {
    let dir = testing::temp_dir("system-spaces");
    let args = dir.join("args");
    let server = MockServer::start(|r| (if r.method == "HEAD" { 404 } else { 200 }, String::new()));
    let mut hello = testing::hello(&dir, json!({}));
    hello.systems[0].settings = serde_json::to_value(Config {
        s3cmd_bin: fake_s3(
            &dir,
            &format!("printf '%s\\n' \"$@\" > '{}'", args.display()),
        ),
        s3cmd_config: "/tmp/system.s3cfg".into(),
        spaces_bucket: "system-bucket".into(),
        spaces_prefix: String::new(),
        ..config(&server)
    })
    .unwrap();
    let call = testing::call(&dir, "sys1", 1);
    let name = call
        .files
        .m4a
        .as_ref()
        .unwrap()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let out =
        testing::run::<ScanCBus>([HostMessage::Hello(hello), HostMessage::CallConcluded(call)]);
    assert_eq!(out.results()[0].1, Outcome::Ok);
    let args = std::fs::read_to_string(args).unwrap();
    assert!(args.starts_with("--config\n/tmp/system.s3cfg\n"));
    assert!(args.ends_with(&format!("s3://system-bucket/{name}\n")));
}
