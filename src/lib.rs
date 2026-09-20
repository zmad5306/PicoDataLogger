#![no_std]

pub mod backoff;
pub mod flash_queue;
pub mod ntp;

/// A sensor measurement with its collection time and device uptime.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct Reading<'a> {
    /// Human-readable logical device identity from `MQTT_CLIENT_ID`.
    pub device_id: &'a str,
    /// Stable RP2350 OTP chip identifier, encoded as 16 lowercase hex digits.
    pub hardware_id: &'a str,
    /// Monotonically increasing identifier used to recognize replayed messages.
    pub sequence: u64,
    pub temperature_c: f32,
    pub relative_humidity_pct: f32,
    /// UTC time when this measurement was taken
    pub timestamp_unix_s: u64,
    /// Seconds since boot, retained for reboot and timing diagnostics.
    pub uptime_s: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodeError {
    BufferTooSmall,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityError {
    BufferTooSmall,
}

pub fn encode_reading<'buffer>(
    reading: &Reading<'_>,
    buffer: &'buffer mut [u8],
) -> Result<&'buffer [u8], EncodeError> {
    let encoded_len =
        serde_json_core::to_slice(reading, buffer).map_err(|_| EncodeError::BufferTooSmall)?;
    Ok(&buffer[..encoded_len])
}

pub fn format_hardware_id(chip_id: u64, buffer: &mut [u8; 16]) -> &str {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for (index, byte) in buffer.iter_mut().enumerate() {
        let shift = (15 - index) * 4;
        *byte = HEX[((chip_id >> shift) & 0x0f) as usize];
    }
    // Every byte comes from the ASCII-only lookup table above.
    core::str::from_utf8(buffer).expect("hex lookup table must contain valid UTF-8")
}

pub fn compose_mqtt_client_id<'a>(
    device_id: &str,
    hardware_id: &str,
    buffer: &'a mut [u8],
) -> Result<&'a str, IdentityError> {
    let required = device_id
        .len()
        .checked_add(1)
        .and_then(|length| length.checked_add(hardware_id.len()))
        .ok_or(IdentityError::BufferTooSmall)?;
    if required > buffer.len() {
        return Err(IdentityError::BufferTooSmall);
    }
    buffer[..device_id.len()].copy_from_slice(device_id.as_bytes());
    buffer[device_id.len()] = b'-';
    buffer[device_id.len() + 1..required].copy_from_slice(hardware_id.as_bytes());
    core::str::from_utf8(&buffer[..required]).map_err(|_| IdentityError::BufferTooSmall)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example_reading() -> Reading<'static> {
        Reading {
            device_id: "basement-sensor",
            hardware_id: "0123456789abcdef",
            sequence: 42,
            temperature_c: 23.4,
            relative_humidity_pct: 45.6,
            timestamp_unix_s: 1_700_000_000,
            uptime_s: 120,
        }
    }

    fn assert_encoding_contains(reading: &Reading<'_>, expected: &[u8]) {
        let mut buffer = [0_u8; 256];
        let encoded =
            encode_reading(reading, &mut buffer).expect("test buffer should be large enough");

        assert!(
            encoded
                .windows(expected.len())
                .any(|bytes| bytes == expected)
        );
    }

    #[test]
    fn encodes_the_exact_example_payload() {
        let mut buffer = [0_u8; 256];

        let encoded = encode_reading(&example_reading(), &mut buffer).unwrap();

        assert_eq!(
            encoded,
            br#"{"device_id":"basement-sensor","hardware_id":"0123456789abcdef","sequence":42,"temperature_c":23.4,"relative_humidity_pct":45.6,"timestamp_unix_s":1700000000,"uptime_s":120}"#
        );
    }

    #[test]
    fn reports_an_undersized_buffer() {
        let mut buffer = [0_u8; 16];

        let error = encode_reading(&example_reading(), &mut buffer).unwrap_err();

        assert_eq!(error, EncodeError::BufferTooSmall);
    }

    #[test]
    fn encodes_negative_temperature() {
        let mut reading = example_reading();
        reading.temperature_c = -5.25;

        assert_encoding_contains(&reading, br#""temperature_c":-5.25"#);
    }

    #[test]
    fn encodes_zero_percent_humidity() {
        let mut reading = example_reading();
        reading.relative_humidity_pct = 0.0;

        assert_encoding_contains(&reading, br#""relative_humidity_pct":0.0"#);
    }

    #[test]
    fn encodes_one_hundred_percent_humidity() {
        let mut reading = example_reading();
        reading.relative_humidity_pct = 100.0;

        assert_encoding_contains(&reading, br#""relative_humidity_pct":100.0"#);
    }

    #[test]
    fn preserves_known_unix_timestamp() {
        assert_encoding_contains(&example_reading(), br#""timestamp_unix_s":1700000000"#);
    }

    #[test]
    fn formats_hardware_id_as_fixed_width_lowercase_hex() {
        let mut buffer = [0_u8; 16];
        assert_eq!(
            format_hardware_id(0x0123_4567_89ab_cdef, &mut buffer),
            "0123456789abcdef"
        );
    }

    #[test]
    fn composes_human_readable_and_hardware_mqtt_client_id() {
        let mut buffer = [0_u8; 64];
        assert_eq!(
            compose_mqtt_client_id("home-office", "0123456789abcdef", &mut buffer),
            Ok("home-office-0123456789abcdef")
        );
    }

    #[test]
    fn reports_an_undersized_mqtt_client_id_buffer() {
        let mut buffer = [0_u8; 8];
        assert_eq!(
            compose_mqtt_client_id("home-office", "0123456789abcdef", &mut buffer),
            Err(IdentityError::BufferTooSmall)
        );
    }
}
