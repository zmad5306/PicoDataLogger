#![no_std]
#![no_main]

use cyw43::aligned_bytes;
use cyw43_pio::{DEFAULT_CLOCK_DIVIDER, PioSpi};
use embassy_executor::Spawner;
use embassy_rp::bind_interrupts;
use embassy_rp::dma;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::i2c::{Config as I2cConfig, I2c, InterruptHandler as I2cInterruptHandler};
use embassy_rp::peripherals::{DMA_CH0, I2C0, PIO0, USB};
use embassy_rp::pio::{InterruptHandler as PioInterruptHandler, Pio};
use embassy_rp::usb::{Driver, InterruptHandler as UsbInterruptHandler};
use embassy_time::{Duration, Instant, Ticker, Timer, with_timeout};
use panic_halt as _;
use pico_data_logger::ntp::{
    NTP_PACKET_LEN, build_ntp_request, ntp_to_unix_seconds, validate_ntp_response,
};
use sht4x::{Precision, Sht4xAsync};
use static_cell::StaticCell;

const NTP_PORT: u16 = 123;
const MQTT_TCP_BUFFER_SIZE: usize = 1024;
const MQTT_TCP_TIMEOUT_SECS: u64 = 10;
const MQTT_TCP_RETRY_DELAY_SECS: u64 = 10;
const MQTT_TCP_ATTEMPTS: u8 = 3;
const MQTT_PACKET_RX_BUFFER_SIZE: usize = 512;
const MQTT_PACKET_TX_BUFFER_SIZE: usize = 1024;
const MQTT_KEEPALIVE_SECS: u16 = 90;

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => UsbInterruptHandler<USB>;
    PIO0_IRQ_0 => PioInterruptHandler<PIO0>;
    DMA_IRQ_0 => dma::InterruptHandler<DMA_CH0>;
    I2C0_IRQ => I2cInterruptHandler<I2C0>;
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
    ntp_host: &'static str,
}

struct ClockAnchor {
    unix_seconds: u64,
    monotonic: Instant,
}

#[derive(Debug)]
enum ClockError {
    Overflow,
}

#[derive(Debug)]
enum ConfigError {
    MissingWifiSsid,
    MissingWifiPassword,
    MissingMqttHost,
    InvalidMqttPort,
    IncompleteMqttCredentials,
}

#[derive(Debug)]
enum ResolveError {
    Dns(embassy_net::dns::Error),
    NoAddresses,
}

impl From<embassy_net::dns::Error> for ResolveError {
    fn from(err: embassy_net::dns::Error) -> Self {
        ResolveError::Dns(err)
    }
}

impl ResolveError {
    fn log(self, hostname: &str) {
        match self {
            Self::Dns(error) => {
                log::error!("DNS lookup failed for {}: {:?}", hostname, error);
            }
            Self::NoAddresses => {
                log::error!("DNS returned no IPv4 addresses for {}", hostname);
            }
        }
    }
}

impl ClockAnchor {
    fn unix_now(&self) -> Result<u64, ClockError> {
        self.unix_seconds
            .checked_add(self.monotonic.elapsed().as_secs())
            .ok_or(ClockError::Overflow)
    }
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
        let ntp_host = option_env!("NTP_HOST").unwrap_or("pool.ntp.org");

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
            ntp_host,
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

#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static, cyw43::NetDriver<'static>>) -> ! {
    runner.run().await;
}

async fn connect_wifi(
    control: &mut cyw43::Control<'_>,
    stack: &embassy_net::Stack<'_>,
    ssid: &str,
    password: &str,
) {
    loop {
        let mut join_options = cyw43::JoinOptions::new(password.as_bytes());
        join_options.auth = cyw43::JoinAuth::Wpa2;

        log::info!("joining WiFi network: {}", ssid);

        match control.join(ssid, join_options).await {
            Ok(()) => {
                log::info!("joined Wi-Fi network: {}", ssid);
                break;
            }
            Err(error) => {
                log::error!("join failed for Wi-Fi network {}: {:?}", ssid, error);
                log::info!("retrying Wi-Fi join in 5 seconds");
                Timer::after_secs(5).await;
            }
        }
    }

    log::info!("waiting for network link");
    stack.wait_link_up().await;
    log::info!("network link up; waiting for DHCP");

    stack.wait_config_up().await;

    match stack.config_v4() {
        Some(ipv4_config) => {
            log::info!(
                "DHCP configured: address={:?}/{} gateway={:?} dns={:?}",
                ipv4_config.address.address(),
                ipv4_config.address.prefix_len(),
                ipv4_config.gateway,
                ipv4_config.dns_servers.as_slice()
            );
        }
        None => {
            log::error!("network configuration became ready without IPv4 configuration");
        }
    }
}

async fn resolve_ipv4(
    stack: embassy_net::Stack<'_>,
    hostname: &str,
) -> Result<embassy_net::IpAddress, ResolveError> {
    let mut last_error: Option<ResolveError> = None;
    let max_attempts = 3;

    for attempt in 1..=max_attempts {
        log::info!(
            "resolving host: {} (attempt {}/{})",
            hostname,
            attempt,
            max_attempts
        );

        let lookup_result = stack
            .dns_query(hostname, embassy_net::dns::DnsQueryType::A)
            .await;

        let query_result: Result<embassy_net::IpAddress, ResolveError> = match lookup_result {
            Ok(addresses) => {
                if addresses.is_empty() {
                    log::error!("no addresses found for host {}", hostname);
                    Err(ResolveError::NoAddresses)
                } else {
                    log::info!("found addresses for host {}: {:?}", hostname, addresses);
                    Ok(addresses
                        .first()
                        .copied()
                        .ok_or(ResolveError::NoAddresses)?)
                }
            }
            Err(error) => {
                log::error!("failed to resolve host {}: {:?}", hostname, error);
                Err(ResolveError::from(error))
            }
        };

        match query_result {
            Ok(address) => {
                return Ok(address);
            }
            Err(error) => {
                last_error = Some(error);
                if attempt < max_attempts {
                    Timer::after_millis(100).await;
                }
            }
        }
    }

    Err(last_error.unwrap_or(ResolveError::NoAddresses))
}

async fn synchronize_clock(
    stack: embassy_net::Stack<'_>,
    server_address: embassy_net::IpAddress,
    request_id: u64,
) -> Option<ClockAnchor> {
    let mut clock_anchor = None;
    let mut ntp_rx_meta = [embassy_net::udp::PacketMetadata::EMPTY; 1];
    let mut ntp_rx_buffer = [0_u8; 512];
    let mut ntp_tx_meta = [embassy_net::udp::PacketMetadata::EMPTY; 1];
    let mut ntp_tx_buffer = [0_u8; NTP_PACKET_LEN];

    let mut ntp_socket = embassy_net::udp::UdpSocket::new(
        stack,
        &mut ntp_rx_meta,
        &mut ntp_rx_buffer,
        &mut ntp_tx_meta,
        &mut ntp_tx_buffer,
    );

    match ntp_socket.bind(0) {
        Ok(()) => {
            log::info!("NTP UDP socket bound");

            let ntp_request = build_ntp_request(request_id);

            let endpoint = embassy_net::IpEndpoint::new(server_address, NTP_PORT);

            match ntp_socket.send_to(&ntp_request, endpoint).await {
                Ok(()) => {
                    log::info!("sent {}-byte NTP request", ntp_request.len());

                    let mut response = [0_u8; 512];

                    match with_timeout(Duration::from_secs(5), ntp_socket.recv_from(&mut response))
                        .await
                    {
                        Ok(Ok((length, metadata))) => {
                            log::info!(
                                "received {}-byte NTP response from {:?}",
                                length,
                                metadata.endpoint
                            );

                            match validate_ntp_response(&response[..length], request_id) {
                                Ok(()) => {
                                    let ntp_seconds = u32::from_be_bytes([
                                        response[40],
                                        response[41],
                                        response[42],
                                        response[43],
                                    ]);

                                    match ntp_to_unix_seconds(ntp_seconds) {
                                        Some(unix_seconds) => {
                                            log::info!(
                                                "NTP response passed validation: unix_seconds={}",
                                                unix_seconds
                                            );

                                            let anchor = ClockAnchor {
                                                unix_seconds,
                                                monotonic: Instant::now(),
                                            };

                                            log::info!(
                                                "clock anchored: unix_seconds={} monotonic_ticks={}",
                                                anchor.unix_seconds,
                                                anchor.monotonic.as_ticks()
                                            );

                                            clock_anchor = Some(anchor);
                                        }
                                        None => {
                                            log::error!(
                                                "rejected NTP timestamp before the Unix epoch"
                                            );
                                        }
                                    }
                                }
                                Err(error) => {
                                    log::error!("rejected NTP response {:?}", error)
                                }
                            }
                        }
                        Ok(Err(error)) => {
                            log::error!("failed to receive NTP response: {:?}", error);
                        }
                        Err(_) => {
                            log::error!("timed out waiting for NTP response");
                        }
                    }
                }
                Err(error) => {
                    log::error!("failed to send NTP request: {:?}", error);
                }
            }
        }
        Err(error) => {
            log::error!("failed to bind NTP UDP socket: {:?}", error);
        }
    }

    clock_anchor
}

#[embassy_executor::main(
    executor = "embassy_rp::executor::Executor",
    entry = "cortex_m_rt::entry"
)]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    let driver = Driver::new(p.USB, Irqs);

    spawner.spawn(logger_task(driver).expect("Failed to start logger task"));

    let mut i2c_config = I2cConfig::default();
    i2c_config.frequency = 100_000; // 100 kHz
    i2c_config.sda_pullup = true;
    i2c_config.scl_pullup = true;

    let i2c = I2c::new_async(
        p.I2C0, p.PIN_1, // SCL
        p.PIN_0, // SDA
        Irqs, i2c_config,
    );

    let mut sht40 = Sht4xAsync::<_, embassy_time::Delay>::new(i2c);
    let mut delay = embassy_time::Delay;

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
    static NETWORK_RESOURCES: StaticCell<embassy_net::StackResources<4>> = StaticCell::new();

    let state = CYW43_STATE.init(cyw43::State::new());

    let (net_device, mut control, runner) = cyw43::new(state, pwr, spi, fw, nvram).await;
    let mut rng = embassy_rp::clocks::RoscRng;
    let seed = rng.next_u64();

    let net_config = embassy_net::Config::dhcpv4(Default::default());

    let (stack, net_runner) = embassy_net::new(
        net_device,
        net_config,
        NETWORK_RESOURCES.init(embassy_net::StackResources::new()),
        seed,
    );

    spawner.spawn(cyw43_task(runner).expect("Failed to start CYW43 runner task"));
    spawner.spawn(net_task(net_runner).expect("Failed to start network runner task"));

    control.init(clm).await;

    control
        .set_power_management(cyw43::PowerManagementMode::PowerSave)
        .await;

    log::info!("radio ready: CYW43439 initialized in PowerSave mode");

    let started_at = Instant::now();

    Timer::after_secs(2).await;

    log::info!("Pico Data Logger v{} starting", env!("CARGO_PKG_VERSION"));

    match sht40.serial_number(&mut delay).await {
        Ok(serial) => {
            log::info!("SHT40 serial number: 0x{:08X}", serial);
        }
        Err(sht4x::Error::I2c(error)) => {
            log::error!("SHT40 I2C error: {:?}", error);
        }
        Err(sht4x::Error::Crc) => {
            log::error!("SHT40 serial number failed CRC validation");
        }
        Err(error) => {
            log::error!("Unexpected SHT40 error: {:?}", error);
        }
    }

    let config = match AppConfig::load() {
        Ok(config) => config,
        Err(error) => {
            log::error!("invalid application configuration: {:?}", error);

            loop {
                Timer::after_secs(60).await;
            }
        }
    };

    connect_wifi(&mut control, &stack, config.wifi_ssid, config.wifi_password).await;

    let mqtt_endpoint = match resolve_ipv4(stack, config.mqtt_host).await {
        Ok(address) => {
            let endpoint = embassy_net::IpEndpoint::new(address, config.mqtt_port);
            log::info!(
                "resolved MQTT endpoint: host={} endpoint={:?}",
                config.mqtt_host,
                endpoint
            );
            Some(endpoint)
        }
        Err(error) => {
            error.log(config.mqtt_host);
            None
        }
    };

    let clock_anchor = loop {
        log::info!("attempting NTP synchronization");

        match resolve_ipv4(stack, config.ntp_host).await {
            Ok(address) => {
                let request_id = rng.next_u64();

                if let Some(anchor) = synchronize_clock(stack, address, request_id).await {
                    break anchor;
                }
            }
            Err(error) => {
                error.log(config.ntp_host);
            }
        }

        log::info!("time remains unsynchronized; retrying in 5 seconds");
        Timer::after_secs(5).await;
    };

    let mut mqtt_rx_buffer = [0_u8; MQTT_TCP_BUFFER_SIZE];
    let mut mqtt_tx_buffer = [0_u8; MQTT_TCP_BUFFER_SIZE];
    let mut mqtt_packet_rx_buffer = [0_u8; MQTT_PACKET_RX_BUFFER_SIZE];
    let mut mqtt_packet_tx_buffer = [0_u8; MQTT_PACKET_TX_BUFFER_SIZE];

    let mqtt_buffers = minimq::Buffers::new(&mut mqtt_packet_rx_buffer, &mut mqtt_packet_tx_buffer);

    let mqtt_config =
        match minimq::ConfigBuilder::new(mqtt_buffers).client_id(config.mqtt_client_id) {
            Ok(builder) => Some(builder.keepalive_interval(MQTT_KEEPALIVE_SECS)),
            Err(error) => {
                log::error!("invalid MQTT client configuration: {:?}", error);
                None
            }
        };

    let mqtt_config = match (mqtt_config, config.mqtt_username, config.mqtt_password) {
        (Some(builder), Some(username), Some(password)) => {
            match builder.auth(username, password.as_bytes()) {
                Ok(builder) => Some(builder),
                Err(error) => {
                    log::error!("invalid MQTT authentication configuration: {:?}", error);
                    None
                }
            }
        }
        (Some(builder), None, None) => Some(builder),
        (Some(_), _, _) => {
            log::error!("MQTT username and password must be configured together");
            None
        }
        (None, _, _) => None,
    };

    let mqtt_session = mqtt_config.map(minimq::Session::new);

    if let (Some(endpoint), Some(mut mqtt_session)) = (mqtt_endpoint, mqtt_session) {
        for attempt in 1..=MQTT_TCP_ATTEMPTS {
            let mut mqtt_socket =
                embassy_net::tcp::TcpSocket::new(stack, &mut mqtt_rx_buffer, &mut mqtt_tx_buffer);

            mqtt_socket.set_timeout(Some(Duration::from_secs(MQTT_TCP_TIMEOUT_SECS)));

            log::info!(
                "connecting to MQTT endpoint {:?} (attempt {}/{})",
                endpoint,
                attempt,
                MQTT_TCP_ATTEMPTS
            );

            let mqtt_connected = match with_timeout(
                Duration::from_secs(MQTT_TCP_TIMEOUT_SECS),
                mqtt_socket.connect(endpoint),
            )
            .await
            {
                Ok(Ok(())) => {
                    log::info!(
                        "MQTT TCP connected: local={:?} remote={:?}",
                        mqtt_socket.local_endpoint(),
                        mqtt_socket.remote_endpoint()
                    );

                    match with_timeout(
                        Duration::from_secs(MQTT_TCP_TIMEOUT_SECS),
                        mqtt_session.connect(mqtt_socket),
                    )
                    .await
                    {
                        Ok(Ok(connection)) => {
                            log::info!("MQTT CONNACK accepted: {:?}", connection.connect_event());
                            true
                        }
                        Ok(Err(error)) => {
                            log::error!("MQTT session establishment failed: {:?}", error);
                            false
                        }
                        Err(_) => {
                            log::error!(
                                "MQTT session establishment timed out after {} seconds",
                                MQTT_TCP_TIMEOUT_SECS
                            );
                            false
                        }
                    }
                }
                Ok(Err(error)) => {
                    log::error!("MQTT TCP connection failed: {:?}", error);
                    false
                }
                Err(_) => {
                    log::error!(
                        "MQTT TCP connection timed out after {} seconds",
                        MQTT_TCP_TIMEOUT_SECS
                    );
                    false
                }
            };

            if mqtt_connected {
                break;
            }

            if attempt < MQTT_TCP_ATTEMPTS {
                log::info!(
                    "creating a fresh MQTT TCP socket in {} seconds",
                    MQTT_TCP_RETRY_DELAY_SECS
                );
                Timer::after_secs(MQTT_TCP_RETRY_DELAY_SECS).await;
            }
        }
    }

    let mut measurement_ticker = Ticker::every(Duration::from_secs(10));
    let mut consecutive_measurements = 0_u32;

    loop {
        match sht40.measure(Precision::High, &mut delay).await {
            Ok(measurement) => {
                consecutive_measurements = consecutive_measurements.saturating_add(1);

                let temperature = measurement.temperature_celsius();
                let humidity = measurement.humidity_percent();

                log::info!(
                    "SHT40 measurement: temperature={:.2}°C, humidity={:.2}% RH consecutive={}",
                    temperature,
                    humidity,
                    consecutive_measurements
                );
            }
            Err(sht4x::Error::I2c(error)) => {
                consecutive_measurements = 0;
                log::error!("SHT40 measurement failed: I2C error {:?}", error);
            }
            Err(sht4x::Error::Crc) => {
                consecutive_measurements = 0;
                log::error!("SHT40 measurement failed: CRC error");
            }
            Err(error) => {
                consecutive_measurements = 0;
                log::error!("SHT40 measurement failed: {:?}", error);
            }
        };

        let unix_seconds = match clock_anchor.unix_now() {
            Ok(value) => value,
            Err(error) => {
                log::error!(
                    "clock arithmetic failed {:?}; operation remains blocked",
                    error
                );
                Timer::after_secs(5).await;
                continue;
            }
        };

        log::info!("current UTC: unix_seconds={}", unix_seconds);
        log::info!("uptime: {} seconds; LED on", started_at.elapsed().as_secs());

        control.gpio_set(0, true).await;
        Timer::after_secs(1).await;

        log::info!(
            "uptime: {} seconds; LED off",
            started_at.elapsed().as_secs()
        );

        control.gpio_set(0, false).await;

        measurement_ticker.next().await;
    }
}
