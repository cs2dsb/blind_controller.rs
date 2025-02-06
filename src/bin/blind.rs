#![no_std]
#![no_main]
#![feature(sync_unsafe_cell)]
#![feature(impl_trait_in_assoc_type)]

use core::{convert::Infallible, fmt::Write};

use blind_controller::{logging, wifi};
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
use heapless::String;
use log::*;
use picoserve::{
    routing::{get, parse_path_segment},
    AppBuilder, AppRouter,
};

const HEAP_MEMORY_SIZE: usize = 72 * 1024;

// -3 is to reserve some for outgoing socket requests
const WEB_TASK_POOL_SIZE: usize = wifi::STACK_SOCKET_COUNT - 3;

const SSID: &str = env!("SSID");
const PASSWORD: &str = env!("PASSWORD");

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

    let ssid = String::<32>::try_from(SSID).map_err(|()| Error::ParseCredentials)?;
    let password = String::<64>::try_from(PASSWORD).map_err(|()| Error::ParseCredentials)?;

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

    Wifi(wifi::WifiError),

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
