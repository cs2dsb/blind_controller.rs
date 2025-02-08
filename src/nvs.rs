use esp_storage::{FlashStorage, FlashStorageError};
use embedded_storage::{ReadStorage, Storage};
use log::debug;

use crate::partitions::NVS_PARTITION;

pub const MAGIC: u32 = 0xdeadbeef;
pub const MIN_OFFSET: u32 = size_of_val(&MAGIC) as u32;

#[derive(Debug)]
pub enum Error {
    OutOfBounds,
    Storage(FlashStorageError),
}

impl From<FlashStorageError> for Error {
    fn from(value: FlashStorageError) -> Self {
        Self::Storage(value)
    }
}

pub struct Nvs {
    flash: FlashStorage,
    offset: u32,
    size: u32,
}

impl Nvs {
    pub fn new() -> Self {
        let flash = FlashStorage::new();
        debug!("Flash size: 0x{:x?}, NVS: {NVS_PARTITION:?}", flash.capacity());

        let offset = NVS_PARTITION.offset;
        let size = NVS_PARTITION.size;

        Self {
            flash,
            offset,
            size,
        }
    }

    fn valid_offset(&self, offset: u32, len: usize) -> Result<(), Error> {
        if offset < MIN_OFFSET || offset + len as u32 > self.size {
            Err(Error::OutOfBounds)
        } else {
            Ok(())
        }
    }

    pub fn is_valid(&mut self) -> Result<bool, Error> {
        let mut buf = [0; size_of_val(&MAGIC)];
        self.flash.read(self.offset, &mut buf)?;

        let val = u32::from_le_bytes(buf);

        Ok(MAGIC == val)
    }

    pub fn set_valid(&mut self, valid: bool) -> Result<(), Error> {
        let mut buf = [0; size_of_val(&MAGIC)];

        if valid {
            buf = MAGIC.to_le_bytes();
        }

        self.flash.write(self.offset, &buf)?;

        Ok(())
    }

    pub fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), Error> {
        self.valid_offset(offset,buf.len())?;
        self.flash.read(self.offset + offset, buf)?;
        Ok(())
    }

    pub fn write(&mut self, offset: u32, buf: &[u8]) -> Result<(), Error> {
        self.valid_offset(offset,buf.len())?;
        self.flash.write(self.offset + offset, buf)?;
        Ok(())
    }
}