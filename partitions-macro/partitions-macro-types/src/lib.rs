#![no_std]
use core::fmt;

#[derive(Clone)]
pub struct Partition {
    pub offset: u32,
    pub size: u32,
}

impl fmt::Debug for Partition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {

        f.debug_struct("Partition")
            .field("offset", &format_args!("0x{:X}", self.offset))
            .field("size",  &format_args!("0x{:X}", self.size))
            .finish()
    }
}