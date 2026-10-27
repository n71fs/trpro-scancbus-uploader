# Changelog

## [0.2.0]

- Update to plugin SDK 0.8.0 and use each system's resolved plugin settings for uploads.
- Isolate system upload lanes and clients, retaining recorder-wide worker and retry controls.
- Recover queued calls by stable system short name using current overrides; reject removed systems.
- Read the SDK's typed frequency-error field for quality scoring.

## [0.1.0]

- Add a native Trunk Recorder Pro API 1 plugin for ScanCBus.
- Upload M4A recordings to DigitalOcean Spaces using s3cmd, with public CDN checks.
- Preserve the legacy metadata payload, quality score, and NEW/DEDUPED/REPLACED reporting.
- Add configurable upload retries, backoff, and worker concurrency.
- Resume pending uploads after graceful shutdown using the SDK's saved queue.
- Expose plugin settings with a hidden authentication token and secret-free logs.

Requires ffmpeg in the recorder and s3cmd configured for your Spaces account.
Pending calls are saved on graceful shutdown; abrupt termination can lose queue entries.
