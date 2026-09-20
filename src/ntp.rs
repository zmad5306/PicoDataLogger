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

pub fn unix_seconds_from_anchor(anchor_unix_seconds: u64, elapsed_seconds: u64) -> Option<u64> {
    anchor_unix_seconds.checked_add(elapsed_seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    const REQUEST_ID: u64 = 0x0123_4567_89ab_cdef;

    fn valid_response() -> [u8; NTP_PACKET_LEN] {
        let mut response = [0_u8; NTP_PACKET_LEN];

        // LI = 0, VN = 4, Mode = 4 (server).
        response[0] = 0x24;
        response[1] = 1;
        response[24..32].copy_from_slice(&REQUEST_ID.to_be_bytes());

        response
    }

    #[test]
    fn builds_an_ntpv4_client_request_with_the_request_id() {
        let request = build_ntp_request(REQUEST_ID);

        assert_eq!(request[0], 0x23);
        assert_eq!(&request[40..48], &REQUEST_ID.to_be_bytes());
    }

    #[test]
    fn accepts_a_valid_server_response() {
        assert_eq!(validate_ntp_response(&valid_response(), REQUEST_ID), Ok(()));
    }

    #[test]
    fn rejects_a_short_response() {
        assert_eq!(
            validate_ntp_response(&valid_response()[..NTP_PACKET_LEN - 1], REQUEST_ID),
            Err(NtpValidationError::TooShort)
        );
    }

    #[test]
    fn rejects_a_response_that_is_not_from_a_server() {
        let mut response = valid_response();
        response[0] = 0x23;

        assert_eq!(
            validate_ntp_response(&response, REQUEST_ID),
            Err(NtpValidationError::InvalidServerMode)
        );
    }

    #[test]
    fn rejects_an_unsynchronized_server() {
        let mut response = valid_response();
        response[0] = 0xe4;

        assert_eq!(
            validate_ntp_response(&response, REQUEST_ID),
            Err(NtpValidationError::UnsynchronizedServer)
        );
    }

    #[test]
    fn rejects_invalid_strata() {
        for stratum in [0, 16] {
            let mut response = valid_response();
            response[1] = stratum;

            assert_eq!(
                validate_ntp_response(&response, REQUEST_ID),
                Err(NtpValidationError::InvalidStratum)
            );
        }
    }

    #[test]
    fn rejects_a_response_for_a_different_request() {
        let mut response = valid_response();
        response[24..32].copy_from_slice(&(REQUEST_ID + 1).to_be_bytes());

        assert_eq!(
            validate_ntp_response(&response, REQUEST_ID),
            Err(NtpValidationError::RequestTimestampMismatch)
        );
    }

    #[test]
    fn converts_the_ntp_epoch_to_the_unix_epoch() {
        assert_eq!(
            ntp_to_unix_seconds(NTP_UNIX_EPOCH_OFFSET_SECONDS as u32),
            Some(0)
        );
        assert_eq!(
            ntp_to_unix_seconds((NTP_UNIX_EPOCH_OFFSET_SECONDS + 1_700_000_000) as u32),
            Some(1_700_000_000)
        );
    }

    #[test]
    fn rejects_an_ntp_timestamp_before_the_unix_epoch() {
        assert_eq!(
            ntp_to_unix_seconds(NTP_UNIX_EPOCH_OFFSET_SECONDS as u32 - 1),
            None
        );
    }

    #[test]
    fn derives_current_unix_time_from_an_anchor_and_elapsed_time() {
        assert_eq!(
            unix_seconds_from_anchor(1_700_000_000, 125),
            Some(1_700_000_125)
        );
    }

    #[test]
    fn reports_anchor_arithmetic_overflow() {
        assert_eq!(unix_seconds_from_anchor(u64::MAX, 1), None);
    }
}
