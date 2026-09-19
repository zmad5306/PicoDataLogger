#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_rp::bind_interrupts;
use embassy_rp::peripherals::USB;
use embassy_rp::usb::{Driver, InterruptHandler};
use embassy_time::{Instant, Timer};
use panic_halt as _;

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => InterruptHandler<USB>;
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

#[embassy_executor::main(
    executor = "embassy_rp::executor::Executor",
    entry = "cortex_m_rt::entry"
)]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    let driver = Driver::new(p.USB, Irqs);

    spawner.spawn(logger_task(driver).expect("Failed to start logger task"));

    let started_at = Instant::now();

    Timer::after_secs(2).await;

    log::info!("Pico Data Logger v{} starting", env!("CARGO_PKG_VERSION"));

    let _config = match AppConfig::load() {
        Ok(config) => config,
        Err(error) => {
            log::error!("invalid application configuration: {:?}", error);

            loop {
                Timer::after_secs(60).await;
            }
        }
    };

    loop {
        log::info!("uptime: {} seconds", started_at.elapsed().as_secs());
        Timer::after_secs(5).await;
    }
}
