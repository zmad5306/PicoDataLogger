#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
REPO_ROOT="$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)"
MOUNT_POINT="${1:-/Volumes/RP2350}"
ELF_PATH="$REPO_ROOT/target/thumbv8m.main-none-eabihf/release/pico-data-logger"
UF2_PATH="$REPO_ROOT/target/pico-data-logger.uf2"

cd "$REPO_ROOT"

if ! command -v cargo >/dev/null 2>&1; then
    printf '%s\n' "error: cargo is not installed or is not on PATH" >&2
    exit 1
fi

if ! command -v elf2uf2-rs >/dev/null 2>&1; then
    printf '%s\n' "error: elf2uf2-rs is not installed" >&2
    printf '%s\n' "see the README prerequisites for the RP2350-capable install command" >&2
    exit 1
fi

if ! elf2uf2-rs convert --help 2>&1 | grep -q 'rp2350-arm-s'; then
    printf '%s\n' "error: this elf2uf2-rs does not support the RP2350 UF2 family" >&2
    printf '%s\n' "reinstall the RP2350-capable revision documented in README.md" >&2
    exit 1
fi

printf '%s\n' "Building release firmware..."
cargo build --release

printf '%s\n' "Converting ELF to an RP2350 Arm Secure UF2..."
elf2uf2-rs convert --family rp2350-arm-s "$ELF_PATH" "$UF2_PATH"

if [[ ! -d "$MOUNT_POINT" ]]; then
    printf '%s\n' "error: BOOTSEL volume is not mounted at $MOUNT_POINT" >&2
    printf '%s\n' "unplug the Pico, hold BOOTSEL while reconnecting it, then rerun this script" >&2
    exit 1
fi

printf 'Copying firmware to %s...\n' "$MOUNT_POINT"
cp "$UF2_PATH" "$MOUNT_POINT/"
sync

printf '%s\n' "Waiting for the BOOTSEL volume to disappear..."
for _ in {1..15}; do
    if [[ ! -d "$MOUNT_POINT" ]]; then
        break
    fi
    sleep 1
done

if [[ -d "$MOUNT_POINT" ]]; then
    printf '%s\n' "error: $MOUNT_POINT is still mounted; the Pico did not accept or reboot from the UF2" >&2
    exit 1
fi

printf '%s\n' "Firmware copied and the Pico rebooted. Waiting for USB serial..."
for _ in {1..10}; do
    serial_devices=(/dev/cu.usbmodem*)
    if [[ -e "${serial_devices[0]}" ]]; then
        printf 'USB serial device: %s\n' "${serial_devices[0]}"
        if ! command -v screen >/dev/null 2>&1; then
            printf '%s\n' "warning: screen is not installed; firmware was flashed but monitoring cannot start" >&2
            exit 0
        fi
        printf '%s\n' "Starting serial monitor. Exit with Ctrl-A, then K, then Y."
        exec screen "${serial_devices[0]}" 115200
    fi
    sleep 1
done

printf '%s\n' "warning: firmware was flashed, but no /dev/cu.usbmodem* device appeared" >&2
printf '%s\n' "run: system_profiler SPUSBDataType" >&2
exit 2
