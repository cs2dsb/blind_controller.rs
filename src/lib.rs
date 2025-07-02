#![no_std]
#![feature(sync_unsafe_cell)]
#![feature(impl_trait_in_assoc_type)]
#![feature(never_type)]

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
pub mod rtc;
pub mod system_time;

#[cfg(feature = "storage")]
pub mod nvs;

#[cfg(feature = "storage")]
pub mod ota;

#[cfg(feature = "wifi")]
pub mod wifi;

#[cfg(feature = "wifi")]
pub mod http;

#[cfg(feature = "wifi")]
pub mod ntp;
