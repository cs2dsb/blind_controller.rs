#![no_std]
#![no_main]
#![feature(sync_unsafe_cell)]
#![feature(impl_trait_in_assoc_type)]

use core::{convert::Infallible, fmt::Write, num::ParseIntError, str::Utf8Error};
use embedded_io_async::{Read, Write as EioWrite};
use blind_controller::{http::{self, CallbackError}, logging, ntp, nvs::{self, Nvs, MIN_OFFSET}, ota::{self, Ota}, partitions::{ NVS_PARTITION, OTA_0_PARTITION, OTA_1_PARTITION}, rtc::enter_deep as enter_deep_sleep, system_time::SystemTime, wifi::{self, PASSWORD_LEN, SSID_MAX_LEN}};
use chrono::{NaiveDate, TimeZone, Timelike};
use const_format::concatcp;
use embassy_executor::Spawner;
use embassy_sync::{
    blocking_mutex::raw::CriticalSectionRawMutex,
    channel::{Channel, Receiver, Sender},
};
use embassy_time::{Duration, Timer};
// For panic-handler
use esp_backtrace as _;
use esp_hal::{
    clock::CpuClock,
    gpio::{Level, Output},
    peripherals::{Peripherals, LPWR},
    reset::{reset_reason, wakeup_cause},
    rng::Rng,
    rtc_cntl::{Rtc, SocResetReason},
    timer::timg::TimerGroup,
    Config as HalConfig,
};
use esp_hal_embassy::main;
use esp_storage::FlashStorage;
use heapless::{String, Vec};
use log::*;
use picoserve::{
    response::{Content, IntoResponse, ResponseWriter}, routing::{get, parse_path_segment}, AppBuilder, AppRouter
};
use embassy_sync::mutex::Mutex;
use sunrise::{Coordinates, SolarDay, SolarEvent};
use time::{error::ComponentRange, Date, Month, OffsetDateTime, Time, UtcOffset, Weekday};

static UPDATE_PENDING: Mutex<CriticalSectionRawMutex, bool> = Mutex::new(false);

const HEAP_MEMORY_SIZE: usize =  72 * 1024;

// TODO: bump to latest version of esp-alloc which supports passing the link section into it's macro
#[link_section = ".dram2_uninit"] 
static mut HEAP: core::mem::MaybeUninit<[u8; HEAP_MEMORY_SIZE]> = core::mem::MaybeUninit::uninit();

#[allow(static_mut_refs)]
fn init_heap() {
    unsafe {
        esp_alloc::HEAP.add_region(esp_alloc::HeapRegion::new(
            HEAP.as_mut_ptr() as *mut u8,
            HEAP_MEMORY_SIZE,
            esp_alloc::MemoryCapability::Internal.into(),
        ));
    }
}

// -3 is to reserve some for outgoing socket requests
const WEB_TASK_POOL_SIZE: usize = 2; // wifi::STACK_SOCKET_COUNT - 3;

const SSID: Option<&str> = option_env!("SSID");
const PASSWORD: Option<&str> = option_env!("PASSWORD");
const NTP_SERVER: &str = env!("NTP_SERVER");

const BUILD_DATE: i64 = { match i64::from_str_radix(env!("BUILD_DATE"), 10) {
    Ok(v) => v,
    Err(_) => panic!("BUILD_DATE env variable failed to parse as i64"),
}};
const TARGET_TRIPLE: &str = env!("TARGET_TRIPLE");
const RELEASE_URL: &str = "https://github.com/cs2dsb/blind_controller.rs/releases/latest/download/";
const BUILD_DATE_URL: &str = concatcp!(RELEASE_URL, "BUILD_DATE_", TARGET_TRIPLE); 
const FIRMWARE_URL: &str = concatcp!(RELEASE_URL, "blind_", TARGET_TRIPLE); 

const LATITUDE: &str = env!("LATITUDE");
const LONGITUDE: &str = env!("LONGITUDE");
// const LAT_LONG: Coordinates = { match }

const BLIND_HEIGHT: usize = 4450;

macro_rules! mk_static {
    ($t:ty, $val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        #[deny(unused_attributes)]
        let x = STATIC_CELL.uninit().write(($val));
        x
    }};
}

#[derive(Debug, Clone, Copy, PartialEq)]
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
    coordinates: Coordinates,
}

struct Html<const LEN: usize> (String<LEN>);

impl<const LEN: usize> Content for Html<LEN> {
    fn content_type(&self) -> &'static str {
        "text/html; charset=utf-8"
    }

    fn content_length(&self) -> usize {
        self.0.len()
    }

    async fn write_content<W: EioWrite>(self, writer: W) -> Result<(), W::Error> {
        self.0.as_bytes().write_content(writer).await
    }
}

impl AppProps {
    async fn index() -> impl IntoResponse {
        let build_date = chrono::Utc.timestamp_millis_opt(BUILD_DATE).single().expect("Invalid build date in binary");
        let update_pending = *UPDATE_PENDING.lock().await;
        let mut buf = String::<150>::new();
        let _ = write!(&mut buf, "<p>Build date: {build_date:?}</p><p>Update pending: {update_pending:?}</p><a href='/reboot'>Reboot</a>");
        Html(buf)
    }
    async fn time(coordinates: Coordinates) -> impl IntoResponse {
        let st = SystemTime {};
        let configured = st.configured();
        let mut set = true;
        let time = st.datetime().unwrap_or_else(|_| {
            set = false;
            OffsetDateTime::UNIX_EPOCH
        });
        let sunset = calculate_sunset(&time, coordinates);

        let mut buf = String::<150>::new();
        let _ = write!(&mut buf, "<p>Configured: {configured:?}</p><p>Time set: {set:?}</p><p>Time: {time:?}</p><p>Sunset: {sunset:?}</p>");
        Html(buf)
    }
    // TODO: have the handler finish and get an embassy task to actually reboot or something
    #[allow(dependency_on_unit_never_type_fallback)]
    async fn reboot() -> impl IntoResponse {
        let rtc = Rtc::new(unsafe { LPWR::steal()});
         enter_deep_sleep(
             rtc,
             Duration::from_secs(2).into(),
         );
    }
}

struct NoStoreResponseWriter<W> {
    response_writer: W,
}

impl<W: ResponseWriter> ResponseWriter for NoStoreResponseWriter<W> {
    type Error = W::Error;

    async fn write_response<
        R: Read<Error = Self::Error>,
        H: picoserve::response::HeadersIter,
        B: picoserve::response::Body,
    >(
        self,
        connection: picoserve::response::Connection<'_, R>,
        response: picoserve::response::Response<H, B>,
    ) -> Result<picoserve::ResponseSent, Self::Error> {
        let response = response.with_header("Cache-Control", "no-store");

        self
            .response_writer
            .write_response(connection, response)
            .await
    }
}

struct NoStoreLayer;

impl<State, PathParameters> picoserve::routing::Layer<State, PathParameters> for NoStoreLayer {
    type NextState = State;
    type NextPathParameters = PathParameters;

    async fn call_layer<
        'a,
        R: Read + 'a,
        NextLayer: picoserve::routing::Next<'a, R, Self::NextState, Self::NextPathParameters>,
        W: ResponseWriter<Error = R::Error>,
    >(
        &self,
        next: NextLayer,
        state: &State,
        path_parameters: PathParameters,
        request_parts: picoserve::request::RequestParts<'_>,
        response_writer: W,
    ) -> Result<picoserve::ResponseSent, W::Error> {
        next.run(
            state,
            path_parameters,
            NoStoreResponseWriter { response_writer },
        )
        .await
    }
}

impl AppBuilder for AppProps {
    type PathRouter = impl picoserve::routing::PathRouter;

    fn build_app(self) -> picoserve::Router<Self::PathRouter> {
        let Self { sender, coordinates } = self;
        picoserve::Router::new()
            .route("/", get(|| Self::index()))
            .route("/time", get(move || Self::time(coordinates)))
            .route("/reboot", get(|| Self::reboot()))
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
            .layer(NoStoreLayer)
    }
}

fn calculate_sunset(datetime: &OffsetDateTime, coordinates: Coordinates) -> Result<OffsetDateTime, Error> {
    let date = NaiveDate::from_ymd_opt(datetime.year(), datetime.month() as u32, datetime.day() as u32)
        .ok_or(Error::Other("Failed to convert time::Date into chrono::NaiveDate"))?;
    let day = SolarDay::new(coordinates, date);
    let sunset = day.event_time(SolarEvent::Sunset);
    let offset_sunset = datetime
        .replace_hour(sunset.hour() as u8)?
        .replace_minute(sunset.minute() as u8)?
        .replace_second(sunset.second() as u8)?;

    debug!("Sunset is at {offset_sunset}");

    Ok(offset_sunset)
}

#[embassy_executor::task]
async fn schedule_task(sender: Sender<'static, CriticalSectionRawMutex, StepCommand, 10>, coordinates: Coordinates) -> ! {
    let system_time = SystemTime {};

    while !system_time.ntp_synchronized() {
        debug!("schedule_task awaiting ntp sync");
        Timer::after(Duration::from_secs(2)).await;
    }

    let raise = Time::from_hms(12, 30, 0).unwrap();
    let mut state = None;

    loop {
        let state = &mut state;
        let r: Result<(), Error> = async {
            let datetime = system_time.datetime()?;
            
            let sunset = if state == &Some(StepCommand::Raise) {
                Some(calculate_sunset(&datetime, coordinates)?)
            } else {
                None
            };

            let time = datetime.time();

            let action = match (&state, sunset.map(|v| v.time() <= time), time >= raise) {
                (None | Some(StepCommand::Raise), Some(true), _) => {
                    Some(StepCommand::Lower)
                },
                (None | Some(StepCommand::Lower), None | Some(false), true) => {
                    Some(StepCommand::Raise)
                },
                (None, None | Some(false), false) => {
                    // If it's before the raise time it means it is the following day past the lower time
                    Some(StepCommand::Lower)
                },
                _ => None
            };

            if let Some(action) = action {
                *state = Some(action);
                info!("schedule_task sending command: {action:?}");
                sender.send(action).await;
            }

            Ok(())
        }.await;

        if let Err(e) = r {
            error!("schedule_task error: {e:?}");
        }

        debug!("schedule_task sleeping");
        Timer::after(Duration::from_secs(60)).await;
    }
}

#[embassy_executor::task]
async fn ntp_task(mut client: ntp::Client, mut system_time: SystemTime) -> ! {
    let mut first_run = !system_time.configured();
    
    loop {
        let r: Result<(), Error> = async {
            debug!("ntp_task sending request");
            let (_, offset) = client.ntp_request(&mut system_time, !first_run).await?;
            first_run = false;
            debug!("NTP updated. Offset = {offset}");

            // In the UK the clocks go forward 1 hour at 1am on the last Sunday in March, and back 1 hour at 2am on the last Sunday in October. 
            let datetime = system_time.datetime()?;
            let year = datetime.year();

            let forward = Date::from_calendar_date(year, Month::April, 1)?
                .nth_prev_occurrence(Weekday::Sunday, 1);
            let backward = Date::from_calendar_date(year, Month::November, 1)?
                .nth_prev_occurrence(Weekday::Sunday, 1);

            debug!("Forward: {forward:?}, Backward: {backward}");
            
            let forward = OffsetDateTime::new_utc(forward, Time::from_hms(1, 0, 0)?);
            let backward = OffsetDateTime::new_utc(backward, Time::from_hms(2, 0, 0)?);

            let current_offset = system_time.offset()?;
           
            let change = if current_offset.is_positive() && datetime >= backward {
                Some(UtcOffset::UTC)
            } else if current_offset.is_utc() && datetime >= forward {
                Some(UtcOffset::from_hms(1, 0, 0)?)
            } else {
                None
            };

            if let Some(change) = change {
                debug!("Changing offset to {change:?}");
                system_time.set_offset(change);
            }

            system_time.set_ntp_synchronized(true);

            Ok(())
        }.await;

        if let Err(e) = r {
            error!("npt_task error: {e:?}");
        }

        debug!("ntp_task sleeping");
        Timer::after(Duration::from_secs(60 * 60)).await;
        // Timer::after(Duration::from_secs(60)).await;
        
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

    // heap_allocator!(HEAP_MEMORY_SIZE);
    init_heap();

    if let Err(error) = main_fallible(&spawner, peripherals).await {
        error!("Error while running firmware: {:?}", error);

        let rtc = Rtc::new(unsafe { LPWR::steal()});
        enter_deep_sleep(
            rtc,
            Duration::from_secs(10).into(),
        );
    }
}

async fn main_fallible(spawner: &Spawner, peripherals: Peripherals) -> Result<(), Error> {
    let reset_reason = reset_reason().unwrap_or(SocResetReason::ChipPowerOn);
    let wake_reason = wakeup_cause();
    info!("Reset reason: {reset_reason:?}, Wake reason: {wake_reason:?}");

    let build_date = chrono::Utc.timestamp_millis_opt(BUILD_DATE).single().expect("Invalid build date in binary");
    debug!("BUILD_DATE: {BUILD_DATE} ({build_date:?}");

    let latitude = str::parse::<f64>(LATITUDE).expect("LATITUDE environment variable couldn't be parsed as a f64");
    let longitude = str::parse::<f64>(LONGITUDE).expect("LONGITUDE environment variable couldn't be parsed as a f64");
    let coordinates = Coordinates::new(latitude, longitude)
        .expect("Latitude or Longitude out of range");

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
    let new_build_date_millis = {
        let bytes = http_client.req::<20, _>(BUILD_DATE_URL).await?;
        let string = String::from_utf8(bytes)?;
        
        i64::from_str_radix(&string, 10)?
    };
    let remote_build_date = chrono::Utc.timestamp_millis_opt(new_build_date_millis).single()
        .ok_or(Error::InvalidEpochDate)?;

    debug!("Build dates. Local: {build_date:?}, remote: {remote_build_date:?}");
    let do_update = if new_build_date_millis > BUILD_DATE {
        info!("Update available!");
        true
    } else {
        info!("No update available");
        false
    };

    if do_update {
        let mut ota = Ota::new(&mut flash);
        ota.prepare_for_update()?;

        let mut tot_bytes = 0;
        http_client.req_buffered(FIRMWARE_URL, |buf| {
            tot_bytes += buf.len();
            ota.write_update(buf)?;

            // debug!("{}", HEAP.stats());

            Ok::<_, ota::Error>(())
        }).await?;

        ota.commit_update()?;
        *UPDATE_PENDING.lock().await = true;
    }

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

    
    let mut system_time = SystemTime {};
    
    // Configured persists after restarts other than hard resets
    if !system_time.configured() {
        // TODO: timezone
        system_time.configure(UtcOffset::UTC);
    }

    let ntp_client = ntp::Client::new_from_dns(stacks.ntp, NTP_SERVER).await?;

    spawner.must_spawn(motor_task(receiver, tmc_en, tmc_step, tmc_dir, 2));
    spawner.must_spawn(ntp_task(ntp_client, system_time));
    spawner.must_spawn(schedule_task(sender, coordinates));

    let app = &*mk_static!(AppRouter<AppProps>, AppProps { sender, coordinates }.build_app());
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

    InvalidEpochDate,

    Wifi(wifi::WifiError),

    Nvs(nvs::Error),
    Ota(ota::Error),
    Ntp(ntp::Error),

    Http(http::Error),

    Utf8Error(Utf8Error),

    ParseIntError(ParseIntError),

    Timeout(&'static str),

    Date(ComponentRange),
    Other(&'static str),
}

impl From<ComponentRange> for Error {
    fn from(value: ComponentRange) -> Self {
        Self::Date(value)
    }
}

impl From<ntp::Error> for Error {
    fn from(value: ntp::Error) -> Self {
        Self::Ntp(value)
    }
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


impl<E: Into<Error>> From<CallbackError<E>> for Error {
    fn from(value: CallbackError<E>) -> Self {
        match value {
            CallbackError::Callback(e) => e.into(),
            CallbackError::Outer(e) => e.into(),
        }
    }
}