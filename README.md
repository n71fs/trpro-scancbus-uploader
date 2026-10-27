# ScanCBus Uploader for Trunk Recorder Pro

A native Trunk Recorder Pro plugin that replaces the attached legacy upload script.
It speaks the recorder's plugin protocol using `trunk-recorder-plugin` 0.8.0 (API 1).
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

## Install on Ubuntu Server

These instructions assume Trunk Recorder Pro is already running on Ubuntu Server
and you can reach its web interface, log in over SSH, and transfer files by FTP
or SFTP. The plugin runs **on the Ubuntu server**, not on the computer running
your browser. A desktop session on the server is not needed.

Use the same Linux account and config file as your running recorder. In the
examples below the account is `recorder`; replace that name and `YOUR_SERVER`
with your actual account and server address. If you run the recorder through
systemd, check its service's `User=` and `ExecStart=` to identify the account and
any `--config` path. Run sudo commands from an administrator account if the
recorder account has no sudo access; run s3cmd configuration and the plugin CLI
as the recorder account. Installing under your SSH account's default config will not
add the plugin to a recorder using another account or config.

### 1. Install the server dependencies over SSH

Log in using an account with sudo access:

```sh
ssh recorder@YOUR_SERVER
sudo apt update
sudo apt install -y ffmpeg s3cmd ca-certificates
ffmpeg -version
/usr/bin/s3cmd --version
uname -m
```

Use the Linux **x86-64** release for `x86_64` (most Intel/AMD servers), or the
Linux **ARM64** release for `aarch64` (including a Raspberry Pi running 64-bit
Ubuntu). The supplied binaries do not support 32-bit ARM. Rust is only needed
if you choose to build from source.

### 2. Configure DigitalOcean Spaces for the recorder account

Run this **as the account that runs Trunk Recorder Pro**, not as root. If you
logged in using a different account, switch with `sudo -iu recorder` first.

```sh
/usr/bin/s3cmd --configure
chmod 600 ~/.s3cfg
```

Enter your Spaces access key and secret key in that interactive command. For
the default NYC3 bucket, use:

- S3 endpoint / host: `nyc3.digitaloceanspaces.com`
- DNS-style bucket host: `%(bucket)s.nyc3.digitaloceanspaces.com`
- HTTPS: enabled

Use the corresponding region if your bucket is elsewhere. Confirm that the
recorder account can reach your bucket:

```sh
/usr/bin/s3cmd --config /home/recorder/.s3cfg ls s3://scancbus/
```

This lists the bucket without uploading a call. Enter credentials through SSH;
FTP is only needed to transfer release files. If you already have a working
`.s3cfg` from the old script, reuse it instead of running `--configure` again.

### 3. Install the plugin through the remote web interface

Once a GitHub release and its build assets are available:

1. Open your existing Trunk Recorder Pro web interface.
2. Go to **Plugins → Install & settings → Install from GitHub…**.
3. Enter `https://github.com/n71fs/trpro-scancbus-uploader` and tag `v0.2.0`.
4. The recorder downloads the correct Linux build to the **server**, checks its
   release checksums, and adds the plugin. It is initially off.

You can alternatively install over SSH, as the recorder account:

```sh
trunk-pro plugin install https://github.com/n71fs/trpro-scancbus-uploader --tag v0.2.0
```

If your running recorder uses a custom config, use that same file:

```sh
trunk-pro plugin install https://github.com/n71fs/trpro-scancbus-uploader --tag v0.2.0 --config /path/to/your/config.json
```

If the repository is private or the server cannot download the release, use
the file-transfer method below. A tag alone is not enough: installation from
GitHub needs a completed release with the Linux archive, manifest, and
`SHA256SUMS`.

### Alternative: upload a build by FTP or SFTP

On your own computer, open the repository's
[Releases page](https://github.com/n71fs/trpro-scancbus-uploader/releases).
Download the Linux archive for your server's architecture, the matching
`scancbus-uploader-0.2.0.manifest.json`, and `SHA256SUMS` from the same release.
For x86-64 the archive is
`scancbus-uploader-0.2.0-x86_64-unknown-linux-gnu.tar.gz`; for ARM64 it is
`scancbus-uploader-0.2.0-aarch64-unknown-linux-gnu.tar.gz`.

Over SSH, create a transfer folder that your FTP/SFTP account can write to:

```sh
mkdir -p ~/uploads/scancbus-0.2.0
```

Use your FTP client to upload those three files there in **binary** transfer
mode. SFTP uses your SSH login and preserves the file bytes automatically.
Your FTP client may show a different starting directory; use the directory
that maps to this server folder. Then return to SSH:

```sh
cd ~/uploads/scancbus-0.2.0
# Use aarch64-unknown-linux-gnu here instead on an ARM64 server.
archive=scancbus-uploader-0.2.0-x86_64-unknown-linux-gnu.tar.gz
test -f "$archive" && test -f scancbus-uploader-0.2.0.manifest.json && sha256sum --ignore-missing -c SHA256SUMS
```

Continue only if the archive and manifest both report **OK**. The checksum list
also contains builds for other platforms; `--ignore-missing` skips those files.
Extract the archive and put its executable at a stable server path:

```sh
tar -xzf "$archive"
sudo install -d -m 755 /opt/scancbus-uploader
sudo install -m 755 "${archive%.tar.gz}/scancbus-uploader" /opt/scancbus-uploader/scancbus-uploader
/opt/scancbus-uploader/scancbus-uploader --describe
```

For a locally built executable rather than a release archive, transfer it in
binary mode and use `sudo install -m 755` to put it at the same `/opt` path.
Verify it with `--describe`; it should report id `scancbus-uploader`, API `1`,
and M4A audio. Transferred executables need execute permission, which `install`
sets even when FTP does not preserve permissions.

In the **remote web interface**, select **Plugins → Install & settings → Add
from a file…** and enter:

```text
/opt/scancbus-uploader/scancbus-uploader
```

This is a path **on the Ubuntu server**, not a file on your desktop. The
recorder runs that file in place; leave it at that path. It appears as **your
build** and is initially off. Updating this kind of installation means replacing
the executable at the same path, then turning the plugin off and on.

### 4. Enter settings and start uploading

In the remote web interface, open **Setup → Plugins → ScanCBus Uploader**:

1. Enter your ScanCBus `authToken`.
2. Set `s3cmdBin` to `/usr/bin/s3cmd`.
3. Set `s3cmdConfig` to `/home/recorder/.s3cfg`, using the recorder account's
   actual absolute path. This avoids relying on a service's home directory.
4. Keep the defaults for MARCS/COIRS, lptp, the scancbus bucket, NYC3 CDN, and
   retry delays, or change them for your setup.
5. Turn the plugin **On**.

Under **Setup → Recording → M4A audio**, choose **Automatic** or **ffmpeg**,
not **None: WAV only**. The plugin requires real M4A audio and will report a
configuration error if no encoder is available. Systems inherit the plugin's
settings unless you override them on the system's plugin card, as described below.

Start recording using the web interface. If it was already recording, changing
plugin settings restarts the plugin automatically. Check **Plugins → Install &
settings** for its log and **Plugins → Health** for upload/retry counts. After a
recorded call, expect a log such as `API RESULT: NEW id=… tg=… quality=…`, and
confirm the call appears on ScanCBus. With recording stopped, **Runs while
recording** is normal; the plugin does not run on its own.

If a check fails, verify that the recorder account can read the `.s3cfg` and
execute the plugin, that ffmpeg is enabled, and that the server can reach
ScanCBus, your Spaces region endpoint, and your CDN. For a systemd installation,
`journalctl -u YOUR_RECORDER_SERVICE -n 100 --no-pager` also shows recent recorder
logs; replace the service name with your actual one.

### Remote web access over SSH

Keep using your existing web access if it is already configured. If the
recorder only listens on the server's loopback address, open a tunnel **from
your own computer**:

```sh
ssh -N -L 8080:127.0.0.1:8080 recorder@YOUR_SERVER
```

Leave that SSH session open and open `http://localhost:8080` in your browser.
Use the recorder's actual port if it differs. Trunk Recorder Pro's web interface
has no built-in login; use the tunnel or an authenticated reverse proxy for
access across the internet.

### Optional: build on Ubuntu instead of downloading

Install Rust 1.85 or newer using [rustup](https://rustup.rs/), then run these
commands over SSH. Build tools are only needed for this method:

```sh
sudo apt install -y build-essential git
git clone https://github.com/n71fs/trpro-scancbus-uploader.git
cd trpro-scancbus-uploader
cargo build --locked --release
cargo test --locked
./target/release/scancbus-uploader --describe
sudo install -d -m 755 /opt/scancbus-uploader
sudo install -m 755 target/release/scancbus-uploader /opt/scancbus-uploader/scancbus-uploader
```

Then use **Add from a file…** with the `/opt` path and complete step 4 above.

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
| `retryDelaysSeconds` | `[10, 60, 300, 900]` | Delay before each additional attempt; recorder-wide |
| `workers` | `2` | Parallel call uploads per system (1–16); recorder-wide |

### Per-system overrides

On a system's card in **Setup**, open **Plugins → ScanCBus Uploader** and use
Trunk Recorder Pro's override controls to change its upload settings. For example,
override `systemName` for a different ScanCBus system, or `authToken`, `apiUrl`,
`spacesBucket`, `spacesPrefix`, and `cdnBase` for a different destination.
`sourceDescription`, s3cmd paths, and HTTP/upload timeouts also follow the system's
resolved settings. Fields without an override inherit the recorder-wide settings.
Keep `spacesPrefix` and `cdnBase` consistent with the bucket's public URL.

The recorder supplies the merged settings; no separate list of systems or duplicate
settings form is needed in this plugin. Each system has its own HTTP client and
upload lane, so a slow or failing destination does not block another system.
`workers` and `retryDelaysSeconds` are recorder-wide queue controls: set them on the
main plugin card. Their per-system overrides do not change queue behavior.

Pending calls are matched by the recorder system's short name, independent of its
numeric index or overridden ScanCBus `systemName`. On restart they use that system's
current settings. A pending call whose system has been removed fails visibly rather
than uploading with another system's credentials.

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
holds up to 10,000 waiting calls per system; overflowing calls fail visibly.

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

Over SSH, as the recorder account, create a private `settings.local.json`
(ignored by Git) and restrict it with `chmod 600 settings.local.json`. Use the
installed executable path from `trunk-pro plugin list` if you installed through
GitHub rather than at `/opt/scancbus-uploader/scancbus-uploader`:

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
trunk-pro plugin run /opt/scancbus-uploader/scancbus-uploader /path/to/recordings --limit 5 --settings settings.local.json
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
permanent errors, secret-free logs, shutdown interruption, per-system destinations,
inherited settings, and saved-queue recovery across changed system indexes without
real credentials or a radio.

CI runs those checks and builds the executable. A `v<version>` tag matching
Cargo.toml triggers the release workflow to build Linux x86-64/ARM64, macOS
universal, and Windows archives, plus the manifest, checksums, and provenance
attestations for public repositories. For a missed tag event, open GitHub
**Actions → Release → Run workflow**, select `main`, and enter the existing tag
(such as `v0.2.0`). The workflow builds the tagged source and can populate an
existing release with its missing assets. Wait for it to finish before installing
from GitHub. A completed release can be installed with **Install from GitHub…**;
registry submission additionally requires choosing a license.

References: [plugin author guide](https://github.com/TrunkRecorder/trunk-recorder-pro/blob/main/docs/plugins/writing-plugins.md)
and [plugin lifecycle and installation](https://github.com/TrunkRecorder/trunk-recorder-pro/tree/main/docs/plugins).
