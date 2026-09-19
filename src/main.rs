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
use embassy_time::{Duration, Instant, Timer, with_timeout};
use panic_halt as _;
use static_cell::StaticCell;

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => UsbInterruptHandler<USB>;
    PIO0_IRQ_0 => PioInterruptHandler<PIO0>;
    DMA_IRQ_0 => dma::InterruptHandler<DMA_CH0>;
});

type UsbDriver = Driver<'static, USB>;

const NTP_PACKET_LEN: usize = 48;
const NTP_PORT: u16 = 123;
const NTP_UNIX_EPOCH_OFFSET_SECONDS: u64 = 2_208_988_800;

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
enum NtpValidationError {
    TooShort,
    InvalidServerMode,
    UnsynchronizedServer,
    InvalidStratum,
    RequestTimestampMismatch,
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

fn build_ntp_request(request_id: u64) -> [u8; NTP_PACKET_LEN] {
    let mut packet = [0_u8; NTP_PACKET_LEN];

    // LI = 0 (00): no leap-second warning.
    // VN = 4 (100): NTP version 4.
    // Mode = 3 (011): client request.

    // 00_100_011 = 0010_0011 = 0x23

    packet[0] = 0x23;
    packet[40..48].copy_from_slice(&request_id.to_be_bytes());

    packet
}

fn validate_ntp_response(response: &[u8], request_id: u64) -> Result<(), NtpValidationError> {
    if response.len() < NTP_PACKET_LEN {
        return Err(NtpValidationError::TooShort);
    }

    // First byte layout: [LI: bits 7-6] [VN: bits 5-3] [Mode: bits 2-0].
    // Mask off LI and VN, leaving only the three-bit mode.
    let mode = response[0] & 0b0000_0111;

    if mode != 4 {
        return Err(NtpValidationError::InvalidServerMode);
    }

    // Shift away VN and Mode, leaving the two-bit leap indicator.
    let leap_indicator = response[0] >> 6;

    // Leap-indicator values:
    // - 0: no warning
    // - 1: the current day will contain an added leap second
    // - 2: the current day will omit a leap second
    // - 3: the server clock is unsynchronized—reject its time

    if leap_indicator == 3 {
        return Err(NtpValidationError::UnsynchronizedServer);
    }

    // Stratum 1-15 identifies a synchronized primary or secondary time source.
    // Stratum 0 is a control/Kiss-o'-Death response; values above 15 are invalid.
    let stratum = response[1];

    if stratum == 0 || stratum > 15 {
        return Err(NtpValidationError::InvalidStratum);
    }

    // An NTP server copies the client's transmit timestamp (request bytes 40..48)
    // into the response's originate timestamp field (response bytes 24..32).
    let expected_originate = request_id.to_be_bytes();
    let actual_originate = &response[24..32];

    if actual_originate != expected_originate.as_slice() {
        return Err(NtpValidationError::RequestTimestampMismatch);
    }

    Ok(())
}

fn ntp_to_unix_seconds(ntp_seconds: u32) -> Option<u64> {
    u64::from(ntp_seconds).checked_sub(NTP_UNIX_EPOCH_OFFSET_SECONDS)
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

    let _mqtt_address = match resolve_ipv4(stack, config.mqtt_host).await {
        Ok(address) => {
            log::info!("resolved MQTT host {} to {:?}", config.mqtt_host, address);
            Some(address)
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

    loop {
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

        Timer::after_secs(1).await;
    }
}
