# Pico Data Logger

A data logger built with:

- Raspberry Pi Pico 2 W
- Adafruit SHT40 temperature and humidity sensor (I2C)
- Qwiic-compatible cable

## Hardware wiring

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

Set the required values in PowerShell 7 before building or running the deployment script:

```powershell
$env:WIFI_SSID = "your-network-name"
$env:WIFI_PASSWORD = "your-network-password"
$env:MQTT_HOST = "broker.example.com"
```

The remaining settings are optional:

```powershell
$env:MQTT_PORT = "1883"
$env:MQTT_TOPIC = "pico-data-logger/readings"
$env:MQTT_CLIENT_ID = "pico-data-logger"
$env:MQTT_USERNAME = "your-mqtt-username"
$env:MQTT_PASSWORD = "your-mqtt-password"
```

In Bash or Zsh, export the same values before building or running the macOS deployment script:

```bash
export WIFI_SSID="your-network-name"
export WIFI_PASSWORD="your-network-password"
export MQTT_HOST="broker.example.com"

export MQTT_PORT="1883"
export MQTT_TOPIC="pico-data-logger/readings"
export MQTT_CLIENT_ID="pico-data-logger"
export MQTT_USERNAME="your-mqtt-username"
export MQTT_PASSWORD="your-mqtt-password"
```

For anonymous MQTT, ensure both optional credentials are absent:

```bash
unset MQTT_USERNAME MQTT_PASSWORD
```

`MQTT_USERNAME` and `MQTT_PASSWORD` must either both be set or both be absent. The optional settings use their documented defaults when omitted.

These values are read by `option_env!` during compilation. They are embedded in the resulting firmware binary, so compile-time configuration prevents accidental source-control commits but does not make credentials secret from someone who obtains the binary. Do not commit real credentials or generated firmware containing them.

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

After accepting the complete UF2, the ROM bootloader writes the application to external flash and automatically reboots. `/Volumes/RP2350` should disappear within a few seconds. Do not expect the Pico 2 W's onboard LED to light automatically; this firmware does not configure that LED.

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
