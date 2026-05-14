//! USB device stack. Composite device with one CDC ACM `events`
//! interface for Phase 4a per `docs/usb_interface_design.md`; the
//! `control`, `console`, and vendor-bulk interfaces land in later
//! phases on top of the same `Builder`.

use embassy_rp::Peri;
use embassy_rp::bind_interrupts;
use embassy_rp::peripherals::USB;
use embassy_rp::usb::{Driver, InterruptHandler};
use embassy_usb::class::cdc_acm::{CdcAcmClass, State as CdcState};
use embassy_usb::{Builder, Config, UsbDevice};
use static_cell::StaticCell;

pub mod events;

bind_interrupts!(pub(crate) struct UsbIrqs {
    USBCTRL_IRQ => InterruptHandler<USB>;
});

/// Vendor strings the USB host sees. The product ID is a test value —
/// real distribution would mint a proper VID/PID.
const VID: u16 = 0xC0DE;
const PID: u16 = 0xCAFE;
const MANUFACTURER: &str = "vintage-kvm";
const PRODUCT: &str = "Pico1284";

type UsbDriver = Driver<'static, USB>;

/// All of embassy-usb's persistent buffers in one bag so the static_cells
/// are colocated and easy to size-tune.
pub struct UsbResources {
    pub config_descriptor: [u8; 256],
    pub bos_descriptor: [u8; 256],
    pub msos_descriptor: [u8; 256],
    pub control_buf: [u8; 128],
    pub events_state: CdcState<'static>,
}

impl UsbResources {
    pub const fn new() -> Self {
        Self {
            config_descriptor: [0; 256],
            bos_descriptor: [0; 256],
            msos_descriptor: [0; 256],
            control_buf: [0; 128],
            events_state: CdcState::new(),
        }
    }
}

static RESOURCES: StaticCell<UsbResources> = StaticCell::new();

/// Build the composite USB device + the events-CDC class instance.
/// Spawns the USB control task; returns the CDC class so the caller can
/// hand it to the events-writer task.
pub fn build(usb: Peri<'static, USB>) -> (UsbDevice<'static, UsbDriver>, CdcAcmClass<'static, UsbDriver>) {
    let driver = Driver::new(usb, UsbIrqs);

    let mut config = Config::new(VID, PID);
    config.manufacturer = Some(MANUFACTURER);
    config.product = Some(PRODUCT);
    config.serial_number = Some("0000-0001"); // TODO: derive from RP2350 chip id
    config.max_power = 100;
    config.max_packet_size_0 = 64;
    // IAD-composite: required for >1 CDC function to bind cleanly on
    // Windows. Even with only one CDC today, the descriptor stays
    // compatible with later composites.
    config.composite_with_iads = true;
    config.device_class = 0xEF;
    config.device_sub_class = 0x02;
    config.device_protocol = 0x01;

    let r = RESOURCES.init(UsbResources::new());

    let mut builder = Builder::new(
        driver,
        config,
        &mut r.config_descriptor,
        &mut r.bos_descriptor,
        &mut r.msos_descriptor,
        &mut r.control_buf,
    );

    let events_class = CdcAcmClass::new(&mut builder, &mut r.events_state, 64);

    let usb_device = builder.build();
    (usb_device, events_class)
}

#[embassy_executor::task]
pub async fn run_device(mut device: UsbDevice<'static, UsbDriver>) -> ! {
    device.run().await
}
