#![no_std]
#![no_main]
#![feature(sync_unsafe_cell)]
#![feature(impl_trait_in_assoc_type)]

use core::{convert::Infallible, fmt::Write, num::ParseIntError, str::Utf8Error};

use blind_controller::{http, logging, nvs::{self, Nvs, MIN_OFFSET}, ota::{self, Ota}, partitions::{ NVS_PARTITION, OTA_0_PARTITION, OTA_1_PARTITION}, wifi::{self, PASSWORD_LEN, SSID_MAX_LEN}};
use const_format::concatcp;
use embassy_executor::Spawner;
use embassy_sync::{
    blocking_mutex::raw::CriticalSectionRawMutex,
    channel::{Channel, Receiver, Sender},
};
use embassy_time::{Duration, Timer};
use esp_alloc::heap_allocator;
// For panic-handler
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    gpio::{Level, Output},
    peripherals::Peripherals,
    reset::{reset_reason, wakeup_cause},
    rng::Rng,
    rtc_cntl::SocResetReason,
    timer::timg::TimerGroup,
    Config as HalConfig,
};
use esp_hal_embassy::main;
use esp_storage::FlashStorage;
use heapless::{String, Vec};
use log::*;
use picoserve::{
    routing::{get, parse_path_segment},
    AppBuilder, AppRouter,
};

const HEAP_MEMORY_SIZE: usize = 72 * 1024;

// -3 is to reserve some for outgoing socket requests
const WEB_TASK_POOL_SIZE: usize = wifi::STACK_SOCKET_COUNT - 3;

const SSID: Option<&str> = option_env!("SSID");
const PASSWORD: Option<&str> = option_env!("PASSWORD");
const BUILD_DATE: i64 = { match i64::from_str_radix(env!("BUILD_DATE"), 10) {
    Ok(v) => v,
    Err(_) => panic!("BUILD_DATE env variable failed to parse as i64"),
}};
const TARGET_TRIPLE: &str = env!("TARGET_TRIPLE");
const RELEASE_URL: &str = "https://github.com/cs2dsb/blind_controller.rs/releases/latest/download/";
const BUILD_DATE_URL: &str = concatcp!(RELEASE_URL, "BUILD_DATE_", TARGET_TRIPLE); 

const BLIND_HEIGHT: usize = 4450;

macro_rules! mk_static {
    ($t:ty, $val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        #[deny(unused_attributes)]
        let x = STATIC_CELL.uninit().write(($val));
        x
    }};
}

#[derive(Debug)]
enum StepCommand {
    Forward(usize),
    Backward(usize),
    Raise,
    Lower,
}

#[embassy_executor::task(pool_size = WEB_TASK_POOL_SIZE)]
async fn web_task(
    id: usize,
    stack: embassy_net::Stack<'static>,
    app: &'static AppRouter<AppProps>,
    config: &'static picoserve::Config<Duration>,
) -> ! {
    let port = 80;
    let mut tcp_rx_buffer = [0; 1024];
    let mut tcp_tx_buffer = [0; 1024];
    let mut http_buffer = [0; 2048];

    picoserve::listen_and_serve(
        id,
        app,
        config,
        stack,
        port,
        &mut tcp_rx_buffer,
        &mut tcp_tx_buffer,
        &mut http_buffer,
    )
    .await
}

struct AppProps {
    sender: Sender<'static, CriticalSectionRawMutex, StepCommand, 10>,
}

impl AppBuilder for AppProps {
    type PathRouter = impl picoserve::routing::PathRouter;

    fn build_app(self) -> picoserve::Router<Self::PathRouter> {
        let Self { sender } = self;
        picoserve::Router::new()
            .route(
                ("/forward", parse_path_segment::<usize>()),
                get(move |n| async move {
                    sender.send(StepCommand::Forward(n)).await;
                    let mut buf = String::<64>::new();
                    let _ = write!(&mut buf, "Forward {n}");
                    buf
                }),
            )
            .route(
                ("/backward", parse_path_segment::<usize>()),
                get(move |n| async move {
                    sender.send(StepCommand::Backward(n)).await;
                    let mut buf = String::<64>::new();
                    let _ = write!(&mut buf, "Backwards {n}");
                    buf
                }),
            )
            .route(
                "/raise",
                get(move || async move {
                    sender.send(StepCommand::Raise).await;
                    "Raise"
                }),
            )
            .route(
                "/lower",
                get(move || async move {
                    sender.send(StepCommand::Lower).await;
                    "Lower"
                }),
            )
    }
}

#[embassy_executor::task]
async fn motor_task(
    receiver: Receiver<'static, CriticalSectionRawMutex, StepCommand, 10>,
    mut tmc_en: Output<'static>,
    mut tmc_step: Output<'static>,
    mut tmc_dir: Output<'static>,
    microsteps: usize,
) -> ! {
    let mut raised = true;
    loop {
        let msg = receiver.receive().await;
        debug!("Step command: {:?}", msg);

        match (raised, &msg) {
            (true, StepCommand::Raise) => {
                warn!("Already raised, skipping raise command");
                continue;
            },
            (false, StepCommand::Lower) => {
                warn!("Already lowered, skipping lower command");
                continue;
            },
            (false, StepCommand::Raise) | (true, StepCommand::Lower) => {
                raised = !raised;
            },
            _ => {},
        }

        let (dir, n) = match msg {
            StepCommand::Forward(n) => (Level::High, n),
            StepCommand::Backward(n) => (Level::Low, n),
            StepCommand::Raise => (Level::High, BLIND_HEIGHT),
            StepCommand::Lower => (Level::Low, BLIND_HEIGHT),
        };

        let n = n * microsteps;

        tmc_dir.set_level(dir);
        tmc_en.set_low();
        let fast = 1500;
        let slow = 5000;
        for i in 0..n {
            let speed = if i < 100 || i >= n - 100 { slow } else { fast };
            tmc_step.toggle();
            Timer::after(Duration::from_micros(speed)).await;
        }
        tmc_en.set_high();

        debug!("Stepping done")
    }
}

#[main]
async fn main(spawner: Spawner) {
    logging::setup();

    let peripherals = esp_hal::init({
        let mut c = HalConfig::default();
        c.cpu_clock = CpuClock::max();
        c
    });
    trace!("hal::init done");

    heap_allocator!(HEAP_MEMORY_SIZE);

    if let Err(error) = main_fallible(&spawner, peripherals).await {
        error!("Error while running firmware: {:?}", error);

        // info!("Sleeping");
        // let rtc = Rtc::new(unsafe { LPWR::steal()});
        // enter_deep_sleep(
        //     rtc,
        //     Duration::from_secs(10).into(),
        // );
    }
}

async fn main_fallible(spawner: &Spawner, peripherals: Peripherals) -> Result<(), Error> {
    let reset_reason = reset_reason().unwrap_or(SocResetReason::ChipPowerOn);
    let wake_reason = wakeup_cause();
    info!("Reset reason: {reset_reason:?}, Wake reason: {wake_reason:?}");

    debug!("BUILD_DATE: {BUILD_DATE}");

    debug!("NVS partition: {NVS_PARTITION:?}");
    debug!("OTA_0 partition: {OTA_0_PARTITION:?}");
    debug!("OTA_1 partition: {OTA_1_PARTITION:?}");

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let rng = Rng::new(peripherals.RNG);

    cfg_if::cfg_if! {
        if #[cfg(feature = "esp32")] {
            let timg1 = TimerGroup::new(peripherals.TIMG1);
            esp_hal_embassy::init(timg1.timer0);
        } else {
            use esp_hal::timer::systimer::SystemTimer;
            let systimer = SystemTimer::new(peripherals.SYSTIMER);
            esp_hal_embassy::init(systimer.alarm0);
        }
    }
    trace!("esp_hal_embassy::init done");

    let mut flash = FlashStorage::new();
    let (ssid, password) = { // Block to drop nvs
        let mut nvs = Nvs::new(&mut flash);

        if let (Some(ssid), Some(password)) = (SSID, PASSWORD) {
            let ssid = String::<SSID_MAX_LEN>::try_from(ssid).map_err(|()| Error::ParseCredentials)?;
            let password = String::<PASSWORD_LEN>::try_from(password).map_err(|()| Error::ParseCredentials)?;

            debug!("Using SSID and password embedded in binary");

            nvs.set_valid(false)?;
            
            let ssid_len = [ssid.len() as u8];
            let ssid_bytes = ssid.as_bytes();

            let password_len = [password.len() as u8];
            let password_bytes = password.as_bytes();

            nvs.write(MIN_OFFSET, &ssid_len)?;
            nvs.write(MIN_OFFSET + 1, ssid_bytes)?;

            nvs.write(MIN_OFFSET + 1 + SSID_MAX_LEN as u32, &password_len)?;
            nvs.write(MIN_OFFSET + 1 + SSID_MAX_LEN as u32 + 1, password_bytes)?;

            nvs.set_valid(true)?;

            debug!("SSID and password saved to NVS");

            (ssid, password)
        } else {
            if nvs.is_valid()? {
                debug!("Using SSID and password from NVS");

                let mut ssid = Vec::<u8, SSID_MAX_LEN>::new();
                let mut password = Vec::<u8, PASSWORD_LEN>::new();

                let mut len = [0_u8];

                nvs.read(MIN_OFFSET, &mut len)?;
                ssid.resize(len[0] as usize, 0).map_err(|()| Error::ParseCredentials)?;
                nvs.read(MIN_OFFSET + 1, ssid.as_mut_slice())?;
        
                nvs.read(MIN_OFFSET + 1 + SSID_MAX_LEN as u32, &mut len)?;
                password.resize(len[0] as usize, 0).map_err(|()| Error::ParseCredentials)?;
                nvs.read(MIN_OFFSET + 1 + SSID_MAX_LEN as u32 + 1, password.as_mut_slice())?;

                let ssid = match String::from_utf8(ssid) {
                    Ok(str) => Some(str),
                    Err(_) => {
                        error!("SSID from NVS was invalid utf8");
                        None
                    },
                };

                let password = match String::from_utf8(password) {
                    Ok(str) => Some(str),
                    Err(_) => {
                        error!("Password from NVS was invalid utf8");
                        None
                    },
                };

                if ssid.is_none() || password.is_none() {
                    nvs.set_valid(false)?;
                    Err(Error::ParseCredentials)?;
                }

                debug!("NVS credentials valid");
                
                (ssid.unwrap(), password.unwrap())
            } else {
                Err(Error::MissingCredentials)?
            }
        }
    };

    let pending_handle = wifi::connect(
        &spawner,
        rng,
        timg0,
        peripherals.WIFI,
        peripherals.RADIO_CLK,
        (ssid, password),
    )?;
    let (_handle, stacks) = pending_handle.wait_for_connection().await;
    trace!("Connected");

    let stack = stacks.tcp.stack();

    let mut http_client = http::Client::new(stack, rng);
    let build_date = {
        let bytes = http_client.req::<20, _>(BUILD_DATE_URL).await?;
        let string = String::from_utf8(bytes)?;
        
        i64::from_str_radix(&string, 10)?
    };
    debug!("Build dates. Local: {BUILD_DATE}, remote: {build_date}");
    if build_date > BUILD_DATE {
        info!("Update available!")
    } else {
        info!("No update available");
    }

    loop { Timer::after_secs(10).await }

    #[allow(unreachable_code)]
    let mut _ota = Ota::new(&mut flash);
    // ota.read_select_entries()?;
    // loop { Timer::after_secs(10).await }

    let channel = mk_static!(Channel::<CriticalSectionRawMutex, StepCommand, 10>, Channel::new());
    let sender = channel.sender();
    let receiver = channel.receiver();

    let tmc_step = Output::new(peripherals.GPIO32, Level::High);
    let tmc_dir = Output::new(peripherals.GPIO33, Level::High);
    // en is active low
    let tmc_en = Output::new(peripherals.GPIO25, Level::High);

    // Microstep config
    // Low, Low = 8
    // High, Low = 2
    // Low, High = 4
    let _tmc_ms1 = Output::new(peripherals.GPIO27, Level::High);
    let _tmc_ms2 = Output::new(peripherals.GPIO26, Level::Low);

    spawner.must_spawn(motor_task(receiver, tmc_en, tmc_step, tmc_dir, 2));

    let app = &*mk_static!(AppRouter<AppProps>, AppProps { sender }.build_app());
    let config = &*mk_static!(
        picoserve::Config<Duration>,
        picoserve::Config::new(picoserve::Timeouts {
            start_read_request: Some(Duration::from_secs(5)),
            read_request: Some(Duration::from_secs(1)),
            write: Some(Duration::from_secs(1)),
        })
        .keep_connection_alive()
    );

    let stack = stacks.tcp.stack();
    // Spawn handler tasks
    for id in 0..WEB_TASK_POOL_SIZE {
        // TODO: stacks refactor
        spawner.must_spawn(web_task(id, stack, app, config));
    }

    return Ok(());
}

#[allow(unused)]
#[derive(Debug)]
pub enum Error {
    /// An impossible error existing only to satisfy the type system
    Impossible(Infallible),

    /// Error while parsing SSID or password
    ParseCredentials,
    MissingCredentials,

    Wifi(wifi::WifiError),

    Nvs(nvs::Error),
    Ota(ota::Error),

    Http(http::Error),

    Utf8Error(Utf8Error),

    ParseIntError(ParseIntError),

    Timeout(&'static str),
}

impl From<Infallible> for Error {
    fn from(value: Infallible) -> Self {
        Self::Impossible(value)
    }
}

impl From<wifi::WifiError> for Error {
    fn from(value: wifi::WifiError) -> Self {
        Self::Wifi(value)
    }
}

impl From<nvs::Error> for Error {
    fn from(value: nvs::Error) -> Self {
        Self::Nvs(value)
    }
}

impl From<ota::Error> for Error {
    fn from(value: ota::Error) -> Self {
        Self::Ota(value)
    }
}

impl From<http::Error> for Error {
    fn from(value: http::Error) -> Self {
        Self::Http(value)
    }
}

impl From<Utf8Error> for Error {
    fn from(value: Utf8Error) -> Self {
        Self::Utf8Error(value)
    }
}

impl From<ParseIntError> for Error {
    fn from(value: ParseIntError) -> Self {
        Self::ParseIntError(value)
    }
}
