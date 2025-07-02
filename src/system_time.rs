// todo: get rid of this
#![allow(static_mut_refs)]

use core::cell::SyncUnsafeCell;
#[allow(unused)]
use log::*;

pub struct SystemTime {}
use esp_hal::{ram, peripherals};
use time::{error::ComponentRange, OffsetDateTime, UtcOffset };


struct ClockConfig {
    configured: bool,
    offset_seconds: i32,
    calibration: u64,
    ntp_synchronized: bool,
}

impl ClockConfig {
    const fn new() -> Self {
        Self {
            configured: false,
            offset_seconds: 0,
            calibration: 0,
            ntp_synchronized: false,
        }
    }
}

#[ram(rtc_fast)]
static mut CLOCK_CONFIG: SyncUnsafeCell<ClockConfig> = SyncUnsafeCell::new(ClockConfig::new());

// TODO: Calibrate RTC by using NTP instead of 40mhz clock (real freq = delta rtc / delta real time)
// https://www.youtube.com/watch?v=fZAR8WTKiSg

/// Cribbed from <https://github.com/esp-rs/esp-hal/pull/1883>. Only c3 has been implemented
impl SystemTime {
    
    /// Read the current value of the boot time registers in microseconds.
    pub fn get_boot_time_us(&self) -> u64 {
        let rtc_cntl = unsafe { &*peripherals::LPWR::ptr() };

        let (l, h) = (rtc_cntl.store2(), rtc_cntl.store3());

        let l = l.read().bits() as u64;
        let h = h.read().bits() as u64;

        // https://github.com/espressif/esp-idf/blob/23e4823f17a8349b5e03536ff7653e3e584c9351/components/newlib/port/esp_time_impl.c#L115
        let r = l + (h << 32);
        //trace!("get_boot_time_us: {}", r);
        r
    }

    fn set_boot_time_us(&self, boot_time_us: u64) {
        //trace!("set_boot_time_us: {}", boot_time_us);
        
        let rtc_cntl = unsafe { &*peripherals::LPWR::ptr() };
        
        let (l, h) = (rtc_cntl.store2(), rtc_cntl.store3());

        // https://github.com/espressif/esp-idf/blob/23e4823f17a8349b5e03536ff7653e3e584c9351/components/newlib/port/esp_time_impl.c#L102-L103
        l.write(|w| unsafe { w.bits((boot_time_us & 0xffffffff) as u32) });
        h.write(|w| unsafe { w.bits((boot_time_us >> 32) as u32) });
    }

    pub fn get_rtc_time_us(&self) -> u64 {
        // let r = self.get_rtc_time_raw() * 1_000_000 / RtcClock::get_slow_freq().frequency().to_Hz() as u64;
        let r2 = self.get_rtc_time_cal_us();
        //trace!("get_rtc_time_us: r: {}, r2: {}", r, r2);
        r2
    }

    pub fn configure(&mut self, offset: UtcOffset) {
        self.set_offset(offset);
        let clock_config: Option<&'static mut _> = unsafe { CLOCK_CONFIG.get().as_mut() };
        let clock_config: &'static mut _ = unsafe { clock_config.unwrap_unchecked() };

        clock_config.calibration = self.get_cal_val() as u64;
        clock_config.configured = true;
    }

    pub fn calibration(&self) -> u64 {
        let clock_config: Option<&'static mut _> = unsafe { CLOCK_CONFIG.get().as_mut() };
        let clock_config: &'static mut _ = unsafe { clock_config.unwrap_unchecked() };

        clock_config.calibration
    }

    pub fn offset(&self) -> Result<UtcOffset, ComponentRange> {
        let clock_config: Option<&'static mut _> = unsafe { CLOCK_CONFIG.get().as_mut() };
        let clock_config: &'static mut _ = unsafe { clock_config.unwrap_unchecked() };

        UtcOffset::from_whole_seconds(clock_config.offset_seconds)
    }

    pub fn set_offset(&mut self, offset: UtcOffset) {
        let clock_config: Option<&'static mut _> = unsafe { CLOCK_CONFIG.get().as_mut() };
        let clock_config: &'static mut _ = unsafe { clock_config.unwrap_unchecked() };

        clock_config.offset_seconds = offset.whole_seconds();
    }

    pub fn ntp_synchronized(&self) -> bool {
        let clock_config: Option<&'static mut _> = unsafe { CLOCK_CONFIG.get().as_mut() };
        let clock_config: &'static mut _ = unsafe { clock_config.unwrap_unchecked() };

        clock_config.ntp_synchronized
    }

    pub fn set_ntp_synchronized(&mut self, ntp_synchronized: bool) {
        let clock_config: Option<&'static mut _> = unsafe { CLOCK_CONFIG.get().as_mut() };
        let clock_config: &'static mut _ = unsafe { clock_config.unwrap_unchecked() };

        clock_config.ntp_synchronized = ntp_synchronized;
    }

    pub fn configured(&self) -> bool {
        let clock_config: Option<&'static mut _> = unsafe { CLOCK_CONFIG.get().as_mut() };
        let clock_config: &'static mut _ = unsafe { clock_config.unwrap_unchecked() };
        clock_config.configured
    }

    pub fn datetime(&self) -> Result<OffsetDateTime, ComponentRange> {
        let offset = self.offset()?;

        let r = OffsetDateTime::from_unix_timestamp_nanos(self.get_time_us() as i128 * 1000)?
            .checked_to_offset(offset)
            // TODO:
            .unwrap();

        //trace!("datetime: {:?}", r);
        Ok(r)
    }
    
    
    /// Set the current value of the time registers in microseconds.
    pub fn set_time_us(&self, time_us: u64) {
        //trace!("set_time_us: {}", time_us);
        // Current time is boot time + time since boot (rtc time)
        // So boot time = current time - time since boot (rtc time)
        let rtc_time_us = self.get_rtc_time_us();
        if time_us < rtc_time_us {
            // An overflow would happen if we subtracted rtc_time_us from time_us.
            // To work around this, we can wrap around u64::MAX by subtracting the
            // difference between the current time and the time since boot.
            // Subtracting time since boot and adding current new time is equivalent and
            // avoids overflow. We just checked that rtc_time_us is less than time_us
            // so this won't overflow.
            self.set_boot_time_us(u64::MAX - rtc_time_us + time_us)
        } else {
            self.set_boot_time_us(time_us - rtc_time_us)
        }
    }

    pub fn update_boot(&self) {
        let now = self.get_time_us();
        self.set_time_us(now);
    }

    /// Read the current value of the time registers in microseconds.
    pub fn get_time_us(&self) -> u64 {
        // current time is boot time + time since boot
        let rtc_time_us = self.get_rtc_time_us();
        let boot_time_us = self.get_boot_time_us();
        let wrapped_boot_time_us = u64::MAX - boot_time_us;
        // We can detect if we wrapped the boot time by checking if rtc time is greater
        // than the amount of time we would've wrapped.
        let r = if rtc_time_us > wrapped_boot_time_us {
            // We also just checked that this won't overflow
            rtc_time_us - wrapped_boot_time_us
        } else {
            boot_time_us + rtc_time_us
        };
        //trace!("get_time_us: {}", r);
        r
    }

    /// Read the current raw value of the rtc time registers.
    ///
    /// **This function does not take into account the boot time registers, and
    /// therefore will not react to using [`set_time_us`][Self::set_time_us]
    /// and [`set_time_ms`][Self::set_time_ms].**
    pub fn get_rtc_time_raw(&self) -> u64 {
        let rtc_cntl = unsafe { &*peripherals::LPWR::ptr() };

        #[cfg(feature = "esp32")]
        let (l, h) = {
            rtc_cntl.time_update().write(|w| w.time_update().set_bit());
            while rtc_cntl.time_update().read().time_valid().bit_is_clear() {
                // might take 1 RTC slowclk period, don't flood RTC bus
                #[inline(always)]
                fn ets_delay_us(us: u32) {
                    extern "C" {
                        fn ets_delay_us(us: u32);
                    }

                    unsafe { ets_delay_us(us) };
                }

                // WHYDOBEPRIVATE?
                //esp_hal::rom::ets_delay_us(1);
                ets_delay_us(1);
                
            }
            let h = rtc_cntl.time1().read().time_hi().bits();
            let l = rtc_cntl.time0().read().time_lo().bits();
            (l, h)
        };
        #[cfg(any(feature = "esp32c2", feature = "esp32c3", feature = "esp32s2", feature = "esp32s3"))]
        let (l, h) = {
            rtc_cntl.time_update().write(|w| w.time_update().set_bit());
            let h = rtc_cntl.time_high0().read().timer_value0_high().bits();
            let l = rtc_cntl.time_low0().read().timer_value0_low().bits();
            (l, h)
        };
       
        let r = ((h as u64) << 32) | (l as u64);
        //trace!("get_rtc_time_raw: {}", r);
        r
    }

    pub fn get_cal_val(&self) -> u32 {
        let rtc_cntl = unsafe { &*peripherals::LPWR::ptr() };
        let r = rtc_cntl.store1().read().scratch1().bits();
        //debug!("get_cal_val: {}", r);
        r
    }

    pub fn get_rtc_time_cal_us(&self) -> u64 {
        // Not public in esp-hal 
        const CAL_FRACT: u32 = 19;

        let cal = self.calibration();
        let ticks = self.get_rtc_time_raw();

        let ticks_low = ticks & (u32::MAX as u64);
        let ticks_high = ticks >> 32;

        let cal_fract = CAL_FRACT as u64;

        let us = 
            // Low
            ((ticks_low * cal) >> cal_fract) 
            // High
            + ((ticks_high * cal) << (32 - cal_fract));

        //trace!("get_rtc_time_cal: cal: {}, ticks: {}, ticks_low: {}, ticks_high: {}, us: {}", cal, ticks, ticks_low, ticks_high, us);

        // 136 khz rtc crystal
        // 40 mhz main crystal

        us
    }

    /*
    /* RTC counter result is up to 2^48, calibration factor is up to 2^24,
     * for a 32kHz clock. We need to calculate (assuming no overflow):
     *   (ticks * cal) >> RTC_CLK_CAL_FRACT
     *
     * An overflow in the (ticks * cal) multiplication would cause time to
     * wrap around after approximately 13 days, which is probably not enough
     * for some applications.
     * Therefore multiplication is split into two terms, for the lower 32-bit
     * and the upper 16-bit parts of "ticks", i.e.:
     *   ((ticks_low + 2^32 * ticks_high) * cal) >> RTC_CLK_CAL_FRACT
     */
    const uint64_t ticks_low = ticks & UINT32_MAX;
    const uint64_t ticks_high = ticks >> 32;
    const uint64_t delta_time_us = ((ticks_low * cal) >> RTC_CLK_CAL_FRACT) +
                                   ((ticks_high * cal) << (32 - RTC_CLK_CAL_FRACT));
     */


}