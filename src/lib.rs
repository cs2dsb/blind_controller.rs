#![no_std]
#![feature(sync_unsafe_cell)]
#![feature(impl_trait_in_assoc_type)]
#![feature(never_type)]
#![feature(const_size_of_val)]

#[macro_export]
macro_rules! mk_static {
    ($t:ty, $val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        #[deny(unused_attributes)]
        let x = STATIC_CELL.uninit().write(($val));
        x
    }};
}

pub mod logging;
pub mod rng;
pub mod partitions;

#[cfg(feature = "wifi")]
pub mod nvs;

#[cfg(feature = "wifi")]
pub mod wifi;
