pub const NTP_PACKET_LEN: usize = 48;
const NTP_UNIX_EPOCH_OFFSET_SECONDS: u64 = 2_208_988_800;

#[derive(Debug, PartialEq, Eq)]
pub enum NtpValidationError {
    TooShort,
    InvalidServerMode,
    UnsynchronizedServer,
    InvalidStratum,
    RequestTimestampMismatch,
}

pub fn build_ntp_request(request_id: u64) -> [u8; NTP_PACKET_LEN] {
    let mut packet = [0_u8; NTP_PACKET_LEN];

    // LI = 0 (00): no leap-second warning.
    // VN = 4 (100): NTP version 4.
    // Mode = 3 (011): client request.

    // 00_100_011 = 0010_0011 = 0x23

    packet[0] = 0x23;
    packet[40..48].copy_from_slice(&request_id.to_be_bytes());

    packet
}

pub fn validate_ntp_response(response: &[u8], request_id: u64) -> Result<(), NtpValidationError> {
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

pub fn ntp_to_unix_seconds(ntp_seconds: u32) -> Option<u64> {
    u64::from(ntp_seconds).checked_sub(NTP_UNIX_EPOCH_OFFSET_SECONDS)
}
