#![no_std]
#![no_main]

use cyw43::aligned_bytes;
use cyw43_pio::{DEFAULT_CLOCK_DIVIDER, PioSpi};
use embassy_executor::Spawner;
use embassy_rp::bind_interrupts;
use embassy_rp::dma;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::peripherals::{DMA_CH0, PIO0, USB};
use embassy_rp::pio::{InterruptHandler as PioInterruptHandler, Pio};
use embassy_rp::usb::{Driver, InterruptHandler as UsbInterruptHandler};
use embassy_time::{Instant, Timer};
use panic_halt as _;
use static_cell::StaticCell;

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => UsbInterruptHandler<USB>;
    PIO0_IRQ_0 => PioInterruptHandler<PIO0>;
    DMA_IRQ_0 => dma::InterruptHandler<DMA_CH0>;
});

type UsbDriver = Driver<'static, USB>;

struct AppConfig {
    wifi_ssid: &'static str,
    wifi_password: &'static str,
    mqtt_host: &'static str,
    mqtt_port: u16,
    mqtt_topic: &'static str,
    mqtt_client_id: &'static str,
    mqtt_username: Option<&'static str>,
    mqtt_password: Option<&'static str>,
}

#[derive(Debug)]
enum ConfigError {
    MissingWifiSsid,
    MissingWifiPassword,
    MissingMqttHost,
    InvalidMqttPort,
    IncompleteMqttCredentials,
}

impl AppConfig {
    fn load() -> Result<Self, ConfigError> {
        let wifi_ssid = option_env!("WIFI_SSID").ok_or(ConfigError::MissingWifiSsid)?;
        let wifi_password = option_env!("WIFI_PASSWORD").ok_or(ConfigError::MissingWifiPassword)?;
        let mqtt_host = option_env!("MQTT_HOST").ok_or(ConfigError::MissingMqttHost)?;
        let mqtt_port = match option_env!("MQTT_PORT") {
            Some(value) => match value.parse::<u16>() {
                Ok(port) if port != 0 => port,
                _ => return Err(ConfigError::InvalidMqttPort),
            },
            None => 1883,
        };
        let mqtt_topic = option_env!("MQTT_TOPIC").unwrap_or("pico-data-logger/readings");
        let mqtt_client_id = option_env!("MQTT_CLIENT_ID").unwrap_or("pico-data-logger");
        let mqtt_username = option_env!("MQTT_USERNAME");
        let mqtt_password = option_env!("MQTT_PASSWORD");

        match (mqtt_username, mqtt_password) {
            (None, None) | (Some(_), Some(_)) => {}
            _ => return Err(ConfigError::IncompleteMqttCredentials),
        }

        Ok(Self {
            wifi_ssid,
            wifi_password,
            mqtt_host,
            mqtt_port,
            mqtt_topic,
            mqtt_client_id,
            mqtt_username,
            mqtt_password,
        })
    }
}

#[embassy_executor::task]
async fn logger_task(driver: UsbDriver) -> ! {
    embassy_usb_logger::run!(1024, log::LevelFilter::Info, driver);
}

#[embassy_executor::task]
async fn cyw43_task(
    runner: cyw43::Runner<'static, cyw43::SpiBus<Output<'static>, PioSpi<'static, PIO0, 0>>>,
) -> ! {
    runner.run().await;
}

#[embassy_executor::main(
    executor = "embassy_rp::executor::Executor",
    entry = "cortex_m_rt::entry"
)]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    let driver = Driver::new(p.USB, Irqs);

    spawner.spawn(logger_task(driver).expect("Failed to start logger task"));

    let fw = aligned_bytes!("../firmware/43439A0.bin");
    let clm = aligned_bytes!("../firmware/43439A0_clm.bin");
    let nvram = aligned_bytes!("../firmware/nvram_rp2040.bin");

    let pwr = Output::new(p.PIN_23, Level::Low);
    let cs = Output::new(p.PIN_25, Level::High);

    let mut pio = Pio::new(p.PIO0, Irqs);

    let spi = PioSpi::new(
        &mut pio.common,
        pio.sm0,
        DEFAULT_CLOCK_DIVIDER,
        pio.irq0,
        cs,
        p.PIN_24,
        p.PIN_29,
        dma::Channel::new(p.DMA_CH0, Irqs),
    );

    static CYW43_STATE: StaticCell<cyw43::State> = StaticCell::new();

    let state = CYW43_STATE.init(cyw43::State::new());

    let (_net_device, mut control, runner) = cyw43::new(state, pwr, spi, fw, nvram).await;

    spawner.spawn(cyw43_task(runner).expect("Failed to start CYW43 runner task"));

    control.init(clm).await;

    control
        .set_power_management(cyw43::PowerManagementMode::PowerSave)
        .await;

    log::info!("radio ready: CYW43439 initialized in PowerSave mode");

    let started_at = Instant::now();

    Timer::after_secs(2).await;

    log::info!("Pico Data Logger v{} starting", env!("CARGO_PKG_VERSION"));

    let config = match AppConfig::load() {
        Ok(config) => config,
        Err(error) => {
            log::error!("invalid application configuration: {:?}", error);

            loop {
                Timer::after_secs(60).await;
            }
        }
    };

    loop {
        let mut join_options = cyw43::JoinOptions::new(config.wifi_password.as_bytes());
        join_options.auth = cyw43::JoinAuth::Wpa2;

        log::info!("joining WiFi network: {}", config.wifi_ssid);

        match control.join(config.wifi_ssid, join_options).await {
            Ok(()) => {
                log::info!("joined Wi-Fi network: {}", config.wifi_ssid);
                break;
            }
            Err(error) => {
                log::error!(
                    "join failed for Wi-Fi network {}: {:?}",
                    config.wifi_ssid,
                    error
                );
                log::info!("retrying Wi-Fi join in 5 seconds");
                Timer::after_secs(5).await;
            }
        }
    }

    loop {
        log::info!("uptime: {} seconds; LED on", started_at.elapsed().as_secs());
        control.gpio_set(0, true).await;
        Timer::after_secs(1).await;

        log::info!(
            "uptime: {} seconds; LED off",
            started_at.elapsed().as_secs()
        );
        control.gpio_set(0, false).await;
        Timer::after_secs(1).await;
    }
}
