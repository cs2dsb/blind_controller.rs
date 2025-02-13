use core::time::Duration;

use esp_hal::rtc_cntl::{sleep::TimerWakeupSource, Rtc};
use log::info;

pub fn enter_deep(mut rtc: Rtc, interval: Duration) -> ! {
    let wakeup_source = TimerWakeupSource::new(interval);
    
    info!("Entering deep sleep for {:?}", interval);
    rtc.sleep_deep(&[&wakeup_source]);
}