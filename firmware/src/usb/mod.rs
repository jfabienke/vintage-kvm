//! USB device stack. Composite device with three landed interfaces
//! per `docs/usb_interface_design.md`:
//!
//! - `events`  — CDC ACM IN-only telemetry stream (Phase 4a).
//! - `control` — CDC ACM bidirectional RPC (Phase 4b).
//! - `bulk`    — vendor-class bulk IN+OUT for blobs (Phase 6).
//!
//! The `console` CDC slot is left for Phase 5 and plugs onto the same
//! `Builder` when it lands.

use embassy_rp::Peri;
use embassy_rp::bind_interrupts;
use embassy_rp::peripherals::USB;
use embassy_rp::usb::{Driver, Endpoint, In, InterruptHandler, Out};
use embassy_usb::class::cdc_acm::{CdcAcmClass, State as CdcState};
use embassy_usb::{Builder, Config, UsbDevice};
use static_cell::StaticCell;

pub mod bulk;
pub mod control;
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
    pub control_state: CdcState<'static>,
}

impl UsbResources {
    pub const fn new() -> Self {
        Self {
            config_descriptor: [0; 256],
            bos_descriptor: [0; 256],
            msos_descriptor: [0; 256],
            control_buf: [0; 128],
            events_state: CdcState::new(),
            control_state: CdcState::new(),
        }
    }
}

static RESOURCES: StaticCell<UsbResources> = StaticCell::new();

/// Composite USB device plus the class / endpoint handles landed
/// today: events (IN-only CDC telemetry), control (bidirectional CDC
/// RPC), and the vendor bulk IN/OUT pair.
pub struct UsbStack {
    pub device: UsbDevice<'static, UsbDriver>,
    pub events: CdcAcmClass<'static, UsbDriver>,
    pub control: CdcAcmClass<'static, UsbDriver>,
    pub bulk_in: Endpoint<'static, USB, In>,
    pub bulk_out: Endpoint<'static, USB, Out>,
}

/// Build the composite USB device + both CDC class instances. The
/// caller drives `device` via `run_device` and hands each class to its
/// matching task.
pub fn build(usb: Peri<'static, USB>) -> UsbStack {
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
    let control_class = CdcAcmClass::new(&mut builder, &mut r.control_state, 64);

    // Vendor-class function for the bulk interface. Class 0xFF
    // (vendor-specific) lets libusb / WinUSB claim it; the host picks
    // up the IAD because composite_with_iads is set above. v1 advertises
    // one alt setting with one bulk IN + one bulk OUT endpoint, both
    // 64-byte FS max packet size.
    let (bulk_in, bulk_out) = {
        let mut func = builder.function(0xFF, 0x00, 0x00);
        let mut iface = func.interface();
        let mut alt = iface.alt_setting(0xFF, 0x00, 0x00, None);
        let ep_in = alt.endpoint_bulk_in(None, 64);
        let ep_out = alt.endpoint_bulk_out(None, 64);
        (ep_in, ep_out)
    };

    let device = builder.build();
    UsbStack {
        device,
        events: events_class,
        control: control_class,
        bulk_in,
        bulk_out,
    }
}

#[embassy_executor::task]
pub async fn run_device(mut device: UsbDevice<'static, UsbDriver>) -> ! {
    device.run().await
}
