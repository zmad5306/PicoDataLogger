#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use panic_halt as _;

#[embassy_executor::main(
    executor = "embassy_rp::executor::Executor",
    entry = "cortex_m_rt::entry"
)]
async fn main(_spawner: Spawner) {
    let _peripherals = embassy_rp::init(Default::default());

    loop {
        Timer::after(Duration::from_secs(1)).await;
    }
}
