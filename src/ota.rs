// Inspired by <https://github.com/bjoernQ/esp32c3-ota-experiment/blob/main/src/ota.rs>

use core::fmt;

use esp_storage::{FlashStorage, FlashStorageError};
use embedded_storage::{ReadStorage, Storage};
use log::debug;
use log::warn;

use crate::partitions::OTA_0_PARTITION;
use crate::partitions::OTA_1_PARTITION;
use crate::partitions::OTA_DATA_PARTITION;

const CRC_ALGO: crc::Crc<u32> = crc::Crc::<u32>::new(&crc::Algorithm {
    width: 32,
    // From <https://github.com/espressif/esp-idf/blob/c5865270b50529cd32353f588d8a917d89f3dba4/components/esp_rom/include/esp_rom_crc.h#L29>
    poly: 0x04c11db7,
    init: 0,
    refin: true,
    refout: true,
    xorout: 0xffffffff,
    check: 0,
    residue: 0,
});

// From <https://github.com/espressif/esp-idf/blob/c5865270b50529cd32353f588d8a917d89f3dba4/components/bootloader_support/include/esp_flash_partitions.h#L67-L75>
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(unused)]
enum OtaSelectEntryState {
    New             = 0x0,         // Monitor the first boot. In bootloader this state is changed to ESP_OTA_IMG_PENDING_VERIFY
    PendingVerify   = 0x1,         // First boot for this app was. If while the second boot this state is then it will be changed to ABORTED
    Valid           = 0x2,         // App was confirmed as workable. App can boot and work without limits
    Invalid         = 0x3,         // App was confirmed as non-workable. This app will not selected to boot at all
    Aborted         = 0x4,         // App could not confirm the workable or non-workable. In bootloader IMG_PENDING_VERIFY state will be changed to IMG_ABORTED. This app will not selected to boot at all
    Undefined       = 0xFFFFFFFF,  // Undefined. App can boot and work without limits
}

impl From<u32> for OtaSelectEntryState {
    fn from(value: u32) -> Self {
        use OtaSelectEntryState::*;
        match value {
            0x0 => New,
            0x1 => PendingVerify,
            0x2 => Valid,
            0x3 => Invalid,
            0x4 => Aborted,
            _ => Undefined,
        }
    }
}

// From <https://github.com/espressif/esp-idf/blob/c5865270b50529cd32353f588d8a917d89f3dba4/components/bootloader_support/include/esp_flash_partitions.h#L79-L84>
#[repr(C)]
#[derive(Clone)]
struct OtaSelectEntry {
    ota_seq: u32,
    seq_label: [u8; 20],
    ota_state: OtaSelectEntryState,
    crc: u32, /* CRC32 of ota_seq field only */
}

impl OtaSelectEntry {
    fn calculate_checksum(&self) -> u32 {
        let bytes = self.ota_seq.to_le_bytes();
        CRC_ALGO.checksum(&bytes)
    }

    fn checksum_ok(&self) -> bool {
        let crc = self.calculate_checksum();
        crc == self.crc
    }

    fn reset(&mut self) {
        self.ota_seq = 0xFFFFFFFF;
        self.seq_label = [0xFF; 20];
        self.ota_state = OtaSelectEntryState::Undefined;
        self.crc = self.calculate_checksum();
    } 

    fn write(&mut self, slot: SelectEntrySlot, flash: &mut FlashStorage, update_crc: bool) -> Result<(), Error> {
        if update_crc {
            self.crc = self.calculate_checksum()
        } else if !self.checksum_ok() {
            Err(Error::ChecksumInvalid)?;
        }

        let offset = OTA_DATA_PARTITION.offset + slot.offset();
        let mut buf = [0_u8; size_of::<OtaSelectEntry>()];

        let ota_seq = self.ota_seq.to_le_bytes();
        buf[0..4].copy_from_slice(&ota_seq);

        let crc = self.crc.to_le_bytes();
        buf[size_of::<OtaSelectEntry>() - 4..].copy_from_slice(&crc);

        let ota_state = (self.ota_state as u32).to_le_bytes();
        let i = size_of::<OtaSelectEntry>() - 8;
        buf[i..i+4].copy_from_slice(&ota_state);

        buf[4..4+20].copy_from_slice(&self.seq_label);
        
        flash.write(offset, &buf)?;
        Ok(())
    }

    fn read(slot: SelectEntrySlot, flash: &mut FlashStorage) -> Result<Self, Error> {
        let offset = OTA_DATA_PARTITION.offset + slot.offset();
        let mut buf = [0; size_of::<OtaSelectEntry>()];
        
        flash.read(offset, &mut buf)?;
        
        let ota_seq = {
            let mut ota_seq = [0_u8; 4];
            ota_seq.copy_from_slice(&buf[0..4]);
            u32::from_le_bytes(ota_seq)
        };

        let crc = {
            let mut crc = [0_u8; 4];
            crc.copy_from_slice(&buf[size_of::<OtaSelectEntry>() - 4..]);
            u32::from_le_bytes(crc)
        };

        let ota_state = {
            let mut ota_state = [0_u8; 4];
            let i = size_of::<OtaSelectEntry>() - 8;
            ota_state.copy_from_slice(&buf[i..i+4]);
            OtaSelectEntryState::from(u32::from_le_bytes(ota_state))
        };

        let mut self_ = Self {
            ota_seq,
            seq_label: [0; 20],
            ota_state,
            crc,
        };

        self_.seq_label.copy_from_slice(&buf[4..4+20]);

        Ok(self_)
    }
}

impl fmt::Debug for OtaSelectEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OtaSelectEntry")
            .field("ota_seq", &format_args!("0x{:X}", self.ota_seq))
            .field("seq_label", &format_args!("0x[{:X?}]", &self.seq_label))
            .field("ota_state",  &format_args!("{:?}", &self.ota_state))
            .field("crc",  &format_args!("0x{:X}", self.crc))
            .field("crc_ok", &self.checksum_ok())
            .finish()
    }
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd)]
pub enum SelectEntrySlot {
    Zero,
    One,
}

impl SelectEntrySlot {
    fn offset(self) -> u32 {
        match self {
            SelectEntrySlot::Zero => 0,
            // They are positioned at the front of sequential sectors in flash
            SelectEntrySlot::One => FlashStorage::SECTOR_SIZE,
        }
    }
}

#[derive(Debug)]
pub enum Error {
    ChecksumInvalid,
    TooLarge,
    Storage(FlashStorageError),
}

impl From<FlashStorageError> for Error {
    fn from(value: FlashStorageError) -> Self {
        Self::Storage(value)
    }
}

pub struct Ota<'a> {
    flash: &'a mut FlashStorage,
    update_state: Option<(Slot, u32)>,
}

impl<'a> Ota<'a> {
    pub fn new(flash: &'a mut FlashStorage) -> Ota<'a> {
        debug!("OTA data partition: {OTA_DATA_PARTITION:?}");
        Self { flash, update_state: None }
    }

    pub fn prepare_for_update(&mut self) -> Result<(), Error> {
        let update_slot = self.update_slot()?;
        self.update_state = Some((update_slot, 0));
        Ok(())
    }

    pub fn commit_update(&mut self) -> Result<(), Error> {
        // TODO: checksum?
        let (slot, _offset) = self.update_state.take().expect("commit_update called with no update in progress. Call prepare_for_update first");
        self.set_current_slot(slot)?;
        Ok(())
    }

    pub fn write_update(&mut self, buf: &[u8]) -> Result<(), Error> {
        let (slot, offset) = self.update_state.as_mut().expect("write_update called with no update in progress. Call prepare_for_update first");
        let len = buf.len();
        
        if *offset + len as u32 > slot.size() {
            Err(Error::TooLarge)?;
        }

        if len > 0 {
            let final_offset = slot.offset() + *offset;
            self.flash.write(final_offset, buf)?;    
            *offset += len as u32;
        } else {
            warn!("write_update called with 0 length buffer");
        }

        Ok(())
    }

    pub fn update_slot(&mut self) -> Result<Slot, Error> {

        let [entry0, entry1] = self.get_ota_entries()?;
        
        debug!("Entry0: {entry0:?}");
        debug!("Entry1: {entry1:?}");

        // If either slot is invalid, that's the update slot because we don't want to overwrite the current running firmware
        if entry0.ota_state != OtaSelectEntryState::Valid {
            return Ok(Slot::Slot0);
        } else if entry1.ota_state != OtaSelectEntryState::Valid {
            return Ok(Slot::Slot1);
        }

        let (seq0, seq1) = self.get_slot_seq()?;

        let slot = if seq0 == 0xffffffff && seq1 == 0xffffffff {
            Slot::Slot1
        } else if seq0 == 0xffffffff {
            Slot::Slot0
        } else if seq1 == 0xffffffff {
            Slot::Slot1
        } else if seq0 > seq1 {
            Slot::Slot1
        } else {
            Slot::Slot0
        };

        Ok(slot)
    }

    fn get_ota_entry(&mut self, slot: SelectEntrySlot) -> Result<OtaSelectEntry, Error> {
        OtaSelectEntry::read(slot, &mut self.flash)
    }

    fn get_slot_seq(&mut self) -> Result<(u32, u32), Error> {
        let entry0 = self.get_ota_entry(SelectEntrySlot::Zero)?;
        let entry1 = self.get_ota_entry(SelectEntrySlot::One)?;

        let seq0 = if entry0.checksum_ok() { 
            entry0.ota_seq 
        } else {
            0xFFFFFFFF
        };
        let seq1 = if entry1.checksum_ok() { 
            entry1.ota_seq 
        } else {
            0xFFFFFFFF
        };

        Ok((seq0, seq1))
    }

    fn get_ota_entries(&mut self) -> Result<[OtaSelectEntry; 2], Error> {
        let entry0 = self.get_ota_entry(SelectEntrySlot::Zero)?;
        let entry1 = self.get_ota_entry(SelectEntrySlot::One)?;

        Ok([entry0, entry1])
    }

    pub fn set_current_slot(&mut self, expected_slot: Slot) -> Result<(), Error> {
        let [mut entry0, mut entry1] = self.get_ota_entries()?;
        
        debug!("Entry0: {entry0:?}");
        debug!("Entry1: {entry1:?}");
        
        if !entry0.checksum_ok() {
            warn!("OtaSelectEntry[0] had invalid checksum, resetting");
            entry0.reset();
            // entry0.write(SelectEntrySlot::Zero, &mut self.flash, false)?;
        }

        if !entry1.checksum_ok() {
            warn!("OtaSelectEntry[1] had invalid checksum, resetting");
            entry1.reset();
            // entry1.write(SelectEntrySlot::One, &mut self.flash, false)?;
        }

        
        let (seq, slot) = match (&entry0, &entry1) {
            // No slot is current. Only valid when there is a factory partition in addition to the 2 OTA partitions
            (OtaSelectEntry { ota_seq: _seq0 @ 0xFFFFFFFF, .. }, 
             OtaSelectEntry { ota_seq: _seq1 @ 0xFFFFFFFF, .. }) => {
                (1, Slot::Slot0)
            },
             // Slot 1 contains a valid image
            (OtaSelectEntry { ota_seq: _seq0 @ 0xFFFFFFFF, .. }, 
             OtaSelectEntry { ota_seq: seq1 @ ..0xFFFFFFFF, .. }) => {
                (seq1 + 1, Slot::Slot0)
            },
            // Slot 0 contains a valid image
           (OtaSelectEntry { ota_seq: seq0 @ ..0xFFFFFFFF, .. }, 
            OtaSelectEntry { ota_seq: _seq1 @ 0xFFFFFFFF, .. }) => {
                (seq0 + 1, Slot::Slot1)
           },
           // Both slots contain valid images
           (OtaSelectEntry { ota_seq: seq0 @ ..0xFFFFFFFF, .. }, 
            OtaSelectEntry { ota_seq: seq1 @ ..0xFFFFFFFF, .. }) => if seq1 > seq0 {
                (seq1 + 1, Slot::Slot0)
            } else {
                (seq0 + 1, Slot::Slot1)
           },
        };
        
        let slot = match (entry0.ota_state, entry1.ota_state) {
            // Both valid, use the slot selected above
            (OtaSelectEntryState::Valid, OtaSelectEntryState::Valid) => slot,
            // 0 valid, 1 something else
            (OtaSelectEntryState::Valid, _) => Slot::Slot1,
            // 1 valid, 0 something else
            (_, OtaSelectEntryState::Valid) => Slot::Slot0,
            // Neither valid...
            _ => {
                warn!("Both OTA partitions in !valid state");
                // Most likely factory boot
                Slot::Slot1
            },
        };

        assert_eq!(slot, expected_slot);

        debug!("Committing update to {slot:?}");
        let (entry, entry_slot) = match slot {
            Slot::Slot0 => (&mut entry0, SelectEntrySlot::Zero),
            _ => (&mut entry1, SelectEntrySlot::One),
        };

        debug!("seq: {} -> {}", entry.ota_seq, seq);
        entry.ota_seq = seq;
        entry.ota_state = OtaSelectEntryState::New;

        entry.write(entry_slot, &mut self.flash, true)?;
        debug!("Entry written");

        Ok(())
    }
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd)]
pub enum Slot {
    None,
    Slot0,
    Slot1,
}

impl Slot {
    pub fn number(&self) -> usize {
        match self {
            Slot::None => 0,
            Slot::Slot0 => 0,
            Slot::Slot1 => 1,
        }
    }

    pub fn next(&self) -> Slot {
        match self {
            Slot::None => Slot::Slot0,
            Slot::Slot0 => Slot::Slot1,
            Slot::Slot1 => Slot::Slot0,
        }
    }

    pub fn offset(&self) -> u32 {
        match self.number() {
            0 => OTA_0_PARTITION.offset,
            _ => OTA_1_PARTITION.offset,
        }
    }

    pub fn size(&self) -> u32 {
        match self.number() {
            0 => OTA_0_PARTITION.size,
            _ => OTA_1_PARTITION.size,
        }
    }
}

