//! Power-loss-tolerant, bounded measurement queue for NOR flash.

use embedded_storage::nor_flash::NorFlash;

pub const RECORD_VERSION: u8 = 1;
pub const RECORD_SIZE: usize = 256;
pub const ERASE_SIZE: usize = 4096;
const MAGIC: u32 = 0x514C_4450; // "PDLQ" in little endian.
const ERASED: u8 = 0xff;
const COMMITTED: u8 = 0x7f;
const ACKNOWLEDGED: u8 = 0x3f;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeasurementRecord {
    pub sequence: u64,
    pub timestamp_unix_s: u64,
    pub temperature_c: f32,
    pub relative_humidity_pct: f32,
    pub uptime_s: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueError<E> {
    Storage(E),
    InvalidGeometry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryReport {
    pub depth: usize,
    pub corrupt_records: usize,
    pub next_sequence: u64,
}

pub struct FlashQueue<F> {
    flash: F,
    start: u32,
    length: u32,
    depth: usize,
    next_sequence: u64,
    dropped: u64,
}

impl<F> FlashQueue<F>
where
    F: NorFlash,
{
    pub fn recover(
        flash: F,
        start: u32,
        length: u32,
    ) -> Result<(Self, RecoveryReport), QueueError<F::Error>> {
        if length < (ERASE_SIZE * 2) as u32
            || start.checked_add(length).is_none()
            || start as usize + length as usize > flash.capacity()
            || !(start as usize).is_multiple_of(ERASE_SIZE)
            || !(length as usize).is_multiple_of(ERASE_SIZE)
            || F::ERASE_SIZE != ERASE_SIZE
            || F::WRITE_SIZE > 1
            || !RECORD_SIZE.is_multiple_of(F::WRITE_SIZE)
            || !ERASE_SIZE.is_multiple_of(RECORD_SIZE)
        {
            return Err(QueueError::InvalidGeometry);
        }

        let mut queue = Self {
            flash,
            start,
            length,
            depth: 0,
            next_sequence: 0,
            dropped: 0,
        };
        let mut corrupt_records = 0;
        let mut max_sequence = None;
        for slot in 0..queue.slot_count() {
            match queue.read_slot(slot)? {
                Slot::Active(record) => {
                    queue.depth += 1;
                    max_sequence = Some(
                        max_sequence
                            .map_or(record.sequence, |value: u64| value.max(record.sequence)),
                    );
                }
                Slot::Acknowledged(Some(record)) => {
                    max_sequence = Some(
                        max_sequence
                            .map_or(record.sequence, |value: u64| value.max(record.sequence)),
                    );
                }
                Slot::Corrupt | Slot::Acknowledged(None) => corrupt_records += 1,
                Slot::Erased => {}
            }
        }
        queue.next_sequence = max_sequence.map_or(0, |value| value.wrapping_add(1));
        queue.reclaim_inactive_sectors()?;
        let report = RecoveryReport {
            depth: queue.depth,
            corrupt_records,
            next_sequence: queue.next_sequence,
        };
        Ok((queue, report))
    }

    pub fn depth(&self) -> usize {
        self.depth
    }
    pub fn dropped(&self) -> u64 {
        self.dropped
    }
    pub fn next_sequence(&self) -> u64 {
        self.next_sequence
    }
    pub fn capacity(&self) -> usize {
        self.slot_count() - self.slots_per_sector()
    }
    pub fn into_inner(self) -> F {
        self.flash
    }

    pub fn append(
        &mut self,
        timestamp_unix_s: u64,
        temperature_c: f32,
        relative_humidity_pct: f32,
        uptime_s: u64,
    ) -> Result<MeasurementRecord, QueueError<F::Error>> {
        if self.depth == self.capacity() {
            self.remove_oldest()?;
            self.dropped = self.dropped.saturating_add(1);
        }
        let record = MeasurementRecord {
            sequence: self.next_sequence,
            timestamp_unix_s,
            temperature_c,
            relative_humidity_pct,
            uptime_s,
        };
        self.next_sequence = self.next_sequence.wrapping_add(1);
        let slot = self
            .find_erased_slot()?
            .ok_or(QueueError::InvalidGeometry)?;
        self.write_record(slot, record)?;
        self.depth += 1;
        self.reclaim_inactive_sectors()?;
        Ok(record)
    }

    pub fn peek_oldest(&mut self) -> Result<Option<MeasurementRecord>, QueueError<F::Error>> {
        Ok(self.find_oldest()?.map(|(_, record)| record))
    }

    pub fn acknowledge_oldest(
        &mut self,
    ) -> Result<Option<MeasurementRecord>, QueueError<F::Error>> {
        let Some((slot, record)) = self.find_oldest()? else {
            return Ok(None);
        };
        self.write_state(slot, ACKNOWLEDGED)?;
        self.depth -= 1;
        self.reclaim_inactive_sectors()?;
        Ok(Some(record))
    }

    fn remove_oldest(&mut self) -> Result<(), QueueError<F::Error>> {
        let _ = self.acknowledge_oldest()?;
        Ok(())
    }

    fn slot_count(&self) -> usize {
        self.length as usize / RECORD_SIZE
    }
    fn slots_per_sector(&self) -> usize {
        ERASE_SIZE / RECORD_SIZE
    }
    fn address(&self, slot: usize) -> u32 {
        self.start + (slot * RECORD_SIZE) as u32
    }

    fn read_slot(&mut self, slot: usize) -> Result<Slot, QueueError<F::Error>> {
        let mut bytes = [0xff; RECORD_SIZE];
        self.flash
            .read(self.address(slot), &mut bytes)
            .map_err(QueueError::Storage)?;
        if bytes.iter().all(|byte| *byte == ERASED) {
            return Ok(Slot::Erased);
        }
        if bytes[0] == ACKNOWLEDGED {
            return Ok(Slot::Acknowledged(decode(&bytes)));
        }
        if bytes[0] != COMMITTED {
            return Ok(Slot::Corrupt);
        }
        Ok(match decode(&bytes) {
            Some(record) => Slot::Active(record),
            None => Slot::Corrupt,
        })
    }

    fn find_oldest(&mut self) -> Result<Option<(usize, MeasurementRecord)>, QueueError<F::Error>> {
        let mut oldest = None;
        for slot in 0..self.slot_count() {
            if let Slot::Active(record) = self.read_slot(slot)?
                && oldest
                    .as_ref()
                    .is_none_or(|(_, current): &(usize, MeasurementRecord)| {
                        record.sequence < current.sequence
                    })
            {
                oldest = Some((slot, record));
            }
        }
        Ok(oldest)
    }

    fn find_erased_slot(&mut self) -> Result<Option<usize>, QueueError<F::Error>> {
        for slot in 0..self.slot_count() {
            if matches!(self.read_slot(slot)?, Slot::Erased) {
                return Ok(Some(slot));
            }
        }
        Ok(None)
    }

    fn write_record(
        &mut self,
        slot: usize,
        record: MeasurementRecord,
    ) -> Result<(), QueueError<F::Error>> {
        let mut bytes = encode(record);
        bytes[0] = ERASED;
        self.flash
            .write(self.address(slot), &bytes)
            .map_err(QueueError::Storage)?;
        self.write_state(slot, COMMITTED)
    }

    fn write_state(&mut self, slot: usize, state: u8) -> Result<(), QueueError<F::Error>> {
        self.flash
            .write(self.address(slot), &[state])
            .map_err(QueueError::Storage)
    }

    fn reclaim_inactive_sectors(&mut self) -> Result<(), QueueError<F::Error>> {
        let mut newest_slot = None;
        let mut newest_sequence = None;
        for slot in 0..self.slot_count() {
            let record = match self.read_slot(slot)? {
                Slot::Active(record) | Slot::Acknowledged(Some(record)) => Some(record),
                Slot::Erased | Slot::Acknowledged(None) | Slot::Corrupt => None,
            };
            if let Some(record) = record
                && newest_sequence.is_none_or(|sequence| record.sequence > sequence)
            {
                newest_sequence = Some(record.sequence);
                newest_slot = Some(slot);
            }
        }
        for sector in 0..(self.length as usize / ERASE_SIZE) {
            let first = sector * self.slots_per_sector();
            let mut active = false;
            let mut dirty = false;
            for slot in first..first + self.slots_per_sector() {
                match self.read_slot(slot)? {
                    Slot::Active(_) => active = true,
                    Slot::Acknowledged(_) | Slot::Corrupt => dirty = true,
                    Slot::Erased => {}
                }
            }
            let preserves_newest = newest_slot
                .is_some_and(|slot| slot >= first && slot < first + self.slots_per_sector());
            if !active && dirty && !preserves_newest {
                let from = self.start + (sector * ERASE_SIZE) as u32;
                self.flash
                    .erase(from, from + ERASE_SIZE as u32)
                    .map_err(QueueError::Storage)?;
            }
        }
        Ok(())
    }
}

enum Slot {
    Erased,
    Active(MeasurementRecord),
    Acknowledged(Option<MeasurementRecord>),
    Corrupt,
}

fn encode(record: MeasurementRecord) -> [u8; RECORD_SIZE] {
    let mut bytes = [0xff; RECORD_SIZE];
    bytes[0] = COMMITTED;
    bytes[1] = RECORD_VERSION;
    bytes[4..8].copy_from_slice(&MAGIC.to_le_bytes());
    bytes[8..16].copy_from_slice(&record.sequence.to_le_bytes());
    bytes[16..24].copy_from_slice(&record.timestamp_unix_s.to_le_bytes());
    bytes[24..28].copy_from_slice(&record.temperature_c.to_bits().to_le_bytes());
    bytes[28..32].copy_from_slice(&record.relative_humidity_pct.to_bits().to_le_bytes());
    bytes[32..40].copy_from_slice(&record.uptime_s.to_le_bytes());
    let crc = crc32(&bytes[1..40]);
    bytes[40..44].copy_from_slice(&crc.to_le_bytes());
    bytes
}

fn decode(bytes: &[u8; RECORD_SIZE]) -> Option<MeasurementRecord> {
    if bytes[1] != RECORD_VERSION || u32::from_le_bytes(bytes[4..8].try_into().ok()?) != MAGIC {
        return None;
    }
    let expected = u32::from_le_bytes(bytes[40..44].try_into().ok()?);
    if crc32(&bytes[1..40]) != expected {
        return None;
    }
    Some(MeasurementRecord {
        sequence: u64::from_le_bytes(bytes[8..16].try_into().ok()?),
        timestamp_unix_s: u64::from_le_bytes(bytes[16..24].try_into().ok()?),
        temperature_c: f32::from_bits(u32::from_le_bytes(bytes[24..28].try_into().ok()?)),
        relative_humidity_pct: f32::from_bits(u32::from_le_bytes(bytes[28..32].try_into().ok()?)),
        uptime_s: u64::from_le_bytes(bytes[32..40].try_into().ok()?),
    })
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (0_u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;
    use embedded_storage::nor_flash::{ErrorType, NorFlashError, NorFlashErrorKind, ReadNorFlash};

    #[derive(Debug, Clone, Copy)]
    struct FakeError;
    impl NorFlashError for FakeError {
        fn kind(&self) -> NorFlashErrorKind {
            NorFlashErrorKind::Other
        }
    }
    struct FakeFlash {
        bytes: [u8; ERASE_SIZE * 3],
        fail_after: Option<usize>,
        writes: usize,
        erases: usize,
    }
    impl FakeFlash {
        fn new() -> Self {
            Self {
                bytes: [0xff; ERASE_SIZE * 3],
                fail_after: None,
                writes: 0,
                erases: 0,
            }
        }
    }
    impl ErrorType for FakeFlash {
        type Error = FakeError;
    }
    impl ReadNorFlash for FakeFlash {
        const READ_SIZE: usize = 1;
        fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
            bytes.copy_from_slice(&self.bytes[offset as usize..offset as usize + bytes.len()]);
            Ok(())
        }
        fn capacity(&self) -> usize {
            self.bytes.len()
        }
    }
    impl NorFlash for FakeFlash {
        const WRITE_SIZE: usize = 1;
        const ERASE_SIZE: usize = ERASE_SIZE;
        fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
            if self.fail_after == Some(self.writes) {
                return Err(FakeError);
            }
            self.writes += 1;
            for (target, source) in self.bytes[offset as usize..][..bytes.len()]
                .iter_mut()
                .zip(bytes)
            {
                if (*target | *source) != *target {
                    return Err(FakeError);
                }
                *target &= *source;
            }
            Ok(())
        }
        fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
            self.erases += 1;
            self.bytes[from as usize..to as usize].fill(0xff);
            Ok(())
        }
    }

    fn queue() -> FlashQueue<FakeFlash> {
        FlashQueue::recover(FakeFlash::new(), 0, (ERASE_SIZE * 3) as u32)
            .unwrap()
            .0
    }
    fn append(queue: &mut FlashQueue<FakeFlash>, value: u64) -> MeasurementRecord {
        queue
            .append(1_700_000_000 + value, value as f32, 50.0, value)
            .unwrap()
    }

    #[test]
    fn empty_queue_recovers() {
        let queue = queue();
        assert_eq!(queue.depth(), 0);
        assert_eq!(queue.capacity(), 32);
    }
    #[test]
    fn fifo_and_acknowledgment() {
        let mut q = queue();
        let a = append(&mut q, 1);
        append(&mut q, 2);
        assert_eq!(q.peek_oldest().unwrap(), Some(a));
        assert_eq!(q.acknowledge_oldest().unwrap(), Some(a));
        assert_eq!(q.peek_oldest().unwrap().unwrap().sequence, 1);
    }
    #[test]
    fn reboot_reconstructs_sequence_and_depth() {
        let mut q = queue();
        append(&mut q, 1);
        append(&mut q, 2);
        q.acknowledge_oldest().unwrap();
        let flash = q.into_inner();
        let (mut recovered, report) =
            FlashQueue::recover(flash, 0, (ERASE_SIZE * 3) as u32).unwrap();
        assert_eq!(report.depth, 1);
        assert_eq!(report.next_sequence, 2);
        assert_eq!(recovered.peek_oldest().unwrap().unwrap().sequence, 1);
    }
    #[test]
    fn sequence_survives_reboot_when_queue_is_empty() {
        let mut q = queue();
        append(&mut q, 1);
        q.acknowledge_oldest().unwrap();
        let flash = q.into_inner();
        let (mut recovered, report) =
            FlashQueue::recover(flash, 0, (ERASE_SIZE * 3) as u32).unwrap();
        assert_eq!(report.depth, 0);
        assert_eq!(report.next_sequence, 1);
        assert_eq!(append(&mut recovered, 2).sequence, 1);
    }
    #[test]
    fn overflow_discards_oldest_and_reuses_sectors_in_batches() {
        let mut q = queue();
        for value in 0..48 {
            append(&mut q, value);
        }
        assert_eq!(q.depth(), 32);
        assert_eq!(q.dropped(), 16);
        assert_eq!(q.peek_oldest().unwrap().unwrap().sequence, 16);
        assert!(q.into_inner().erases <= 2);
    }
    #[test]
    fn corrupt_record_is_ignored_on_recovery() {
        let mut q = queue();
        append(&mut q, 1);
        let mut flash = q.into_inner();
        flash.bytes[24] ^= 1;
        let (_, report) = FlashQueue::recover(flash, 0, (ERASE_SIZE * 3) as u32).unwrap();
        assert_eq!(report.depth, 0);
        assert_eq!(report.corrupt_records, 1);
    }
    #[test]
    fn interrupted_body_write_is_ignored() {
        let mut q = queue();
        q.flash.fail_after = Some(q.flash.writes + 1);
        assert!(q.append(1, 2.0, 3.0, 4).is_err());
        let flash = q.into_inner();
        let (_, report) = FlashQueue::recover(flash, 0, (ERASE_SIZE * 3) as u32).unwrap();
        assert_eq!(report.depth, 0);
    }

    #[test]
    fn failed_acknowledgment_replays_the_record_after_reboot() {
        let mut q = queue();
        let record = append(&mut q, 1);
        q.flash.fail_after = Some(q.flash.writes);
        assert!(q.acknowledge_oldest().is_err());
        q.flash.fail_after = None;
        let flash = q.into_inner();
        let (mut recovered, report) =
            FlashQueue::recover(flash, 0, (ERASE_SIZE * 3) as u32).unwrap();
        assert_eq!(report.depth, 1);
        assert_eq!(recovered.peek_oldest().unwrap(), Some(record));
    }

    #[test]
    fn valid_records_before_a_corrupt_tail_are_preserved() {
        let mut q = queue();
        let first = append(&mut q, 1);
        append(&mut q, 2);
        let mut flash = q.into_inner();
        flash.bytes[RECORD_SIZE + 24] ^= 1;
        let (mut recovered, report) =
            FlashQueue::recover(flash, 0, (ERASE_SIZE * 3) as u32).unwrap();
        assert_eq!(report.depth, 1);
        assert_eq!(report.corrupt_records, 1);
        assert_eq!(recovered.peek_oldest().unwrap(), Some(first));
    }
}
