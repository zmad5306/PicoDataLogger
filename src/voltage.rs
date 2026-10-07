//! Pico 2 W VSYS sampling at CYW43 SPI transaction boundaries.
//!
//! GPIO29 is both the PIO clock and ADC3 (VSYS / 3). Only this bus wrapper
//! temporarily disables its digital pad, with CS high and no transfer active.
use cyw43::SpiBusCyw43;
use cyw43_pio::PioSpi;
use embassy_futures::select::{Either, select};
use embassy_rp::adc::{Adc, Blocking, Channel, Config};
use embassy_rp::gpio::Pull;
use embassy_rp::peripherals::{ADC, PIN_29, PIO0};
use embassy_rp::{Peri, pac};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, block_for, with_timeout};

static REQUEST: Signal<CriticalSectionRawMutex, ()> = Signal::new();
static RESPONSE: Signal<CriticalSectionRawMutex, Option<f32>> = Signal::new();

/// Called by the single application sampler. A failed read never reuses old voltage.
pub async fn read_voltage() -> Option<f32> {
    RESPONSE.reset();
    REQUEST.signal(());
    match with_timeout(Duration::from_secs(1), RESPONSE.wait()).await {
        Ok(voltage) => voltage,
        Err(_) => {
            REQUEST.reset();
            None
        }
    }
}

pub struct VoltageSpi {
    spi: PioSpi<'static, PIO0, 0>,
    adc: Adc<'static, Blocking>,
}

impl VoltageSpi {
    pub fn new(spi: PioSpi<'static, PIO0, 0>, adc: Peri<'static, ADC>) -> Self {
        Self {
            spi,
            adc: Adc::new_blocking(adc, Config::default()),
        }
    }

    fn sample(&mut self) {
        let pad = pac::PADS_BANK0.gpio(29);
        let saved_pad = pad.read();
        // SAFETY: the PioSpi owns GPIO29, but is exclusively borrowed here.
        // Its last command has completed (including DMA) and CS is high.
        // Channel disables pad output/input, preventing the autonomous PIO from
        // driving this pin. There is no await until the original pad is restored,
        // so cancellation cannot strand the SPI clock in analog mode. No other
        // task owns the ADC or accesses GPIO29. Do not move this into an app task.
        let mut channel = Channel::new_pin(unsafe { PIN_29::steal() }, Pull::None);
        block_for(Duration::from_micros(100));
        let voltage = (|| {
            // Discard the first conversion after switching the shared pin.
            self.adc.blocking_read(&mut channel).ok()?;
            let mut total = 0_u32;
            for _ in 0..16 {
                total += u32::from(self.adc.blocking_read(&mut channel).ok()?);
            }
            // Nominal 3.3 V ADC reference, 12-bit ADC, board's 3:1 divider.
            // Round to millivolts; this is an estimate, not calibrated accuracy.
            let millivolts = (total * 9900 + 32768) / 65536;
            Some(millivolts as f32 / 1000.0)
        })();
        drop(channel);
        pad.write_value(saved_pad);
        RESPONSE.signal(voltage);
    }

    fn service_request(&mut self) {
        if REQUEST.try_take().is_some() {
            self.sample();
        }
    }
}

impl SpiBusCyw43 for VoltageSpi {
    async fn cmd_write(&mut self, write: &[u32]) -> u32 {
        self.service_request();
        self.spi.cmd_write(write).await
    }

    async fn cmd_read(&mut self, command: u32, read: &mut [u32]) -> u32 {
        self.service_request();
        SpiBusCyw43::cmd_read(&mut self.spi, command, read).await
    }

    async fn wait_for_event(&mut self) {
        loop {
            match select(self.spi.wait_for_event(), REQUEST.wait()).await {
                Either::First(()) => return,
                Either::Second(()) => self.sample(),
            }
        }
    }
}
