# Battery cycle: implementation and validation

The logger now starts a wake cycle every 15 minutes and powers down the radio between cycles. Each awake cycle has a 60-second async budget, including radio initialization and network recovery. No wiring changes are required: keep the SHT40 on 3V3, GND, GP0/SDA, and GP1/SCL.

## Reading the code later

1. Start at `main` in [main.rs](../src/main.rs). The sensor, flash queue, and optional UTC anchor live outside the cycle loop. The loop builds the radio and network inside a temporary async scope. `with_timeout` bounds that entire scope, not just MQTT publishing.
2. Read `upload_cycle`. Once a UTC anchor exists, it measures and saves before connecting to Wi-Fi. Thus a Wi-Fi or broker outage preserves the scheduled sample. A cold boot must obtain NTP first; it skips samples until time is known. Radio initialization failure may also prevent a sample.
3. Read `upload_attempt` and `drain_queue`. Every connection attempt gets new MQTT and TCP state. Records remain in flash until PUBACK. A timeout drops the whole scope, so cancelled protocol state is never reused. Large backlogs may take multiple wake cycles to drain.
4. Return to `main`: after the scope is dropped, GP23 is driven low. Dropping the DMA transfer aborts it; dropping PIO drivers disables state machines and releases pins. Peripheral reborrows prevent a new driver from being constructed while the old owner exists. [voltage.rs](../src/voltage.rs) now accepts that shorter lifetime and clears its signals each cycle.
5. Read [schedule.rs](../src/schedule.rs). Scheduling advances from the previous cycle's deadline, not from upload completion. Missed slots are skipped instead of causing rapid catch-up samples. First-boot network setup can delay the first capture within a slot.
6. Read [power.rs](../src/power.rs). On confirmed battery power, it arms the crystal-clocked AON timer, masks other interrupts, switches the CPU off its PLL, stops both PLLs and the USB PHY, and enters clock-gated SLEEP. It restores clocks, PHY, and interrupt masks before tasks resume. SRAM and the oscillators remain powered. This is SLEEP, not DORMANT or full power-domain shutdown.

There are two clocks deliberately. Embassy's native clock handles async network timeouts while awake. `power::now()` adds sleep measured by AON, subtracting time the native clock already counted on entry/exit. Application scheduling, UTC anchors, refresh deadlines, and uptime use the adjusted clock. Unit tests cover the accounting, but cannot prove the chip actually sleeps or wakes.

On USB power or unknown power-source sensing, the radio still powers down but the firmware uses an async timer wait. That preserves USB diagnostics. Connecting USB during battery sleep will not immediately wake the CPU: wait for the next cycle. Disconnect a directly connected battery pack before applying USB unless reverse-current protection is installed.

The SHT40 remains powered, including its breakout LED. Cold boot resets uptime and loses the in-memory UTC anchor; sleep does neither. The flash queue preserves readings through either event. Its 1,008-record capacity now covers 10 days and 12 hours at 15-minute intervals.

## Hardware acceptance

These checks are pending until performed on the physical Pico. Build and host-test success are not hardware acceptance.

1. **USB baseline:** deploy through the normal script (which loads `.env`), watch the serial log, and confirm `upload cycle complete` followed by a cycle-ended message. Verify two successive readings approximately 900 seconds apart. This tests network reconstruction, not clock-gated sleep.
2. **Battery sleep/wake:** with USB disconnected, observe at least three successive MQTT readings over roughly 30 minutes after the first reading. Confirm `on_battery: true`, increasing sequences, and roughly 900-second increments in both capture timestamps and uptime. No uptime reset is expected. The Pico LED should be off between cycles.
3. **Measure current:** measure in series with the battery supply, recording awake peaks, sleep current, and energy/current over a full 15-minute cycle. Use a suitable range that will tolerate Wi-Fi startup peaks without excessive meter voltage drop. Do not connect a current-range meter across the battery. Compare against the old firmware under the same conditions; no specific current or runtime is guaranteed by this change.
4. **Broker outage:** after successful NTP synchronization, block the broker for two or more cycles. Confirm each awake window ends within approximately 60 seconds and sleep current returns. Restore the broker and verify replay of original timestamps and sequence numbers. PUBACK-loss replay may duplicate a sequence, but must not silently discard it.
5. **Wi-Fi outage:** repeat with the access point unavailable. Once an anchor exists, readings should accumulate and replay on recovery. Restoring service may take until the next scheduled wake, up to 15 minutes.
6. **Cold boot without NTP:** verify no fabricated UTC readings, bounded awake windows, and a successful first reading after NTP returns. Records captured before the reboot remain queued, but new samples are skipped until the new boot has an anchor.
7. **Longer run:** observe multiple days, including a daily NTP refresh, to check drift, repeated driver initialization, battery voltage against a multimeter, and cumulative uptime.

If a battery wake fails, use BOOTSEL to restore known-good firmware and capture the last logs/readings. USB diagnostics alone cannot validate battery sleep because they intentionally use the ordinary wait path.

## Upstream references

- [Raspberry Pi clock-gated sleep implementation](https://github.com/raspberrypi/pico-extras/blob/master/src/rp2_common/pico_sleep/sleep.c): `sleep_goto_sleep_until` keeps the POWMAN reference clock enabled and enters processor deep sleep.
- [Pico SDK low-power implementation](https://github.com/raspberrypi/pico-sdk/blob/master/src/rp2_common/pico_low_power/low_power.c): timed AON wake and exclusion of other interrupt sources.
- The installed `embassy-rp` 0.10 AON, DMA, PIO, and ADC sources were checked for timer wake semantics and driver drop behavior. The generic oscillator-dormant helper is not used for this implementation.
