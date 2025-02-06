use core::{
    cell::SyncUnsafeCell,
    ops::{Deref, DerefMut},
};

use embassy_executor::Spawner;
use embassy_net::{Config, DhcpConfig, Runner, Stack, StackResources, StaticConfigV4};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, signal::Signal};
use embassy_time::{Duration, Instant, Timer};
use esp_hal::{
    peripherals::{RADIO_CLK, WIFI},
    ram,
    rng::Rng,
    timer::timg::{TimerGroup, TimerGroupInstance},
};
use esp_wifi::{
    wifi::{
        AuthMethod, ClientConfiguration, Configuration, ScanConfig, WifiController, WifiDevice,
        WifiEvent, WifiStaDevice, WifiState,
    },
    EspWifiController,
};
use heapless::String;
use log::*;
use rand_core::RngCore;

use crate::{mk_static, rng::RngWrapper};

pub const STACK_SOCKET_COUNT: usize = 12;

#[ram(rtc_fast)]
static LAST_SSID: SyncUnsafeCell<Option<String<32>>> = SyncUnsafeCell::new(None);

#[ram(rtc_fast)]
static LAST_BSSID: SyncUnsafeCell<Option<[u8; 6]>> = SyncUnsafeCell::new(None);

#[ram(rtc_fast)]
static LAST_CHANNEL: SyncUnsafeCell<Option<u8>> = SyncUnsafeCell::new(None);

#[ram(rtc_fast)]
static LAST_AUTH_METHOD: SyncUnsafeCell<AuthMethod> = SyncUnsafeCell::new(AuthMethod::WPA2Personal);

#[ram(rtc_fast)]
static STATIC_IP_CONFIG: SyncUnsafeCell<Option<StaticConfigV4>> = SyncUnsafeCell::new(None);

// TODO: this should be some kind of duration based expiry but need easy way to get current time
const MAX_STATIC_IP_USES: usize = 10;
#[ram(rtc_fast)]
static STATIC_IP_USES: SyncUnsafeCell<usize> = SyncUnsafeCell::new(0);

// These stacks are types to prevent multiple instantiation of various resources. Similar to how pac
// peripherals are moved into things that take ownership of them
// TODO: the change to Stack so it's Copy has messed this up a bit. Clean it up or chuck it away
pub struct UdpStack {
    stack: Stack<'static>,
}
impl UdpStack {
    pub fn stack(&self) -> Stack<'static> {
        self.stack
    }
}
pub struct NtpStack {
    stack: Stack<'static>,
}
impl NtpStack {
    pub fn stack(&self) -> Stack<'static> {
        self.stack
    }
}
pub struct TcpStack {
    stack: Stack<'static>,
}
impl TcpStack {
    pub fn stack(&self) -> Stack<'static> {
        self.stack
    }
}

pub struct Stacks {
    pub udp: UdpStack,
    pub ntp: NtpStack,
    pub tcp: TcpStack,
}

pub struct WifiHandle {
    stack: Stack<'static>,
    shutdown:
        &'static Signal<CriticalSectionRawMutex, &'static Signal<CriticalSectionRawMutex, ()>>,
    restart: &'static Signal<CriticalSectionRawMutex, ()>,
}

impl WifiHandle {
    pub fn shutdown_signal(
        &self,
    ) -> &'static Signal<CriticalSectionRawMutex, &'static Signal<CriticalSectionRawMutex, ()>>
    {
        &self.shutdown
    }

    pub fn restart_signal(&self) -> &'static Signal<CriticalSectionRawMutex, ()> {
        &self.restart
    }
}

pub struct WifiPendingHandle(WifiHandle);
impl Deref for WifiPendingHandle {
    type Target = WifiHandle;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl DerefMut for WifiPendingHandle {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl From<Stack<'static>> for WifiPendingHandle {
    fn from(stack: Stack<'static>) -> Self {
        static SHUTDOWN: Signal<
            CriticalSectionRawMutex,
            &'static Signal<CriticalSectionRawMutex, ()>,
        > = Signal::new();
        static RESTART: Signal<CriticalSectionRawMutex, ()> = Signal::new();
        let handle = WifiHandle { stack, shutdown: &SHUTDOWN, restart: &RESTART };
        Self(handle)
    }
}

impl WifiPendingHandle {
    pub async fn wait_for_connection<'a>(self) -> (WifiConnectedHandle, Stacks) {
        loop {
            if self.stack.is_link_up() {
                break;
            }
            debug!("Wait for network link");
            Timer::after(Duration::from_millis(1000)).await;
        }

        loop {
            if let Some(config) = self.stack.config_v4() {
                info!("Connected to WiFi with IP address {}", config.address);

                let static_ip = unsafe { STATIC_IP_CONFIG.get().as_mut().unwrap_unchecked() };
                let static_ip_uses = unsafe { STATIC_IP_USES.get().as_mut().unwrap_unchecked() };
                if static_ip.is_none() {
                    *static_ip = Some(config);
                    *static_ip_uses = 0;
                }
                break;
            }
            debug!("Wait for IP address");
            Timer::after(Duration::from_millis(1000)).await;
        }

        let handle = WifiConnectedHandle::from(self);

        let stacks = Stacks {
            udp: UdpStack { stack: handle.stack },
            ntp: NtpStack { stack: handle.stack },
            tcp: TcpStack { stack: handle.stack },
        };

        (handle, stacks)
    }
}

pub struct WifiConnectedHandle(WifiHandle);
impl Deref for WifiConnectedHandle {
    type Target = WifiHandle;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl DerefMut for WifiConnectedHandle {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl From<WifiPendingHandle> for WifiConnectedHandle {
    fn from(value: WifiPendingHandle) -> Self {
        Self(value.0)
    }
}
impl WifiConnectedHandle {
    pub async fn shutdown(self) {
        shutdown_wifi(self.shutdown).await;
    }

    // 'a lifetime prevents using the wifi stack after shutdown has been called
    pub fn stack<'a>(&'a self) -> &'a Stack<'static> {
        &self.stack
    }
}

pub async fn shutdown_wifi(
    signal: &'static Signal<CriticalSectionRawMutex, &'static Signal<CriticalSectionRawMutex, ()>>,
) {
    static SHUTDOWN_COMPLETE: Signal<CriticalSectionRawMutex, ()> = Signal::new();
    signal.signal(&SHUTDOWN_COMPLETE);

    SHUTDOWN_COMPLETE.wait().await
}

/// Connect to WiFi
pub fn connect<T: TimerGroupInstance>(
    spawner: &Spawner,
    rng: Rng,
    timer: TimerGroup<T>,
    wifi: WIFI,
    radio_clock_control: RADIO_CLK,
    // clocks: &Clocks<'_>,
    (ssid, password): (String<32>, String<64>),
) -> Result<WifiPendingHandle, WifiError> {
    let mut rng_wrapper = RngWrapper::from(rng.clone());
    let seed = rng_wrapper.next_u64();
    debug!("Use random seed 0x{:016x}", seed);

    let init = &*mk_static!(
        EspWifiController<'static>,
        esp_wifi::init(timer.timer0, rng.clone(), radio_clock_control)?
    );

    let last_ssid = unsafe { LAST_SSID.get().as_mut().unwrap_unchecked() };
    let last_bssid = unsafe { LAST_BSSID.get().as_mut().unwrap_unchecked() };
    let last_channel = unsafe { LAST_CHANNEL.get().as_mut().unwrap_unchecked() };
    let last_auth_method = unsafe { LAST_AUTH_METHOD.get().as_mut().unwrap_unchecked() };

    if last_ssid.as_deref() != Some(ssid.as_str()) {
        *last_bssid = None;
    }

    let (wifi_interface, controller) = if last_bssid.is_some() {
        let config = ClientConfiguration {
            ssid: ssid.clone(),
            password: password.clone(),
            bssid: *last_bssid,
            channel: *last_channel,
            auth_method: *last_auth_method,
        };
        debug!("Using new_with_config");
        esp_wifi::wifi::new_with_config(&init, wifi, config)?
    } else {
        debug!("Using new_with_mode");
        esp_wifi::wifi::new_with_mode(&init, wifi, WifiStaDevice)?
    };

    let static_ip = unsafe { STATIC_IP_CONFIG.get().as_mut().unwrap_unchecked() };
    let static_ip_uses = unsafe { STATIC_IP_USES.get().as_mut().unwrap_unchecked() };

    let config = if static_ip.is_some() && *static_ip_uses < MAX_STATIC_IP_USES {
        *static_ip_uses += 1;
        debug!("Using static IP");
        Config::ipv4_static(static_ip.as_ref().unwrap().clone())
    } else {
        debug!("Using DHCP");
        Config::dhcpv4(DhcpConfig::default())
    };

    debug!("Initialize network stack");
    // Init network stack
    let (stack, runner) = embassy_net::new(
        wifi_interface,
        config,
        mk_static!(StackResources<STACK_SOCKET_COUNT>, StackResources::new()),
        seed,
    );
    debug!("Initialize network stack done");

    spawner.must_spawn(net_task(runner));

    let handle = WifiPendingHandle::from(stack);
    spawner.must_spawn(connection(
        controller,
        ssid,
        password,
        handle.shutdown_signal(),
        handle.restart_signal(),
    ));

    Ok(handle)
}

/// Task for ongoing network processing
#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, WifiDevice<'static, WifiStaDevice>>) {
    debug!("Net task spawned");
    runner.run().await
}

/// Task for WiFi connection
///
/// This will wrap [`connection_fallible()`] and trap any error.
#[embassy_executor::task]
async fn connection(
    controller: WifiController<'static>,
    ssid: String<32>,
    password: String<64>,
    shutdown_signal: &'static Signal<
        CriticalSectionRawMutex,
        &'static Signal<CriticalSectionRawMutex, ()>,
    >,
    restart_signal: &'static Signal<CriticalSectionRawMutex, ()>,
) {
    trace!("Connection task spawned");

    let Err(error) =
        connection_fallible(controller, ssid, password, shutdown_signal, restart_signal).await;
    error!("Cannot connect to WiFi: {:?}", error);
}

/// Fallible task for WiFi connection
async fn connection_fallible(
    mut controller: WifiController<'static>,
    ssid: String<32>,
    password: String<64>,
    shutdown_signal: &'static Signal<
        CriticalSectionRawMutex,
        &'static Signal<CriticalSectionRawMutex, ()>,
    >,
    restart_signal: &'static Signal<CriticalSectionRawMutex, ()>,
) -> Result<!, WifiError> {
    trace!("Start connection");
    trace!("Device capabilities: {:?}", controller.capabilities());

    let start = Instant::now();

    let last_ssid = unsafe { LAST_SSID.get().as_mut().unwrap_unchecked() };
    let last_bssid = unsafe { LAST_BSSID.get().as_mut().unwrap_unchecked() };
    let last_channel = unsafe { LAST_CHANNEL.get().as_mut().unwrap_unchecked() };
    let last_auth_method = unsafe { LAST_AUTH_METHOD.get().as_mut().unwrap_unchecked() };

    if let Some(lssid) = last_ssid {
        if lssid != ssid.as_str() {
            // Clear the cached values if they are for a different ssid
            *last_bssid = None;
            *last_channel = None;
            *last_auth_method = AuthMethod::WPA2Personal;
        } else {
            trace!("Reusing last_bssid");
        }
    }
    *last_ssid = Some(ssid.clone());

    loop {
        if esp_wifi::wifi::wifi_state() == WifiState::StaConnected {
            // wait until we're no longer connected
            trace!("Disconnecting previous connection");
            controller.wait_for_event(WifiEvent::StaDisconnected).await;
            Timer::after(Duration::from_millis(5000)).await;
        }

        if last_bssid.is_none() {
            if !matches!(controller.is_started(), Ok(true)) {
                trace!("Starting WiFi controller");
                controller.start_async().await?;
                trace!("WiFi controller started");
            }

            while matches!(controller.is_started(), Ok(false)) {
                Timer::after_micros(10).await;
            }

            trace!("Scanning for {ssid}");
            let (aps, n) = controller
                .scan_with_config_async::<1>(ScanConfig {
                    ssid: Some(ssid.as_str()),
                    ..Default::default()
                })
                .await?;
            trace!("Scan complete: {} results", n);

            if aps.len() >= 1 {
                let ap = &aps[0];
                trace!("Scan found AP: {ap:?}");
                *last_bssid = Some(ap.bssid);

                *last_channel = Some(ap.channel);
                if let Some(am) = ap.auth_method {
                    *last_auth_method = am;
                }
                controller.stop_async().await?;
                Timer::after_micros(100).await;
            } else {
                Timer::after_micros(100).await;
                continue;
            }
        }

        let mut client_config = ClientConfiguration {
            ssid: ssid.clone(),
            password: password.clone(),
            ..Default::default()
        };

        client_config.bssid = *last_bssid;
        client_config.channel = *last_channel;
        client_config.auth_method = *last_auth_method;

        let client_config = Configuration::Client(client_config);
        controller.set_configuration(&client_config)?;

        if !matches!(controller.is_started(), Ok(true)) {
            controller.start_async().await?;
        }

        trace!("Connect to WiFi network");
        match controller.connect_async().await {
            Ok(()) => {
                let elapsed = start.elapsed();

                trace!(
                    "Connected to WiFi network. Took {:.2}s",
                    elapsed.as_micros() as f32 / 1_000_000.
                );

                trace!("Wait for request to stop wifi");
                let complete = shutdown_signal.wait().await;
                trace!("Received signal to stop wifi");
                controller.stop_async().await?;
                complete.signal(());
                trace!("WiFi stopped");

                restart_signal.wait().await;
            },
            Err(error) => {
                error!("Failed to connect to WiFi network: {:?}", error);
                Timer::after(Duration::from_millis(500)).await;
            },
        }
    }

    // trace!("Leave connection task");
    // Ok(())
}

#[allow(unused)]
#[derive(Debug)]
pub enum WifiError {
    WifiInitialization(esp_wifi::InitializationError),
    Wifi(esp_wifi::wifi::WifiError),
}

impl From<esp_wifi::InitializationError> for WifiError {
    fn from(value: esp_wifi::InitializationError) -> Self {
        Self::WifiInitialization(value)
    }
}

impl From<esp_wifi::wifi::WifiError> for WifiError {
    fn from(value: esp_wifi::wifi::WifiError) -> Self {
        Self::Wifi(value)
    }
}
