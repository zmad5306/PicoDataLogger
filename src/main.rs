#![no_std]
#![no_main]

use cyw43::aligned_bytes;
use cyw43_pio::{DEFAULT_CLOCK_DIVIDER, PioSpi};
use embassy_executor::Spawner;
use embassy_futures::select::{Either, Either3, select, select3};
use embassy_rp::bind_interrupts;
use embassy_rp::clocks::RoscRng;
use embassy_rp::dma;
use embassy_rp::flash::{Blocking, Flash};
use embassy_rp::gpio::{Level, Output};
use embassy_rp::i2c::{
    Async as I2cAsync, Config as I2cConfig, I2c, InterruptHandler as I2cInterruptHandler,
};
use embassy_rp::peripherals::{DMA_CH0, FLASH, I2C0, PIO0, USB};
use embassy_rp::pio::{InterruptHandler as PioInterruptHandler, Pio};
use embassy_rp::usb::{Driver, InterruptHandler as UsbInterruptHandler};
use embassy_time::{Delay, Duration, Instant, Timer, with_timeout};
use panic_halt as _;
use pico_data_logger::backoff::RetryBackoff;
use pico_data_logger::flash_queue::{FlashQueue, MeasurementRecord};
use pico_data_logger::ntp::{
    NTP_PACKET_LEN, build_ntp_request, ntp_to_unix_seconds, unix_seconds_from_anchor,
    validate_ntp_response,
};
use pico_data_logger::{Reading, compose_mqtt_client_id, encode_reading, format_hardware_id};
use sht4x::{Precision, Sht4xAsync};
use static_cell::StaticCell;

const NTP_PORT: u16 = 123;
const MQTT_TCP_BUFFER_SIZE: usize = 1024;
const MQTT_TCP_TIMEOUT_SECS: u64 = 10;
const MQTT_PACKET_RX_BUFFER_SIZE: usize = 512;
const MQTT_PACKET_TX_BUFFER_SIZE: usize = 1024;
const MQTT_KEEPALIVE_SECS: u16 = 90;
const READING_JSON_BUFFER_SIZE: usize = 256;
const PUBLISH_INTERVAL_SECS: u64 = 60;
const UTC_REFRESH_INTERVAL_SECS: u64 = 24 * 60 * 60;
const WIFI_JOIN_TIMEOUT_SECS: u64 = 15;
const MQTT_EFFECTIVE_CLIENT_ID_SIZE: usize = 96;
const FLASH_SIZE: usize = 4 * 1024 * 1024;
const STORAGE_OFFSET: u32 = 0x003c_0000;
const STORAGE_LENGTH: u32 = 256 * 1024;

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => UsbInterruptHandler<USB>;
    PIO0_IRQ_0 => PioInterruptHandler<PIO0>;
    DMA_IRQ_0 => dma::InterruptHandler<DMA_CH0>;
    I2C0_IRQ => I2cInterruptHandler<I2C0>;
});

type UsbDriver = Driver<'static, USB>;
type Sensor = Sht4xAsync<I2c<'static, I2C0, I2cAsync>, Delay>;
type MeasurementQueue = FlashQueue<Flash<'static, FLASH, Blocking, FLASH_SIZE>>;

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

struct ClockState {
    anchor: ClockAnchor,
    next_refresh_at: Instant,
    refresh_backoff: RetryBackoff,
}

struct AppRuntime<'a> {
    config: &'a AppConfig,
    stack: embassy_net::Stack<'static>,
    rng: &'a mut RoscRng,
    sensor: &'a mut Sensor,
    delay: &'a mut Delay,
    started_at: Instant,
    clock: ClockState,
    reading_json_buffer: &'a mut [u8; READING_JSON_BUFFER_SIZE],
    hardware_id: &'a str,
    queue: &'a mut MeasurementQueue,
    next_sample_at: Instant,
}

struct Supervisor<'a> {
    app: AppRuntime<'a>,
    control: &'a mut cyw43::Control<'static>,
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
        unix_seconds_from_anchor(self.unix_seconds, self.monotonic.elapsed().as_secs())
            .ok_or(ClockError::Overflow)
    }
}

impl ClockState {
    fn new(anchor: ClockAnchor) -> Self {
        Self {
            anchor,
            next_refresh_at: Instant::now() + Duration::from_secs(UTC_REFRESH_INTERVAL_SECS),
            refresh_backoff: RetryBackoff::new(),
        }
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

        log::info!(
            "joining configured Wi-Fi network with {}-second timeout",
            WIFI_JOIN_TIMEOUT_SECS
        );

        match select(
            Timer::after_secs(WIFI_JOIN_TIMEOUT_SECS),
            control.join(ssid, join_options),
        )
        .await
        {
            Either::First(()) => {
                log::error!(
                    "Wi-Fi join timed out after {} seconds",
                    WIFI_JOIN_TIMEOUT_SECS
                );
                control.leave().await;
            }
            Either::Second(Ok(())) => {
                log::info!("joined configured Wi-Fi network");
                break;
            }
            Either::Second(Err(error)) => {
                log::error!("Wi-Fi join failed: {:?}", error);
            }
        }

        log::info!("retrying Wi-Fi join in 5 seconds");
        Timer::after_secs(5).await;
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

async fn check_sensor(sensor: &mut Sensor, delay: &mut Delay) {
    match sensor.serial_number(delay).await {
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
}

async fn synchronize_clock_until_ready(
    stack: embassy_net::Stack<'_>,
    ntp_host: &str,
    rng: &mut RoscRng,
) -> ClockAnchor {
    loop {
        log::info!("attempting NTP synchronization");

        match resolve_ipv4(stack, ntp_host).await {
            Ok(address) => {
                let request_id = rng.next_u64();

                if let Some(anchor) = synchronize_clock(stack, address, request_id).await {
                    return anchor;
                }
            }
            Err(error) => {
                error.log(ntp_host);
            }
        }

        log::info!("time remains unsynchronized; retrying in 5 seconds");
        Timer::after_secs(5).await;
    }
}

async fn refresh_clock(
    clock: &mut ClockState,
    stack: embassy_net::Stack<'_>,
    ntp_host: &str,
    rng: &mut RoscRng,
) -> bool {
    let refreshed_anchor = match resolve_ipv4(stack, ntp_host).await {
        Ok(address) => {
            let request_id = rng.next_u64();
            synchronize_clock(stack, address, request_id).await
        }
        Err(error) => {
            error.log(ntp_host);
            None
        }
    };

    match refreshed_anchor {
        Some(anchor) => {
            clock.anchor = anchor;
            clock.refresh_backoff.reset();
            clock.next_refresh_at = Instant::now() + Duration::from_secs(UTC_REFRESH_INTERVAL_SECS);
            true
        }
        None => {
            let retry_delay_secs = clock.refresh_backoff.next_delay_secs();
            clock.next_refresh_at = Instant::now() + Duration::from_secs(retry_delay_secs);
            log::error!(
                "UTC refresh failed; retaining last valid anchor and retrying in {} seconds",
                retry_delay_secs
            );
            false
        }
    }
}

async fn capture_reading<'a>(
    sensor: &mut Sensor,
    delay: &mut Delay,
    clock: &ClockAnchor,
    started_at: Instant,
    sequence: u64,
    device_id: &'a str,
    hardware_id: &'a str,
) -> Option<Reading<'a>> {
    let measurement = match sensor.measure(Precision::High, delay).await {
        Ok(measurement) => measurement,
        Err(sht4x::Error::I2c(error)) => {
            log::error!("SHT40 measurement failed: I2C error {:?}", error);
            return None;
        }
        Err(sht4x::Error::Crc) => {
            log::error!("SHT40 measurement failed: CRC error");
            return None;
        }
        Err(error) => {
            log::error!("SHT40 measurement failed {:?}", error);
            return None;
        }
    };

    let Some(temperature_c) = measurement.temperature_celsius().checked_to_num::<f32>() else {
        log::error!("SHT40 fixed-point temperature conversion failed");
        return None;
    };
    let Some(relative_humidity_pct) = measurement.humidity_percent().checked_to_num::<f32>() else {
        log::error!("SHT40 fixed-point humidity conversion failed");
        return None;
    };
    let timestamp_unix_s = match clock.unix_now() {
        Ok(timestamp) => timestamp,
        Err(error) => {
            log::error!(
                "clock arithmetic failed while timestamping measurement: {:?}",
                error
            );
            return None;
        }
    };

    let reading = Reading {
        device_id,
        hardware_id,
        sequence,
        temperature_c,
        relative_humidity_pct,
        timestamp_unix_s,
        uptime_s: started_at.elapsed().as_secs(),
    };

    log::info!(
        "live reading captured: temperature={:.2}°C humidity={:.2}% RH timestamp={} uptime={}",
        reading.temperature_c,
        reading.relative_humidity_pct,
        reading.timestamp_unix_s,
        reading.uptime_s
    );

    Some(reading)
}

async fn capture_and_enqueue(runtime: &mut AppRuntime<'_>) {
    let sequence = runtime.queue.next_sequence();
    let reading = capture_reading(
        runtime.sensor,
        runtime.delay,
        &runtime.clock.anchor,
        runtime.started_at,
        sequence,
        runtime.config.mqtt_client_id,
        runtime.hardware_id,
    )
    .await;
    runtime.next_sample_at += Duration::from_secs(PUBLISH_INTERVAL_SECS);

    let Some(reading) = reading else { return };
    let previous_dropped = runtime.queue.dropped();
    match runtime.queue.append(
        reading.timestamp_unix_s,
        reading.temperature_c,
        reading.relative_humidity_pct,
        reading.uptime_s,
    ) {
        Ok(record) => {
            if runtime.queue.dropped() != previous_dropped {
                log::error!("measurement queue full; discarded oldest record");
            }
            log::info!(
                "queued measurement: sequence={} depth={}/{}",
                record.sequence,
                runtime.queue.depth(),
                runtime.queue.capacity()
            );
        }
        Err(error) => log::error!("failed to persist measurement: {:?}", error),
    }
}

fn queued_reading<'a>(
    record: MeasurementRecord,
    device_id: &'a str,
    hardware_id: &'a str,
) -> Reading<'a> {
    Reading {
        device_id,
        hardware_id,
        sequence: record.sequence,
        temperature_c: record.temperature_c,
        relative_humidity_pct: record.relative_humidity_pct,
        timestamp_unix_s: record.timestamp_unix_s,
        uptime_s: record.uptime_s,
    }
}

async fn drain_queue(
    runtime: &mut AppRuntime<'_>,
    connection: &mut minimq::Connection<'_, '_, embassy_net::tcp::TcpSocket<'_>>,
) -> bool {
    loop {
        while runtime.next_sample_at <= Instant::now() {
            capture_and_enqueue(runtime).await;
        }
        let record = match runtime.queue.peek_oldest() {
            Ok(Some(record)) => record,
            Ok(None) => return true,
            Err(error) => {
                log::error!("failed to read measurement queue: {:?}", error);
                return false;
            }
        };
        let reading = queued_reading(record, runtime.config.mqtt_client_id, runtime.hardware_id);
        let payload = match encode_reading(&reading, runtime.reading_json_buffer) {
            Ok(payload) => payload,
            Err(error) => {
                log::error!("failed to encode queued measurement: {:?}", error);
                return false;
            }
        };
        let publication = minimq::Publication::bytes(runtime.config.mqtt_topic, payload)
            .qos(minimq::QoS::AtLeastOnce);
        let operation = match connection.publish(publication).await {
            Ok(Some(operation)) => operation,
            Ok(None) => {
                log::error!("QoS 1 publish returned no operation handle");
                return false;
            }
            Err(error) => {
                log::error!("queued MQTT publish failed: {:?}", error);
                return false;
            }
        };
        while connection.is_pending(&operation) {
            match select(connection.poll(), Timer::at(runtime.next_sample_at)).await {
                Either::First(Ok(_)) => {}
                Either::First(Err(error)) => {
                    log::error!("MQTT failed while awaiting PUBACK: {:?}", error);
                    return false;
                }
                Either::Second(()) => capture_and_enqueue(runtime).await,
            }
        }
        if !connection.is_complete(&operation) {
            log::error!("queued MQTT publish was not acknowledged");
            return false;
        }
        match runtime.queue.acknowledge_oldest() {
            Ok(_) => log::info!(
                "replayed queued measurement: sequence={} remaining={}",
                record.sequence,
                runtime.queue.depth()
            ),
            Err(error) => {
                log::error!("PUBACK received but queue retirement failed: {:?}", error);
                return false;
            }
        }
    }
}

async fn wait_with_sampling(runtime: &mut AppRuntime<'_>, duration: Duration) {
    let deadline = Instant::now() + duration;
    loop {
        match select(Timer::at(deadline), Timer::at(runtime.next_sample_at)).await {
            Either::First(()) => return,
            Either::Second(()) => capture_and_enqueue(runtime).await,
        }
    }
}

async fn recover_wifi_with_sampling(app: &mut AppRuntime<'_>, control: &mut cyw43::Control<'_>) {
    loop {
        let mut join_options = cyw43::JoinOptions::new(app.config.wifi_password.as_bytes());
        join_options.auth = cyw43::JoinAuth::Wpa2;
        match select3(
            Timer::after_secs(WIFI_JOIN_TIMEOUT_SECS),
            control.join(app.config.wifi_ssid, join_options),
            Timer::at(app.next_sample_at),
        )
        .await
        {
            Either3::First(()) => {
                log::error!("Wi-Fi recovery join timed out");
                control.leave().await;
            }
            Either3::Second(Ok(())) => break,
            Either3::Second(Err(error)) => log::error!("Wi-Fi recovery join failed: {:?}", error),
            Either3::Third(()) => {
                capture_and_enqueue(app).await;
                continue;
            }
        }
        wait_with_sampling(app, Duration::from_secs(5)).await;
    }

    while !app.stack.is_link_up() {
        match select(app.stack.wait_link_up(), Timer::at(app.next_sample_at)).await {
            Either::First(()) => {}
            Either::Second(()) => capture_and_enqueue(app).await,
        }
    }
    while !app.stack.is_config_up() {
        match select(app.stack.wait_config_up(), Timer::at(app.next_sample_at)).await {
            Either::First(()) => {}
            Either::Second(()) => capture_and_enqueue(app).await,
        }
    }
    log::info!("Wi-Fi and DHCP recovered");
}

fn create_mqtt_session<'buffer>(
    config: &AppConfig,
    effective_client_id: &str,
    packet_rx_buffer: &'buffer mut [u8; MQTT_PACKET_RX_BUFFER_SIZE],
    packet_tx_buffer: &'buffer mut [u8; MQTT_PACKET_TX_BUFFER_SIZE],
) -> Option<minimq::Session<'buffer>> {
    let buffers = minimq::Buffers::new(packet_rx_buffer, packet_tx_buffer);
    let builder = match minimq::ConfigBuilder::new(buffers).client_id(effective_client_id) {
        Ok(builder) => builder.keepalive_interval(MQTT_KEEPALIVE_SECS),
        Err(error) => {
            log::error!("invalid MQTT client configuration: {:?}", error);
            return None;
        }
    };

    let builder = match (config.mqtt_username, config.mqtt_password) {
        (Some(username), Some(password)) => match builder.auth(username, password.as_bytes()) {
            Ok(builder) => builder,
            Err(error) => {
                log::error!("invalid MQTT authentication configuration: {:?}", error);
                return None;
            }
        },
        (None, None) => builder,
        _ => {
            log::error!("MQTT username and password must be configured together");
            return None;
        }
    };

    Some(minimq::Session::new(builder))
}

async fn run_connected_session(
    runtime: &mut AppRuntime<'_>,
    connection: &mut minimq::Connection<'_, '_, embassy_net::tcp::TcpSocket<'_>>,
) {
    'publishing: loop {
        if !drain_queue(runtime, connection).await {
            break 'publishing;
        }

        loop {
            let utc_refresh_due = loop {
                match select3(
                    Timer::at(runtime.next_sample_at),
                    connection.poll(),
                    Timer::at(runtime.clock.next_refresh_at),
                )
                .await
                {
                    Either3::First(()) => {
                        capture_and_enqueue(runtime).await;
                        break false;
                    }
                    Either3::Second(Ok(Some(_publication))) => {
                        log::info!("received an unexpected MQTT publication while waiting");
                    }
                    Either3::Second(Ok(None)) => {
                        // Minimq made internal progress, such as keepalive traffic.
                    }
                    Either3::Second(Err(error)) => {
                        log::error!(
                            "MQTT session service failed while waiting for next sample: {:?}",
                            error
                        );
                        break 'publishing;
                    }
                    Either3::Third(()) => {
                        log::info!("UTC refresh deadline reached");
                        break true;
                    }
                }
            };

            if !utc_refresh_due {
                break;
            }

            if refresh_clock(
                &mut runtime.clock,
                runtime.stack,
                runtime.config.ntp_host,
                runtime.rng,
            )
            .await
            {
                log::info!("scheduled UTC clock refresh succeeded");
            }
        }
    }
}

async fn run_supervisor(
    supervisor: &mut Supervisor<'_>,
    mqtt_session: &mut minimq::Session<'_>,
    mqtt_rx_buffer: &mut [u8; MQTT_TCP_BUFFER_SIZE],
    mqtt_tx_buffer: &mut [u8; MQTT_TCP_BUFFER_SIZE],
) -> ! {
    let Supervisor { app, control } = supervisor;
    let mut reconnect_backoff = RetryBackoff::new();

    // supervisor
    loop {
        let mut network_recovered = false;

        if !app.stack.is_link_up() {
            log::info!("network link is down; clearing stale Wi-Fi association state");
            control.leave().await;

            log::info!("returning to Wi-Fi join");
            recover_wifi_with_sampling(app, control).await;
            network_recovered = true;
        }

        if !app.stack.is_config_up() {
            log::info!("network link is up; waiting for DHCP configuration");

            match select3(
                app.stack.wait_config_up(),
                app.stack.wait_link_down(),
                Timer::at(app.next_sample_at),
            )
            .await
            {
                Either3::First(()) => {
                    log::info!("network configuration restored");
                    network_recovered = true;
                }
                Either3::Second(()) => {
                    log::info!("network link dropped while waiting for DHCP");
                    continue;
                }
                Either3::Third(()) => {
                    capture_and_enqueue(app).await;
                    continue;
                }
            }
        }

        if network_recovered {
            log::info!("network recovered; attempting UTC resynchronization");

            if refresh_clock(&mut app.clock, app.stack, app.config.ntp_host, app.rng).await {
                log::info!("UTC clock anchor refreshed after network recovery");
            }
        }

        let endpoint = match resolve_ipv4(app.stack, app.config.mqtt_host).await {
            Ok(address) => {
                let endpoint = embassy_net::IpEndpoint::new(address, app.config.mqtt_port);

                log::info!(
                    "resolved MQTT endpoint: host={} endpoint={:?}",
                    app.config.mqtt_host,
                    endpoint
                );

                endpoint
            }
            Err(error) => {
                error.log(app.config.mqtt_host);

                let retry_delay_secs = reconnect_backoff.next_delay_secs();

                log::info!(
                    "broker DNS unavailable; retrying in {} secs",
                    retry_delay_secs
                );

                wait_with_sampling(app, Duration::from_secs(retry_delay_secs)).await;
                continue;
            }
        };

        let mut mqtt_socket =
            embassy_net::tcp::TcpSocket::new(app.stack, mqtt_rx_buffer, mqtt_tx_buffer);

        mqtt_socket.set_timeout(Some(Duration::from_secs(MQTT_TCP_TIMEOUT_SECS)));

        log::info!("connecting to MQTT endpoint {:?}", endpoint);

        match with_timeout(
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

                mqtt_socket.set_timeout(None);

                match with_timeout(
                    Duration::from_secs(MQTT_TCP_TIMEOUT_SECS),
                    mqtt_session.connect(mqtt_socket),
                )
                .await
                {
                    Ok(Ok(mut connection)) => {
                        log::info!("MQTT CONNACK accepted: {:?}", connection.connect_event());

                        reconnect_backoff.reset();

                        run_connected_session(app, &mut connection).await;
                    }
                    Ok(Err(error)) => {
                        log::error!("MQTT session establishment failed: {:?}", error);
                    }
                    Err(_) => {
                        log::error!(
                            "MQTT session establishment timed out after {} seconds",
                            MQTT_TCP_TIMEOUT_SECS
                        );
                    }
                };
            }
            Ok(Err(error)) => {
                log::error!("MQTT TCP connection failed: {:?}", error);
            }
            Err(_) => {
                log::error!(
                    "MQTT TCP connection timed out after {} seconds",
                    MQTT_TCP_TIMEOUT_SECS
                );
            }
        };

        let retry_delay_secs = reconnect_backoff.next_delay_secs();

        log::info!(
            "creating a fresh MQTT TCP socket in {} seconds",
            retry_delay_secs
        );

        wait_with_sampling(app, Duration::from_secs(retry_delay_secs)).await;
    }
}

#[embassy_executor::main(
    executor = "embassy_rp::executor::Executor",
    entry = "cortex_m_rt::entry"
)]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    let driver = Driver::new(p.USB, Irqs);

    spawner.spawn(logger_task(driver).expect("Failed to start logger task"));

    let hardware_chip_id = match embassy_rp::otp::get_chipid() {
        Ok(chip_id) => chip_id,
        Err(error) => {
            log::error!("failed to read RP2350 hardware ID: {:?}", error);
            loop {
                Timer::after_secs(60).await;
            }
        }
    };

    let flash = Flash::<_, Blocking, FLASH_SIZE>::new_blocking(p.FLASH);
    let (mut measurement_queue, recovery) =
        match FlashQueue::recover(flash, STORAGE_OFFSET, STORAGE_LENGTH) {
            Ok(value) => value,
            Err(error) => {
                log::error!("measurement queue recovery failed: {:?}", error);
                loop {
                    Timer::after_secs(60).await;
                }
            }
        };

    let mut i2c_config = I2cConfig::default();
    i2c_config.frequency = 100_000; // 100 kHz
    i2c_config.sda_pullup = true;
    i2c_config.scl_pullup = true;

    let i2c = I2c::new_async(
        p.I2C0, p.PIN_1, // SCL
        p.PIN_0, // SDA
        Irqs, i2c_config,
    );

    let mut sht40: Sensor = Sht4xAsync::new(i2c);
    let mut delay = Delay;

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
    log::info!(
        "measurement queue recovered: depth={} corrupt={} next_sequence={} capacity={}",
        recovery.depth,
        recovery.corrupt_records,
        recovery.next_sequence,
        measurement_queue.capacity()
    );

    check_sensor(&mut sht40, &mut delay).await;

    let config = match AppConfig::load() {
        Ok(config) => config,
        Err(error) => {
            log::error!("invalid application configuration: {:?}", error);

            loop {
                Timer::after_secs(60).await;
            }
        }
    };
    let mut hardware_id_buffer = [0_u8; 16];
    let hardware_id = format_hardware_id(hardware_chip_id, &mut hardware_id_buffer);
    let mut effective_client_id_buffer = [0_u8; MQTT_EFFECTIVE_CLIENT_ID_SIZE];
    let effective_client_id = match compose_mqtt_client_id(
        config.mqtt_client_id,
        hardware_id,
        &mut effective_client_id_buffer,
    ) {
        Ok(client_id) => client_id,
        Err(error) => {
            log::error!("failed to compose unique MQTT client ID: {:?}", error);
            loop {
                Timer::after_secs(60).await;
            }
        }
    };
    log::info!(
        "device identity ready: device_id={} hardware_id={} mqtt_client_id={}",
        config.mqtt_client_id,
        hardware_id,
        effective_client_id
    );

    connect_wifi(&mut control, &stack, config.wifi_ssid, config.wifi_password).await;

    let clock_anchor = synchronize_clock_until_ready(stack, config.ntp_host, &mut rng).await;
    let clock = ClockState::new(clock_anchor);

    let mut mqtt_rx_buffer = [0_u8; MQTT_TCP_BUFFER_SIZE];
    let mut mqtt_tx_buffer = [0_u8; MQTT_TCP_BUFFER_SIZE];
    let mut mqtt_packet_rx_buffer = [0_u8; MQTT_PACKET_RX_BUFFER_SIZE];
    let mut mqtt_packet_tx_buffer = [0_u8; MQTT_PACKET_TX_BUFFER_SIZE];

    let mut reading_json_buffer = [0_u8; READING_JSON_BUFFER_SIZE];

    let mqtt_session = create_mqtt_session(
        &config,
        effective_client_id,
        &mut mqtt_packet_rx_buffer,
        &mut mqtt_packet_tx_buffer,
    );

    let Some(mut mqtt_session) = mqtt_session else {
        log::error!("MQTT configuration unavailable; supervisor cannot start");

        loop {
            Timer::after_secs(PUBLISH_INTERVAL_SECS).await;
        }
    };

    let mut supervisor = Supervisor {
        app: AppRuntime {
            config: &config,
            stack,
            rng: &mut rng,
            sensor: &mut sht40,
            delay: &mut delay,
            started_at,
            clock,
            reading_json_buffer: &mut reading_json_buffer,
            hardware_id,
            queue: &mut measurement_queue,
            next_sample_at: Instant::now(),
        },
        control: &mut control,
    };

    run_supervisor(
        &mut supervisor,
        &mut mqtt_session,
        &mut mqtt_rx_buffer,
        &mut mqtt_tx_buffer,
    )
    .await
}
