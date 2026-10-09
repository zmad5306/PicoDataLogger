# Pico Data Logger

Bare-metal Rust firmware that reads temperature and humidity from an SHT40, assigns synchronized UTC timestamps, and publishes JSON readings over MQTT from a Raspberry Pi Pico 2 W.

The firmware takes a reading on a 15-minute schedule. Each wake cycle has a 60-second budget for radio initialization, sampling, connecting, and uploading. It saves readings to flash before uploading, powers down the radio after each cycle, and uses clock-gated RP2350 SLEEP on battery power. USB-powered operation keeps diagnostics responsive between cycles.

Project documentation:

- This README is the build, deployment, operation, troubleshooting, and acceptance-test runbook.
- [Architecture](docs/architecture.md) explains the async tasks, hardware interfaces, protocol layers, fixed-memory model, clock anchor, and bounded upload cycles.
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

The CYW43439 uses Embassy's `PowerSave` mode while awake and is held in reset/power-down through GP23 between cycles. On confirmed battery power, both PLLs and peripheral clocks are stopped/gated during RP2350 SLEEP; a crystal-clocked always-on timer wakes the processor. SRAM and the crystal remain powered: this is clock-gated SLEEP, not DORMANT or complete power-domain shutdown. The SHT40 remains on the documented 3V3 rail, including its power LED.

On USB power, or if VBUS sensing fails, the radio still powers down but the CPU uses an ordinary async wait to preserve USB diagnostics. Connecting USB during battery sleep does not immediately wake the firmware; allow the next scheduled wake. Actual current and repeated wakeup reliability require hardware verification. See [Battery cycle implementation and validation](docs/battery-cycle.md).

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

The Windows script identifies the running firmware by its USB VID/PID instead of choosing the first COM port. If more than one Pico running this firmware is connected, select the intended port explicitly:

```powershell
./scripts/deploy.ps1 -SerialPort COM3
```

### Monitor without flashing

To attach to a Pico that is already running, use the standalone monitor for your development machine.

On macOS with Bash:

```sh
bash ./scripts/monitor-macos.sh
```

On Windows with PowerShell 7:

```powershell
./scripts/monitor.ps1
```

Both scripts require exactly one matching device unless a serial device is supplied explicitly:

```sh
bash ./scripts/monitor-macos.sh /dev/cu.usbmodem101
```

```powershell
./scripts/monitor.ps1 -SerialPort COM3
```

The macOS monitor uses `screen`; exit it with `Ctrl-A`, then `K`, then `Y`. Stop the PowerShell monitor with `Ctrl-C`.

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

After accepting the complete UF2, the ROM bootloader writes the application to external flash and automatically reboots. `/Volumes/RP2350` should disappear within a few seconds. Each wake initializes the CYW43439; the onboard LED indicates sampling and connection activity during that cycle, then goes out when the radio powers down.

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

USB logging runs in a separate Embassy task. During each wake cycle, the application, CYW43439 runner, and network runner are polled together within one cancellable scope. Its 60-second deadline includes radio initialization and network recovery. All network drivers are dropped before radio power-down. See the [architecture document](docs/architecture.md) for ownership and recovery details.

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

At boot the firmware starts USB logging, opens the sensor, reads the hardware ID, and recovers the flash queue. It then repeats this cycle on a 15-minute schedule:

1. Initialize the radio and fresh DHCP/network state, and sense whether USB power is present.
2. If a UTC anchor already exists, capture and persist a reading before attempting Wi-Fi, so ordinary network outages do not prevent sampling.
3. Join Wi-Fi and obtain DHCP. On the first successful boot synchronization, obtain UTC with NTP before capturing the first reading. Until then, skip samples rather than invent timestamps.
4. Refresh an existing UTC anchor when due (daily), allowing at most 10 seconds of the cycle for refresh; retain the old anchor on failure.
5. Connect to MQTT and publish queued records oldest-first with QoS 1. Retire each only after PUBACK.
6. After success or the 60-second awake deadline, drop all network futures/drivers and power down the radio. Sleep until the next scheduled cycle.

The schedule is measured from cycle starts, so upload time does not add another 15 minutes. Missed slots are skipped rather than sampled in a burst. Radio initialization and first-boot NTP can delay the actual capture within a slot. An initial NTP outage produces no samples until a valid anchor exists. After an ordinary reboot that anchor must be obtained again; already queued records retain their original timestamps.

Application UTC and `uptime_s` include sleep duration measured by the always-on timer. Network timeouts use Embassy's native awake-time clock. USB-powered cycles retain the same sampling schedule and radio power-down behavior, using an async wait instead of clock-gated sleep.

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
{"device_id":"basement-sensor","hardware_id":"0123456789abcdef","sequence":42,"temperature_c":21.34,"relative_humidity_pct":43.13,"vsys_voltage_v":2.85,"on_battery":true,"timestamp_unix_s":1789916205,"uptime_s":16}
```

| Field | Meaning |
| --- | --- |
| `device_id` | Human-readable logical identity from `MQTT_CLIENT_ID`, such as `home-office`. |
| `hardware_id` | Stable 16-digit hexadecimal RP2350 chip ID read from OTP memory. It is an identifier, not a secret. |
| `sequence` | Monotonically increasing record identifier; repeated values identify at-least-once replay after a reset. |
| `temperature_c` | SHT40 temperature in degrees Celsius. |
| `relative_humidity_pct` | SHT40 relative humidity percentage. |
| `vsys_voltage_v` | Estimated VSYS voltage in volts, captured with the reading; `null` if sampling fails or the record predates voltage monitoring. |
| `on_battery` | Boolean: `true` when USB VBUS is absent, `false` when present; `null` for unavailable sensing or older records. |
| `timestamp_unix_s` | UTC Unix timestamp captured with the measurement. |
| `uptime_s` | Whole seconds since this firmware booted. |

The MQTT connection client ID combines both identities as `<device_id>-<hardware_id>`, preventing two physical Picos with the same human-readable configuration from disconnecting each other. Publications use MQTT QoS 1 and are not retained. The firmware keeps each flash record until the broker acknowledges its publication. If power fails after broker delivery but before local retirement, that record is sent again after reboot; consumers should use `(hardware_id, sequence)` to recognize the duplicate. `device_id` remains convenient for human-facing grouping and can intentionally survive hardware replacement.

### Supply-voltage telemetry

`on_battery` reads the Pico 2 W's CYW43439 `WL_GPIO2` VBUS-sense input through `Control::gpio_get`, then inverts it. USB power present means `false`; USB power absent means `true`. This describes the USB-or-AA wiring used here: the board cannot identify whether a non-USB VSYS supply is a battery or another external supply, or directly measure which source supplies current in an arbitrary dual-supply circuit. Sensing does not depend on USB enumeration, a serial monitor, or a voltage threshold. A timed-out or short GPIO response produces `null`.

For server-side battery alerts, require `on_battery == true`, a non-null `vsys_voltage_v`, and a recent capture timestamp.

Every reading includes `vsys_voltage_v` for server-side low-battery alerts. The Pico 2 W measures VSYS/3 on ADC3/GPIO29, which also carries the CYW43439 SPI clock. The radio bus wrapper services voltage requests between completed SPI transactions, with chip select high. It temporarily disables the pin's digital pad, lets it settle, discards one ADC conversion, averages 16 conversions, and restores the original pad before resuming radio communication. A one-second request timeout produces `null` rather than a stale reading. This feature does not shut down the device or apply a local low-battery threshold.

Conversion assumes a nominal 3.3 V ADC reference and the board's 3:1 divider; the result is rounded to millivolts but is not calibrated to millivolt accuracy. Compare telemetry against a multimeter before choosing alert thresholds. With a battery directly on VSYS and USB disconnected, this estimates pack voltage. With USB powering the board, it measures the USB-derived VSYS rail; with a blocking diode, it measures voltage after that diode. See the [Pico 2 W datasheet, sections 2.1 and 3.4–3.5](https://datasheets.raspberrypi.com/picow/pico-2-w-datasheet.pdf).

Voltage and `on_battery` are persisted at capture time, so delayed MQTT replay retains the original values. New records use version 3. Versions 1 and 2 remain readable and publish `on_battery: null`; version 1 also publishes voltage as `null`. Server alerts should ignore null values and use `timestamp_unix_s` to avoid treating old replayed measurements as current battery status. A separate last-seen alert can detect a device that stops reporting altogether.

Hardware acceptance: observe MQTT while running from batteries, compare `vsys_voltage_v` with a meter across VSYS/GND, and confirm publishing continues with wireless activity. Then interrupt broker access, collect readings, reboot, restore access, and confirm replay preserves their voltages and power-source flags. Verify new readings show `on_battery: true` on batteries and `false` on powered USB, including a USB charger without a serial connection. When switching to USB for diagnostics, disconnect a directly wired battery pack first unless the supply has the documented reverse-current protection. Compilation and host tests do not verify ADC accuracy or radio coexistence on the physical board.

### Offline queue capacity and wear

The final 256 KiB of onboard flash is reserved for measurements and excluded from the firmware link region. Records occupy one 256-byte flash page and include a format version, sequence number, original Unix timestamp, temperature, humidity, uptime, VSYS voltage, power-source flag, and CRC-32 integrity check. One 4-KiB erase sector is retained as circular working space, leaving 1,008 usable records—10 days and 12 hours at one reading every 15 minutes.

Writes are append-only. A record becomes visible only after its body and checksum have been written and a final commit byte is programmed. Recovery ignores incomplete or corrupt records and reconstructs FIFO order from sequence numbers. Acknowledgment and commit markers only clear flash bits; sectors are erased in 16-record batches instead of once per measurement. When all 1,008 positions are occupied, the oldest measurement is explicitly discarded so newer outage data continues to be captured.

## Recovery behavior

Recovery runs only within the current 60-second awake budget. MQTT attempts use increasing retry delays, fresh sessions, and fresh sockets. Wi-Fi joins retain their 15-second timeout and DHCP its 30-second timeout, all subordinate to the overall deadline.

- A failed sensor read skips this cycle's sample; queued records can still upload.
- A Wi-Fi or broker outage leaves the captured record in flash. The next attempt is in a later wake cycle if the current budget expires.
- A timed-out QoS 1 publication remains queued unless PUBACK has already been processed and the record retired. Power loss between broker receipt and retirement may cause duplicate replay.
- A failed NTP refresh retains the existing anchor. A cold boot without NTP cannot timestamp new samples and retries next cycle.
- Radio initialization or sensor/driver stalls are also covered by the async deadline, provided the driver yields. A synchronous hardware hang or panic is not a watchdog-protected failure.

The logger remains available throughout awake work and USB-powered waits. It cannot run during battery sleep.

### Onboard status LED

The Pico 2 W onboard LED is wired to the CYW43439 radio module's `WL_GPIO0`, not to an RP2350 GPIO such as GP25. The firmware therefore drives it through the same initialized CYW43439 control path used for Wi-Fi:

| Pattern | Meaning |
| --- | --- |
| Off | Idle, or radio powered down between cycles (including after a failed cycle) |
| Solid on | Sampling or publishing |
| Flashing | Wi-Fi/NTP recovery while awake; other retries may show a steady fault phase |

The LED cannot signal an outage while the radio is powered down. Use recent MQTT capture timestamps and a server-side last-seen alert. PUBACK confirms broker receipt, not subscriber processing.

## Recovery troubleshooting and acceptance checks

Use USB power and keep the serial monitor and MQTT subscriber visible for these recovery checks. Do not reset except where explicitly requested. USB checks do not verify battery sleep; follow the separate [battery acceptance procedure](docs/battery-cycle.md#hardware-acceptance).

### Sensor interruption

1. Confirm readings are arriving approximately every 15 minutes; the LED is off while waiting, solid during a sample, and off again after its publication is acknowledged.
2. Disconnect the SHT40, wait for a sample, and confirm an I2C failure is logged without a reboot.
3. Reconnect the sensor using 3V3, GND, GP0/SDA, and GP1/SCL.
4. Confirm a later sample is published, the LED returns to off after PUBACK, and `uptime_s` continues increasing.

### Broker outage

1. Subscribe to the readings topic and note the latest `sequence` and `timestamp_unix_s`.
2. Block broker access for at least three 15-minute sample intervals and confirm serial queue depth grows across wake cycles.
3. Power-cycle the Pico while broker access remains blocked; confirm recovery logs the preserved queue depth.
4. Restore broker access and observe the queued timestamps arrive in ascending sequence order before normal live publishing resumes; confirm the LED returns to off after the backlog is acknowledged.
5. Confirm any duplicate carries the same sequence number and that recovery requires no manual reset.

The MQTT connection intentionally closes between cycles. A failure to send a graceful DISCONNECT may leave the old broker session visible until its keepalive expires; it is not maintained during sleep.

The USB log distinguishes `publish submission timed out`, `PUBACK timed out`, awake-budget exhaustion, and TCP connection failures. Publish-timeout messages include the affected sequence number and current queue depth; the record remains queued for replay after reconnection.

### Wi-Fi interruption

1. Interrupt the configured Wi-Fi network long enough for the Pico to detect link loss.
2. Restore the network.
3. At the next scheduled cycle, confirm Wi-Fi join, DHCP, broker DNS, TCP, and MQTT recover; UTC refresh occurs only when due.
4. Confirm publications resume and `uptime_s` did not restart.

### NTP unavailable at boot

1. Make the configured NTP service unreachable before booting the Pico.
2. Confirm the firmware reports that time remains unsynchronized and does not publish readings with uptime substituted for UTC.
3. Restore NTP reachability.
4. At the next scheduled cycle, confirm synchronization succeeds and MQTT publishing begins without resetting the Pico.

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

This firmware is intended for a trusted LAN and is not a hardened production design:

- MQTT uses plaintext TCP. Network observers can read payloads and credentials, and the Pico does not authenticate the broker with TLS.
- NTP is unauthenticated. A network attacker could spoof time and cause incorrect measurement timestamps.
- Wi-Fi and optional MQTT credentials are compiled into the firmware. Ignoring `.env` prevents an ordinary source-control leak but does not protect secrets extracted from the firmware binary or physical device.
- The public RP2350 hardware ID enables stable device correlation and must not be treated as authentication material.
- QoS 1 provides at-least-once transport, so consumers must tolerate duplicates.

A production design should evaluate MQTT over TLS with broker certificate validation, protected credential provisioning/storage, authenticated time, and an explicit offline-delivery policy.
