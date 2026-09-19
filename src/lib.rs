#![no_std]

pub mod ntp;

/// A sensor measurement with its collection time and device uptime.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct Reading {
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

pub fn encode_reading<'buffer>(
    reading: &Reading,
    buffer: &'buffer mut [u8],
) -> Result<&'buffer [u8], EncodeError> {
    let encoded_len =
        serde_json_core::to_slice(reading, buffer).map_err(|_| EncodeError::BufferTooSmall)?;
    Ok(&buffer[..encoded_len])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example_reading() -> Reading {
        Reading {
            temperature_c: 23.4,
            relative_humidity_pct: 45.6,
            timestamp_unix_s: 1_700_000_000,
            uptime_s: 120,
        }
    }

    fn assert_encoding_contains(reading: &Reading, expected: &[u8]) {
        let mut buffer = [0_u8; 128];
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
        let mut buffer = [0_u8; 128];

        let encoded = encode_reading(&example_reading(), &mut buffer).unwrap();

        assert_eq!(
            encoded,
            br#"{"temperature_c":23.4,"relative_humidity_pct":45.6,"timestamp_unix_s":1700000000,"uptime_s":120}"#
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
}
