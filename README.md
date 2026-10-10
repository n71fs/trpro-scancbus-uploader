# ScanCBus Uploader for Trunk Recorder Pro

A native Trunk Recorder Pro plugin that replaces the attached legacy upload script.
It speaks the recorder's plugin protocol using `trunk-recorder-plugin` 0.2.1 (API 1).
Network and Spaces uploads run on worker threads, leaving the event reader free.

For each concluded call it:

1. Uses the recorder's M4A file and checks its public CDN URL with HEAD.
2. If the CDN does not return 200, runs `s3cmd put --acl-public` to DigitalOcean Spaces.
3. Sends the original ScanCBus JSON payload to `/api_v2/import_transmission/`.
4. Reads `X-Transmission-Id`, `X-Transmission-Deduped`, and
   `X-Transmission-Replaced`, logging **NEW**, **DEDUPED**, or **REPLACED**.
5. Reports success/failure and retry queue health to the recorder.

The original system name (`MARCS/COIRS`), source (`lptp`), bucket (`scancbus`),
object prefix (`audio_files`), API URL, and CDN URL are the defaults. The auth
token has no default. The payload preserves the frequency, normalized radio
IDs (`srcList`), emergency flag, basename, and quality formula. Play length is
integer seconds, at least one second; stop time is start time plus play length.
Pro supplies `call_length`, so the old script's missing-duration fallbacks are
not needed. A zero duration is handled safely in the quality calculation.

## Build and install

Prerequisites:

- Rust 1.85 or newer to build; Rust is not needed to run the compiled binary.
- Trunk Recorder Pro with its plugin API 1.
- `ffmpeg` available to the **recorder**, so it can encode M4A. The plugin asks
  for M4A and gives a configuration error if encoding is unavailable; it never
  labels a WAV file as M4A.
- `s3cmd` installed on the recording machine and configured for your Spaces
  account. The binary is native, but Spaces transfers still use this external
  tool, just like the original script.

```sh
cargo build --locked --release
./target/release/scancbus-uploader --describe
cargo test --locked
```

On Linux, the executable is `target/release/scancbus-uploader`; on Windows it
has the `.exe` suffix. In Trunk Recorder Pro's Plugins page, choose **Add from
a file…** and select that executable. Fill in the plugin's settings and start
recording. All recorded systems use the configured ScanCBus system name, as in
the original script. No per-system settings are required.

Configure `s3cmd` **as the same user who runs Trunk Recorder Pro**. For Spaces,
set its S3 host to your region (for example `nyc3.digitaloceanspaces.com`) and
its bucket host pattern to `%(bucket)s.nyc3.digitaloceanspaces.com`, enable
HTTPS, and supply your Spaces access/secret keys through s3cmd's configuration.
Keep that file private (`chmod 600 ~/.s3cfg` on Unix). The plugin uses that
file implicitly, or you can set `s3cmdConfig` to its absolute path. It never
logs the configuration or token. Use an absolute `s3cmdBin` path if the
recorder's PATH cannot find s3cmd.

## Settings

The settings form uses these camelCase names (also usable with the replay CLI):

| Setting | Default | Purpose |
| --- | --- | --- |
| `apiUrl` | `https://scancbus.com/api_v2/import_transmission/` | JSON import endpoint |
| `authToken` | empty, required | ScanCBus token; hidden in the settings form |
| `systemName` | `MARCS/COIRS` | Forced ScanCBus system name |
| `sourceDescription` | `lptp` | Recorder/source label |
| `spacesBucket` | `scancbus` | Spaces bucket |
| `spacesPrefix` | `audio_files` | Object directory; empty means bucket root |
| `cdnBase` | `https://scancbus.nyc3.cdn.digitaloceanspaces.com/` | Public URL root |
| `s3cmdBin` | `s3cmd` | Executable path/name |
| `s3cmdConfig` | empty | Optional absolute `.s3cfg` path |
| `httpTimeoutSeconds` | `5` | Per-request timeout |
| `uploadTimeoutSeconds` | `60` | Maximum time for a Spaces upload attempt |
| `retryDelaysSeconds` | `[10, 60, 300, 900]` | Delay before each additional attempt |
| `workers` | `2` | Parallel call uploads (1–16) |

The CDN and API URLs should use HTTPS in production. Redirects are not followed,
so an import request containing the token cannot be forwarded to another URL.
The Spaces key uses the audio **basename**, preserving the legacy behavior.
Choose separate object prefixes for recorders whose filenames might collide.

## Retry behavior and recovery

The defaults allow an initial attempt plus four retries, with waits of 10s,
60s, 5min, and 15min. Configure another list to change the number of attempts
and backoff; `[]` disables retries. Spaces failures/timeouts, API network
failures, HTTP 408/425/429, and 5xx responses retry. Other API statuses fail
immediately; like the original script, **only HTTP 200 counts as success**.
A missing M4A or unusable s3cmd executable also fails immediately.

After Spaces succeeds, API retries in the current process normally skip the
Spaces step. After restarting, the plugin checks the CDN again; a delayed CDN
may cause a harmless overwrite of the same object. A lost API response may
cause a repeated import; the server's transmission deduplication/replacement
behavior handles this, rather than the plugin promising exactly-once delivery.

The SDK sends `call.result` only on success, permanent failure, or retry
exhaustion. While retrying, the recorder retains the call's files. The queue
holds up to 10,000 waiting calls; overflowing calls fail visibly.

On graceful shutdown, pending calls are saved as `queue.jsonl` in the plugin's
data folder and resumed at the next start using the **current settings**.
Tokens are not saved in the queue. The retry schedule starts over after restart.
The plugin interrupts slow s3cmd uploads during shutdown so they can be queued.
An in-flight HTTP request is bounded by `httpTimeoutSeconds`: keep it at the
5-second default for the recorder's usual 10-second shutdown grace period.
A larger timeout or unusually short grace period can cause a forced kill.
The SDK saves on graceful shutdown, not continuously; crashes, power loss, or
forced termination can lose pending queue entries. Keep recordings until
uploads are confirmed if recovery from such failures is required.

A failed/exhausted call is terminal and is not automatically retried on the
next restart. After fixing the problem, retained calls can be replayed using
`trunk-pro plugin run`. If you need to retain failed audio, configure the
recorder's Files settings accordingly.

## Replay recorded calls

Create a private `settings.local.json` (ignored by Git):

```json
{
  "config": {
    "authToken": "YOUR_SCANC_BUS_TOKEN",
    "s3cmdBin": "/usr/bin/s3cmd",
    "retryDelaysSeconds": [10, 60, 300, 900]
  },
  "systems": {}
}
```

```sh
trunk-pro plugin run ./target/release/scancbus-uploader /path/to/recordings --limit 5 --settings settings.local.json
```

**Replay performs real uploads.** Use a test bucket and API endpoint when
validating new settings. Plugin logs appear in the recorder interface; the
legacy `~/script_a.log` and command-line WAV/JSON/audio argument interface are
replaced by the native plugin protocol.

## Development and releases

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

Tests use local HTTP servers and fake s3cmd executables. They verify payload
compatibility, CDN hits, Spaces arguments, transient failures, retries,
permanent errors, secret-free logs, shutdown interruption, and saved-queue
recovery without real credentials or a radio.

CI runs those checks and builds the executable. The included release workflow,
when you choose to push a `v<version>` tag matching Cargo.toml, builds the
recorder's standard Linux x86-64/ARM64, macOS universal, and Windows archives,
manifest, checksums, and provenance attestations. No release has been published
by creating these files. A released plugin can be installed with **Install
from GitHub…**; registry submission additionally requires choosing a license.

References: [plugin author guide](https://github.com/TrunkRecorder/trunk-recorder-pro/blob/main/docs/plugins/writing-plugins.md)
and [plugin lifecycle and installation](https://github.com/TrunkRecorder/trunk-recorder-pro/tree/main/docs/plugins).
