# Changelog

## [0.1.0]

- Add a native Trunk Recorder Pro API 1 plugin for ScanCBus.
- Upload M4A recordings to DigitalOcean Spaces using s3cmd, with public CDN checks.
- Preserve the legacy metadata payload, quality score, and NEW/DEDUPED/REPLACED reporting.
- Add configurable upload retries, backoff, and worker concurrency.
- Resume pending uploads after graceful shutdown using the SDK's saved queue.
- Expose plugin settings with a hidden authentication token and secret-free logs.

Requires ffmpeg in the recorder and s3cmd configured for your Spaces account.
Pending calls are saved on graceful shutdown; abrupt termination can lose queue entries.
