//! Timed RP2350 clock-gated SLEEP, with an accurate crystal-clocked AON timer.
//!
//! Follows pico-extras sleep_goto_sleep_until: only POWMAN's reference clock
//! remains enabled in SLEEP. This is not DORMANT or a power-domain shutdown:
//! SRAM and the crystal stay powered. Both PLLs are stopped for the wait.
//! https://github.com/raspberrypi/pico-extras/blob/master/src/rp2_common/pico_sleep/sleep.c
use core::cell::Cell;
use cortex_m::peripheral::NVIC;
use embassy_rp::aon_timer::{AonTimer, Config};
use embassy_rp::{Peri, interrupt, pac, peripherals::POWMAN};
use embassy_sync::blocking_mutex::{Mutex, raw::CriticalSectionRawMutex};
use embassy_time::{Duration, Instant, Timer};

static SLEEP_OFFSET_US: Mutex<CriticalSectionRawMutex, Cell<u64>> = Mutex::new(Cell::new(0));

/// Application time includes intervals during which Embassy's TIMER0 stops.
/// Transport timeouts continue using Embassy's native monotonic clock.
pub fn now() -> Instant {
    SLEEP_OFFSET_US.lock(|offset| Instant::from_micros(Instant::now().as_micros() + offset.get()))
}

pub struct Sleeper<'d> {
    timer: AonTimer<'d>,
}

impl<'d> Sleeper<'d> {
    pub fn new(
        powman: Peri<'d, POWMAN>,
        irq: impl interrupt::typelevel::Binding<
            interrupt::typelevel::POWMAN_IRQ_TIMER,
            embassy_rp::aon_timer::InterruptHandler,
        > + 'd,
    ) -> Self {
        // Default is XOSC, 12 MHz, interrupt wake. Keeping the crystal running
        // avoids LPOSC drift in timestamps and the 15-minute schedule.
        let mut timer = AonTimer::new(powman, irq, Config::default());
        timer.stop();
        timer.set_counter(0);
        timer.start();
        Self { timer }
    }

    pub async fn wait_until(&mut self, deadline: Instant, battery: bool) {
        let Some(remaining) = deadline.checked_duration_since(now()) else {
            return;
        };
        if !battery {
            // Keep USB diagnostics responsive on USB power or unknown sensing.
            Timer::after(remaining).await;
            return;
        }
        // Let the logger flush before clocks/interrupts are suspended.
        Timer::after_millis(20).await;
        let Some(remaining) = deadline.checked_duration_since(now()) else {
            return;
        };
        if remaining < Duration::from_millis(2) {
            return;
        }
        let aon_before = self.timer.now();
        let native_before = Instant::now();
        if self
            .timer
            .set_alarm(aon_before + remaining.as_millis())
            .is_err()
        {
            return;
        }

        cortex_m::interrupt::free(|_| {
            // SAFETY: single-core firmware, no other task executes in this
            // synchronous critical section. Registers are restored before
            // interrupts or the executor can run again. The radio/network
            // scopes have already been dropped; no DMA is in flight.
            let mut core = unsafe { cortex_m::Peripherals::steal() };
            let enabled = [core.NVIC.iser[0].read(), core.NVIC.iser[1].read()];
            unsafe {
                core.NVIC.icer[0].write(u32::MAX);
                core.NVIC.icer[1].write(u32::MAX);
                NVIC::unmask(interrupt::POWMAN_IRQ_TIMER);
            }
            let clocks = pac::CLOCKS;
            let usb_sie = pac::USB.sie_ctrl().read();
            pac::USB.sie_ctrl().modify(|w| w.set_transceiver_pd(true));
            let sys = clocks.clk_sys_ctrl().read();
            let sleep0 = clocks.sleep_en0().read();
            let sleep1 = clocks.sleep_en1().read();
            let usb = clocks.clk_usb_ctrl().read();
            let adc = clocks.clk_adc_ctrl().read();
            let peri = clocks.clk_peri_ctrl().read();
            let hstx = clocks.clk_hstx_ctrl().read();
            let pll_sys = pac::PLL_SYS.pwr().read();
            let pll_usb = pac::PLL_USB.pwr().read();
            // Default clk_ref is the 12 MHz crystal. Switch the CPU away from
            // PLL_SYS before stopping either PLL.
            clocks
                .clk_sys_ctrl()
                .modify(|w| w.set_src(pac::clocks::vals::ClkSysCtrlSrc::CLK_REF));
            while clocks.clk_sys_selected().read().0 != 1 {}
            clocks.clk_usb_ctrl().modify(|w| w.set_enable(false));
            clocks.clk_adc_ctrl().modify(|w| w.set_enable(false));
            clocks.clk_peri_ctrl().modify(|w| w.set_enable(false));
            clocks.clk_hstx_ctrl().modify(|w| w.set_enable(false));
            pac::PLL_SYS.pwr().modify(|w| {
                w.set_pd(true);
                w.set_vcopd(true);
            });
            pac::PLL_USB.pwr().modify(|w| {
                w.set_pd(true);
                w.set_vcopd(true);
            });
            clocks.sleep_en0().write(|w| w.set_clk_ref_powman(true));
            clocks
                .sleep_en1()
                .write_value(pac::clocks::regs::SleepEn1(0));
            let was_deep = core.SCB.scr.read() & (1 << 2) != 0;
            core.SCB.set_sleepdeep();
            // PRIMASK prevents handlers running with altered clocks. An enabled
            // pending AON interrupt still wakes WFI. Other IRQs are masked in
            // NVIC so USB/timer traffic cannot cause repeated premature wakes.
            while !self.timer.alarm_fired() {
                cortex_m::asm::dsb();
                cortex_m::asm::wfi();
            }
            if !was_deep {
                core.SCB.clear_sleepdeep();
            }
            clocks.sleep_en0().write_value(sleep0);
            clocks.sleep_en1().write_value(sleep1);
            pac::PLL_SYS.pwr().write_value(pll_sys);
            pac::PLL_USB.pwr().write_value(pll_usb);
            while !pac::PLL_SYS.cs().read().lock() {}
            while !pac::PLL_USB.cs().read().lock() {}
            clocks.clk_sys_ctrl().write_value(sys);
            while clocks.clk_sys_selected().read().0 != (1 << sys.src() as u8) {}
            clocks.clk_usb_ctrl().write_value(usb);
            clocks.clk_adc_ctrl().write_value(adc);
            clocks.clk_peri_ctrl().write_value(peri);
            clocks.clk_hstx_ctrl().write_value(hstx);
            pac::USB.sie_ctrl().write_value(usb_sie);
            self.timer.disable_alarm();
            self.timer.disable_alarm_interrupt();
            self.timer.clear_alarm();
            NVIC::unpend(interrupt::POWMAN_IRQ_TIMER);
            unsafe {
                core.NVIC.icer[0].write(u32::MAX);
                core.NVIC.icer[1].write(u32::MAX);
                core.NVIC.iser[0].write(enabled[0]);
                core.NVIC.iser[1].write(enabled[1]);
            }
            // Avoid counting the short entry/exit time twice. The AON timer
            // continues throughout sleep; TIMER0 counts only awake time.
            let elapsed_ms = self.timer.now() - aon_before;
            let native_us = native_before.elapsed().as_micros();
            SLEEP_OFFSET_US.lock(|offset| {
                offset.set(
                    offset.get()
                        + pico_data_logger::schedule::sleep_correction_us(elapsed_ms, native_us),
                )
            });
        });
        log::info!("woke from clock-gated sleep; uptime={}s", now().as_secs());
    }
}
