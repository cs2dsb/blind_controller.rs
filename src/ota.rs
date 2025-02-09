// Inspired by <https://github.com/bjoernQ/esp32c3-ota-experiment/blob/main/src/ota.rs>

use core::fmt;

use esp_storage::{FlashStorage, FlashStorageError};
use embedded_storage::{ReadStorage, Storage};
use log::debug;
use log::trace;
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
#[derive(Debug, Clone, Copy)]
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
    Storage(FlashStorageError),
}

impl From<FlashStorageError> for Error {
    fn from(value: FlashStorageError) -> Self {
        Self::Storage(value)
    }
}

pub struct Ota<'a> {
    flash: &'a mut FlashStorage,
}

impl<'a> Ota<'a> {
    pub fn new(flash: &'a mut FlashStorage) -> Ota<'a> {
        debug!("OTA data partition: {OTA_DATA_PARTITION:?}");
        Self { flash }
    }

    pub fn read_select_entries(&mut self) -> Result<(), Error> {
        let (s0, s1) = self.get_slot_seq();
        let (s0_, s1_) = self.get_slot_seq_new()?;

        debug!("Old: 0x{s0:x}, 0x{s1:x}");
        debug!("New: 0x{s0_:x}, 0x{s1_:x}");

        let old_slot = self.current_slot();
        let new_slot = self.current_slot_new()?;
        debug!("Old: {old_slot:?}");
        debug!("New: {new_slot:?}");

        // assert_eq!(s0, s0_);
        // assert_eq!(s1, s1_);
        // assert_eq!(old_slot, new_slot);

        self.set_current_slot(new_slot.next())?;


        debug!("size: {}, chunk: {}", OTA_0_PARTITION.size, FlashStorage::SECTOR_SIZE);

        assert!(OTA_0_PARTITION.size == OTA_1_PARTITION.size);

        if new_slot == Slot::Slot0 {
            const SECTOR_SIZE: usize = FlashStorage::SECTOR_SIZE as usize;
            const PART_SIZE: usize = OTA_0_PARTITION.size as usize;
            let mut buf = [0_u8; SECTOR_SIZE];
            let mut chunks =PART_SIZE / SECTOR_SIZE;
            if chunks * SECTOR_SIZE > PART_SIZE {
                chunks += 1;
            }

            for i in 0..chunks {
                let buf = &mut buf[0..SECTOR_SIZE.min(PART_SIZE - SECTOR_SIZE*i)];
                trace!("Reading {i} [{}]", buf.len());
                self.flash.read(OTA_0_PARTITION.offset + (i * SECTOR_SIZE) as u32, buf)?;
                trace!("Writing {i}");
                self.flash.write(OTA_1_PARTITION.offset + (i * SECTOR_SIZE) as u32, buf)?;
            }
        } else {
            trace!("Booted from copied fw!");
        }

        Ok(())
    }

    fn get_slot_seq(&mut self) -> (u32, u32) {
        let mut buffer1 = [0u8; 0x20];
        let mut buffer2 = [0u8; 0x20];
        self.flash.read(0xd000, &mut buffer1).unwrap();
        self.flash.read(0xe000, &mut buffer2).unwrap();
        let mut seq0bytes = [0u8; 4];
        let mut seq1bytes = [0u8; 4];
        seq0bytes[..].copy_from_slice(&buffer1[..4]);
        seq1bytes[..].copy_from_slice(&buffer2[..4]);
        let seq0 = u32::from_le_bytes(seq0bytes);
        let seq1 = u32::from_le_bytes(seq1bytes);
        (seq0, seq1)
    }

    pub fn current_slot(&mut self) -> Slot {
        let (seq0, seq1) = self.get_slot_seq();

        if seq0 == 0xffffffff && seq1 == 0xffffffff {
            Slot::None
        } else if seq0 == 0xffffffff {
            Slot::Slot1
        } else if seq1 == 0xffffffff {
            Slot::Slot0
        } else if seq0 > seq1 {
            Slot::Slot0
        } else {
            Slot::Slot1
        }
    }

    pub fn current_slot_new(&mut self) -> Result<Slot, Error> {
        let (seq0, seq1) = self.get_slot_seq_new()?;

        let slot = if seq0 == 0xffffffff && seq1 == 0xffffffff {
            Slot::None
        } else if seq0 == 0xffffffff {
            Slot::Slot1
        } else if seq1 == 0xffffffff {
            Slot::Slot0
        } else if seq0 > seq1 {
            Slot::Slot0
        } else {
            Slot::Slot1
        };

        Ok(slot)
    }

    fn get_ota_entry(&mut self, slot: SelectEntrySlot) -> Result<OtaSelectEntry, Error> {
        OtaSelectEntry::read(slot, &mut self.flash)
    }

    fn get_slot_seq_new(&mut self) -> Result<(u32, u32), Error> {
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

    pub fn set_current_slot(&mut self, slot: Slot) -> Result<(), Error> {
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

        
        let (entry0_dirty, entry1_dirty) = match (&entry0, &entry1) {
            // No slot is current. Only valid when there is a factory partition in addition to the 2 OTA partitions
            (OtaSelectEntry { ota_seq: _seq0 @ 0xFFFFFFFF, .. }, 
             OtaSelectEntry { ota_seq: _seq1 @ 0xFFFFFFFF, .. }) => {
                entry0.ota_seq = 1;
                (true, false)
            },
             // Slot 1 contains a valid image
            (OtaSelectEntry { ota_seq: _seq0 @ 0xFFFFFFFF, .. }, 
             OtaSelectEntry { ota_seq: seq1 @ ..0xFFFFFFFF, .. }) => {
                entry0.ota_seq = seq1 + 1;
                (true, false)
            },
            // Slot 0 contains a valid image
           (OtaSelectEntry { ota_seq: seq0 @ ..0xFFFFFFFF, .. }, 
            OtaSelectEntry { ota_seq: _seq1 @ 0xFFFFFFFF, .. }) => {
               entry1.ota_seq = seq0 + 1;
               (false, true)
           },
           // Both slots contain valid images
           (OtaSelectEntry { ota_seq: seq0 @ ..0xFFFFFFFF, .. }, 
            OtaSelectEntry { ota_seq: seq1 @ ..0xFFFFFFFF, .. }) => if seq1 > seq0 {
                entry0.ota_seq = seq1 + 1;
                (true, false)
            } else {
                entry1.ota_seq = seq0 + 1;
                (false, true)
           },
        };
        assert!(!(entry0_dirty == true && entry1_dirty == true), "Both slots cannot both be marked as dirty");
        assert!(!(entry0_dirty == false && entry1_dirty == false), "Both slots cannot both be marked as not dirty");

        if entry0_dirty {
            assert_eq!(slot, Slot::Slot0);
            debug!("OtaSelectEntry[0] sequence updated to {}", entry0.ota_seq);
            entry0.ota_state = OtaSelectEntryState::New;
            entry0.write(SelectEntrySlot::Zero, &mut self.flash, true)?;

            let check = self.get_ota_entry(SelectEntrySlot::Zero)?;
            debug!("E0w: {entry0:?}");
            debug!("E0r: {check:?}");
        }

        if entry1_dirty {
            assert_eq!(slot, Slot::Slot1);
            debug!("OtaSelectEntry[1] sequence updated to {}", entry1.ota_seq);
            entry1.ota_state = OtaSelectEntryState::New;
            entry1.write(SelectEntrySlot::One, &mut self.flash, true)?;
        }

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
}

