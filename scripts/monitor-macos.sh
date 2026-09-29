#!/usr/bin/env bash

set -euo pipefail

if ! command -v screen >/dev/null 2>&1; then
    printf '%s\n' "error: screen is not installed" >&2
    exit 1
fi

if [[ $# -gt 1 ]]; then
    printf 'usage: %s [/dev/cu.usbmodem...]\n' "$0" >&2
    exit 1
fi

if [[ $# -eq 1 ]]; then
    serial_device="$1"
    if [[ ! -e "$serial_device" ]]; then
        printf 'error: serial device does not exist: %s\n' "$serial_device" >&2
        exit 1
    fi
else
    shopt -s nullglob
    serial_devices=(/dev/cu.usbmodem*)
    shopt -u nullglob

    if [[ ${#serial_devices[@]} -eq 0 ]]; then
        printf '%s\n' "error: no /dev/cu.usbmodem* device was found" >&2
        printf '%s\n' "run: system_profiler SPUSBDataType" >&2
        exit 1
    fi
    if [[ ${#serial_devices[@]} -gt 1 ]]; then
        printf 'error: multiple USB serial devices were found: %s\n' "${serial_devices[*]}" >&2
        printf 'rerun with the intended device, for example: %s %s\n' "$0" "${serial_devices[0]}" >&2
        exit 1
    fi
    serial_device="${serial_devices[0]}"
fi

printf 'Starting serial monitor on %s. Exit with Ctrl-A, then K, then Y.\n' "$serial_device"
exec screen "$serial_device" 115200
