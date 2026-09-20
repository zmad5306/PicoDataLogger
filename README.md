# Pico Data Logger

Bare-metal Rust firmware that reads temperature and humidity from an SHT40, assigns synchronized UTC timestamps, and publishes JSON readings over MQTT from a Raspberry Pi Pico 2 W.

The firmware is designed to run unattended: it services MQTT between samples, retries transient failures indefinitely with bounded delays, rebuilds stale network state at the appropriate layer, and keeps USB diagnostics active during recovery.

Project documentation:

- This README is the build, deployment, operation, troubleshooting, and acceptance-test runbook.
- [Architecture](docs/architecture.md) explains the async tasks, hardware interfaces, protocol layers, fixed-memory model, clock anchor, and recovery supervisor.
- [CYW43439 firmware provenance](firmware/README.md) records the bundled radio firmware source and hashes.

## Hardware

The project uses:

- Raspberry Pi Pico 2 W
- Adafruit SHT40 temperature and humidity sensor (I2C)
- Qwiic-compatible cable

### Hardware wiring

The SHT40 is connected to the Pico 2 W using the following Qwiic wire mapping. Physical pin numbers refer to the numbered pins on the Pico 2 W header.

| Qwiic wire | Signal | Pico 2 W pin | Physical pin number |
| --- | --- | --- | ---: |
| Red | Power | 3V3 OUT | 36 |
| Black | Ground | GND | 3 |
| Blue | SDA (data) | GP0 | 1 |
| Yellow | SCL (clock) | GP1 | 2 |

The I2C data and clock lines use GP0 and GP1, respectively.

### Wireless power management

The firmware explicitly configures the CYW43439 to use Embassy's `PowerSave` mode. This provides a balanced default for a continuously running data logger: the radio conserves power while remaining responsive enough for periodic network and MQTT activity.

## Build and deploy

The Pico 2 W contains an RP2350A, and this project builds for its Arm Cortex-M33 cores. The repository's `.cargo/config.toml` selects `thumbv8m.main-none-eabihf` automatically, so the commands below should be run from the repository root.

### Prerequisites

Install the Rust toolchain and target declared in `rust-toolchain.toml`. Also install an `elf2uf2-rs` revision that supports selecting the RP2350 UF2 family:

```sh
cargo install --git https://github.com/JoNil/elf2uf2-rs \
  --rev f14bf2d981772cd9fbe5bac33b685719caac1add \
  --locked --force
```

Confirm that the converter offers the `rp2350-arm-s` family:

```sh
elf2uf2-rs convert --help
```

Do not use an older converter that only accepts an input and output path. Those versions default to the RP2040 UF2 family, which the RP2350 bootloader ignores.

### Compile-time application configuration

Copy the ignored local environment-file template and replace its placeholders with your Wi-Fi and MQTT settings:

```sh
cp .env.example .env
```

Both deployment scripts automatically load `.env` from the repository root before compiling. The file is ignored by Git so local credentials are not committed. Do not remove `.env` from `.gitignore` or put real credentials in `.env.example`.

Alternatively, set the required values in PowerShell 7 before building or running the deployment script:

```powershell
$env:WIFI_SSID = "your-network-name"
$env:WIFI_PASSWORD = "your-network-password"
$env:MQTT_HOST = "broker.example.com"
```

The remaining settings are optional:

```powershell
$env:MQTT_PORT = "1883"
$env:MQTT_TOPIC = "pico-data-logger/readings"
$env:MQTT_CLIENT_ID = "home-office"
$env:MQTT_USERNAME = "your-mqtt-username"
$env:MQTT_PASSWORD = "your-mqtt-password"
$env:NTP_HOST = "pool.ntp.org"
```

In Bash or Zsh, export the same values before building or running the macOS deployment script:

```bash
export WIFI_SSID="your-network-name"
export WIFI_PASSWORD="your-network-password"
export MQTT_HOST="broker.example.com"

export MQTT_PORT="1883"
export MQTT_TOPIC="pico-data-logger/readings"
export MQTT_CLIENT_ID="home-office"
export MQTT_USERNAME="your-mqtt-username"
export MQTT_PASSWORD="your-mqtt-password"
export NTP_HOST="pool.ntp.org"
```

For anonymous MQTT, ensure both optional credentials are absent:

```bash
unset MQTT_USERNAME MQTT_PASSWORD
```

`MQTT_USERNAME` and `MQTT_PASSWORD` must either both be set or both be absent. The optional settings use their documented defaults when omitted.

`NTP_HOST` uses its documented default when omitted.

These values are read by `option_env!` during compilation. `MQTT_CLIENT_ID` is the human-readable logical device name and defaults to `pico-data-logger`; the firmware appends `-<hardware_id>` to form the unique broker connection ID. They are embedded in the resulting firmware binary, so compile-time configuration prevents accidental source-control commits but does not make credentials secret from someone who obtains the binary. Do not commit real credentials or generated firmware containing them.

### Automated build and flash

With the Pico mounted in BOOTSEL mode, use the script for your development machine.

On macOS with Bash:

```sh
./scripts/deploy-macos.sh
```

On macOS, Linux, or Windows with PowerShell 7 (`pwsh`):

```powershell
./scripts/deploy.ps1
```

The scripts build the release ELF, convert it with the required `rp2350-arm-s` family, copy the UF2, wait for the BOOTSEL volume to disappear, find the USB serial device, and immediately start monitoring it. This makes the deployment command a single build-to-observation workflow and minimizes the chance of missing early buffered logs. They write the generated UF2 under `target/`, so deployment does not leave an untracked artifact in the repository root.

The Bash monitor uses `screen`; exit it with `Ctrl-A`, then `K`, then `Y`. The PowerShell monitor reads the serial port directly; stop it with `Ctrl-C`.

The Bash script accepts a different mount point as its first argument if needed:

```sh
./scripts/deploy-macos.sh /Volumes/RP2350
```

The PowerShell script detects the standard `RP2350` mount location for the current platform, or accepts an explicit path:

```powershell
./scripts/deploy.ps1 -MountPoint /Volumes/RP2350
```

On Windows, an explicit mount point uses its drive-letter path, such as `-MountPoint R:\`.

The manual macOS workflow below documents each operation performed by the scripts.

### 1. Build the release ELF

```sh
cargo build --release
```

The resulting Arm ELF is:

```text
target/thumbv8m.main-none-eabihf/release/pico-data-logger
```

An ELF contains the machine code together with section, symbol, and debug information useful to development tools. The Pico's ROM bootloader expects that program packaged into UF2 blocks for USB deployment.

### 2. Convert the ELF for the Pico 2

```sh
elf2uf2-rs convert --family rp2350-arm-s \
  target/thumbv8m.main-none-eabihf/release/pico-data-logger \
  pico-data-logger.uf2
```

The `rp2350-arm-s` argument is required. It gives the UF2 the RP2350 Secure Arm family ID instead of the converter's RP2040 default.

### 3. Enter BOOTSEL mode

1. Unplug the Pico 2 W.
2. Hold the **BOOTSEL** button.
3. Connect the USB cable while continuing to hold **BOOTSEL**.
4. Release the button after the drive mounts.

Confirm that macOS mounted the ROM bootloader's mass-storage volume:

```sh
ls /Volumes/RP2350
```

It normally contains `INDEX.HTM` and `INFO_UF2.TXT`.

### 4. Flash and reboot

Copy the UF2 to the mounted bootloader volume:

```sh
cp pico-data-logger.uf2 /Volumes/RP2350/
```

After accepting the complete UF2, the ROM bootloader writes the application to external flash and automatically reboots. `/Volumes/RP2350` should disappear within a few seconds. Once the CYW43439 is initialized, the onboard LED enters its rapid-flash startup state until the first sample attempt takes priority.

### 5. Connect to USB serial

Find the serial device created by the running firmware:

```sh
ls /dev/cu.usbmodem*
```

Connect using the exact device name returned by that command:

```sh
screen /dev/cu.usbmodem101 115200
```

The numeric suffix can change after a reboot, so do not assume it will always be `101`. USB CDC does not use the baud rate in the same way as a hardware UART, but `115200` is a conventional value accepted by terminal programs.

To leave `screen`, press `Ctrl-A`, then `K`, then `Y`.

## Deployment troubleshooting

### `RP2350` remains mounted after copying

Inspect the UF2 header:

```sh
xxd -g4 -l 32 pico-data-logger.uf2
```

At offset `0x1c`, an RP2350 Secure Arm image contains `59ff8be4`, the little-endian representation of family ID `0xe48bff59`. If it contains `56ff8be4`, the file was incorrectly generated for an RP2040. Reinstall the converter shown above and repeat the conversion with `--family rp2350-arm-s`.

### No USB serial device appears

First check whether `/Volumes/RP2350` is still mounted. If it is, the ROM bootloader is active and the application is not running. Rebuild and reconvert the ELF, checking the UF2 family as described above.

If the BOOTSEL volume disappeared, refresh the serial-device name:

```sh
ls /dev/cu.usbmodem*
system_profiler SPUSBDataType
```

Reconnect with the newly reported device instead of reusing a stale name from an earlier boot.

## Runtime architecture

The firmware uses independently scheduled Embassy tasks for USB logging, the CYW43439 radio, and the network stack. The main application owns the SHT40, time synchronization, MQTT session, and recovery supervisor. A blocked or failed network operation therefore does not intentionally stop USB diagnostics or the network driver tasks. See the [architecture document](docs/architecture.md) for the detailed task, protocol, ownership, and recovery design.

The data path is:

```text
SHT40 over I2C
  -> fixed-point measurement conversion
  -> reading with logical/hardware identity, sequence, synchronized UTC, and uptime
  -> append-only onboard-flash queue
  -> JSON encoded into a reusable fixed-size buffer
  -> MQTT QoS 1 publication over TCP/Wi-Fi
  -> local retirement after broker PUBACK
  -> broker subscriber
```

The application does not allocate a new JSON or network buffer for every reading. Its JSON, UDP, TCP, and MQTT packet buffers have fixed sizes and are reused.

## Normal operation

At boot the firmware:

1. Starts USB logging and the radio/network tasks.
2. Reads the SHT40 serial number.
3. Joins the configured Wi-Fi network and waits for DHCP.
4. Resolves the NTP host and waits until it obtains a valid UTC clock anchor.
5. Resolves the MQTT broker, opens TCP, and establishes the MQTT session.
6. Recovers the flash-backed measurement queue, then measures once every 60 seconds whether or not MQTT is available.
7. Publishes queued measurements oldest-first with MQTT QoS 1, removing each record only after its PUBACK arrives.

Each reading is timestamped when the measurement is captured, before JSON encoding and network delivery. The firmware publishes both absolute UTC and uptime because they answer different questions: UTC identifies when the measurement occurred, while uptime helps identify reboots and how long the current run has lasted.

The UTC anchor is refreshed after network recovery and at least once per day. A failed refresh does not replace a valid anchor with uptime; it retains the last valid anchor and retries with bounded backoff.

### Observe MQTT readings

Subscribe before or after flashing the Pico:

```sh
mosquitto_sub \
  -h broker.example.com \
  -p 1883 \
  -V 5 \
  -t pico-data-logger/readings \
  -v
```

For a broker running in Docker, the same check can be run inside its container:

```sh
docker exec -it <mosquitto-container> \
  mosquitto_sub \
  -h localhost \
  -p 1883 \
  -V 5 \
  -t pico-data-logger/readings \
  -v
```

Use the configured host, port, topic, and authentication flags when they differ from the defaults. A reading has this shape:

```json
{"device_id":"basement-sensor","hardware_id":"0123456789abcdef","sequence":42,"temperature_c":21.34,"relative_humidity_pct":43.13,"timestamp_unix_s":1789916205,"uptime_s":16}
```

| Field | Meaning |
| --- | --- |
| `device_id` | Human-readable logical identity from `MQTT_CLIENT_ID`, such as `home-office`. |
| `hardware_id` | Stable 16-digit hexadecimal RP2350 chip ID read from OTP memory. It is an identifier, not a secret. |
| `sequence` | Monotonically increasing record identifier; repeated values identify at-least-once replay after a reset. |
| `temperature_c` | SHT40 temperature in degrees Celsius. |
| `relative_humidity_pct` | SHT40 relative humidity percentage. |
| `timestamp_unix_s` | UTC Unix timestamp captured with the measurement. |
| `uptime_s` | Whole seconds since this firmware booted. |

The MQTT connection client ID combines both identities as `<device_id>-<hardware_id>`, preventing two physical Picos with the same human-readable configuration from disconnecting each other. Publications use MQTT QoS 1 and are not retained. The firmware keeps each flash record until the broker acknowledges its publication. If power fails after broker delivery but before local retirement, that record is sent again after reboot; consumers should use `(hardware_id, sequence)` to recognize the duplicate. `device_id` remains convenient for human-facing grouping and can intentionally survive hardware replacement.

### Offline queue capacity and wear

The final 256 KiB of onboard flash is reserved for measurements and excluded from the firmware link region. Records occupy one 256-byte flash page and include a format version, sequence number, original Unix timestamp, temperature, humidity, uptime, and CRC-32 integrity check. One 4-KiB erase sector is retained as circular working space, leaving 1,008 usable records—16 hours and 48 minutes at one reading per minute.

Writes are append-only. A record becomes visible only after its body and checksum have been written and a final commit byte is programmed. Recovery ignores incomplete or corrupt records and reconstructs FIFO order from sequence numbers. Acknowledgment and commit markers only clear flash bits; sectors are erased in 16-record batches instead of once per measurement. When all 1,008 positions are occupied, the oldest measurement is explicitly discarded so newer outage data continues to be captured.

## Recovery behavior

The application is supervised indefinitely rather than stopping after a fixed number of attempts. MQTT reconnection and UTC-refresh retries use delays of 1, 2, 4, 8, 16, 32, and then at most 60 seconds. Successful recovery resets the applicable backoff. Wi-Fi join attempts have a 15-second timeout and retry after five seconds.

Recovery discards state from the failed layer upward:

- A failed SHT40 read skips one publication and retries at the next 60-second sample without dropping a healthy MQTT session.
- A broker DNS, TCP, MQTT handshake, publish, keepalive, or disconnect failure drops the MQTT connection and TCP socket while the 60-second sampler continues appending timestamped readings to flash. The next attempt resolves the broker again, creates fresh transport state, and replays the backlog before newly captured readings.
- A lost Wi-Fi link explicitly clears stale CYW43439 association state, then returns to Wi-Fi join and DHCP before DNS, NTP, TCP, and MQTT are rebuilt.
- A UTC refresh failure retains the last valid clock anchor and retries with the same bounded-backoff policy. A successful refresh resets that backoff and schedules the next daily refresh.

The USB logger remains a separate task throughout these paths, so the serial log should continue reporting recovery transitions while the application waits.

### Onboard status LED

The Pico 2 W onboard LED is wired to the CYW43439 radio module's `WL_GPIO0`, not to an RP2350 GPIO such as GP25. The firmware therefore drives it through the same initialized CYW43439 control path used for Wi-Fi:

| Pattern | Meaning |
| --- | --- |
| Off | Healthy waiting between samples after a broker-acknowledged publication |
| Solid on | A sample is being measured, converted, queued, encoded, or submitted to MQTT |
| Rapid flash (100 ms on / 100 ms off) | A sensor, conversion, storage, configuration, Wi-Fi, DNS, NTP, TCP, or MQTT fault is being reported or recovered |

Startup uses the rapid-flash state because the logger has not yet completed a publish cycle. The firmware keeps advancing the 100 ms flash phase while it awaits Wi-Fi association, network link, DHCP, DNS, NTP, TCP, and MQTT setup. Non-radio futures stay pinned and are not restarted on each LED tick. Wi-Fi association uses the repository's narrowly patched `cyw43::Control::join_with_gpio_flash`, which services `WL_GPIO0` inside the driver's association-event loop because `Control::join` otherwise holds the only control handle. A new sample attempt temporarily changes the indication to solid on. A failure returns it to rapid flashing. With the current QoS 1 queue, the LED returns to off only after the broker's PUBACK is received and the corresponding flash record is retired. PUBACK confirms broker receipt, but it does not prove that any subscriber processed the reading.

## Recovery troubleshooting and acceptance checks

Keep the USB serial monitor and MQTT subscriber visible during each check. Do not reset the Pico while testing recovery.

### Sensor interruption

1. Confirm readings are arriving every 60 seconds; the LED is off while waiting, solid during a sample, and off again after its publication is acknowledged.
2. Disconnect the SHT40, wait for a sample, and confirm an I2C failure is logged and the LED rapidly flashes without a reboot.
3. Reconnect the sensor using 3V3, GND, GP0/SDA, and GP1/SCL.
4. Confirm a later sample is published, the LED returns to off after PUBACK, and `uptime_s` continues increasing.

### Broker outage

1. Subscribe to the readings topic and note the latest `sequence` and `timestamp_unix_s`.
2. Block broker access for at least three 60-second sample intervals and confirm serial queue depth grows while the LED rapidly flashes between solid sample attempts.
3. Power-cycle the Pico while broker access remains blocked; confirm recovery logs the preserved queue depth.
4. Restore broker access and observe the queued timestamps arrive in ascending sequence order before normal live publishing resumes; confirm the LED returns to off after the backlog is acknowledged.
5. Confirm any duplicate carries the same sequence number and that recovery requires no manual reset.

If the broker repeatedly reports a timeout, confirm the firmware services the MQTT connection between samples and that its keepalive is not being blocked by a firewall or container networking rule.

### Wi-Fi interruption

1. Interrupt the configured Wi-Fi network long enough for the Pico to detect link loss.
2. Restore the network.
3. Confirm the log returns through Wi-Fi join, DHCP, UTC refresh, broker DNS, TCP, and MQTT.
4. Confirm publications resume and `uptime_s` did not restart.

### NTP unavailable at boot

1. Make the configured NTP service unreachable before booting the Pico.
2. Confirm the firmware reports that time remains unsynchronized and does not publish readings with uptime substituted for UTC.
3. Restore NTP reachability.
4. Confirm synchronization succeeds and normal MQTT publishing begins without resetting the Pico.

### Common network failures

- **DNS resolution fails:** verify DHCP supplied reachable DNS servers and that `MQTT_HOST` and `NTP_HOST` are valid from the Pico's network.
- **TCP is reset or times out:** verify the broker is running, listening on `MQTT_PORT`, published through its Docker/network configuration, and allowed by host and network firewalls.
- **MQTT handshake fails:** verify the protocol listener, client ID policy, and the username/password pair. A second connection using the same client ID can cause a broker to disconnect the older client.
- **No subscriber output:** verify the subscriber uses the configured topic and authentication. Broker access must recover before the device can replay its flash backlog.
- **Timestamps are implausible:** inspect the NTP validation and clock-anchor logs. Uptime is diagnostic metadata and must not be interpreted as Unix time.

## Automated checks

GitHub Actions runs four independent checks on pull requests and pushes to `main`:

- `cargo fmt --check`
- host library tests on `x86_64-unknown-linux-gnu`
- host library Clippy with warnings denied
- a release cross-build for `thumbv8m.main-none-eabihf` using placeholder configuration

These checks validate formatting, host-testable logic, linting, and compilation. They cannot prove that USB, Wi-Fi, DHCP, I2C, the physical SHT40, NTP reachability, or the MQTT broker work on real hardware; use the acceptance checks above for those behaviors.

## Security limitations

This is a LAN learning project, not a hardened production design:

- MQTT uses plaintext TCP. Network observers can read payloads and credentials, and the Pico does not authenticate the broker with TLS.
- NTP is unauthenticated. A network attacker could spoof time and cause incorrect measurement timestamps.
- Wi-Fi and optional MQTT credentials are compiled into the firmware. Ignoring `.env` prevents an ordinary source-control leak but does not protect secrets extracted from the firmware binary or physical device.
- The public RP2350 hardware ID enables stable device correlation and must not be treated as authentication material.
- QoS 1 provides at-least-once transport, so consumers must tolerate duplicates.

A production design should evaluate MQTT over TLS with broker certificate validation, protected credential provisioning/storage, authenticated time, and an explicit offline-delivery policy.

## Learning recap

- **Ownership and borrowing:** drivers, sockets, buffers, and protocol sessions have clear owners; borrowed buffers cannot be reused while an async operation still depends on them.
- **Lifetimes:** the encoded JSON slice is tied to the caller-provided buffer, preventing it from outliving that storage.
- **Traits:** the queue is generic over `embedded-storage` NOR-flash traits, so the same ordering and recovery logic runs against host fake flash and the Pico driver.
- **`Result` and `Option`:** expected configuration, conversion, encoding, sensor, DNS, and protocol failures are handled explicitly instead of panicking.
- **Async tasks:** the Embassy executor allows USB logging, the radio, networking, timers, and application work to make progress cooperatively without OS threads.
- **Static memory:** fixed-size sensor, JSON, TCP, and MQTT buffers make memory use predictable and avoid garbage collection or an allocator.
- **I2C:** the RP2350 communicates with the SHT40 over the GP0/GP1 I2C bus and validates sensor responses.
- **UDP and NTP:** a validated NTP response anchors Unix time to a monotonic `Instant`; later timestamps advance from that known point.
- **TCP:** TCP supplies MQTT's ordered byte transport but does not understand MQTT topics, publications, sessions, or keepalive.
- **JSON:** a typed `Reading` is serialized into a reusable caller-owned buffer, with an explicit error when that buffer is too small.
- **MQTT:** the client maintains a protocol session over TCP, services keepalive traffic between samples, publishes queued readings with QoS 1, and retires flash records only after PUBACK.
