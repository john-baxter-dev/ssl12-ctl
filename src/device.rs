//! USB transport: open the SSL 12, locate the vendor interface + bulk endpoints,
//! perform the version handshake, and send/receive DSP messages.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use nusb::descriptors::TransferType;
use nusb::transfer::{
    Buffer, Bulk, ControlOut, ControlType, Direction, In, Out, Recipient, TransferError,
};
use nusb::{Endpoint, Interface, MaybeFuture};

use crate::controls::*;
use crate::protocol::{self, DspCode};

/// When false (the default), the transport's bring-up diagnostics — the per-step FTDI-open report
/// and keepalive warnings — are suppressed. They're scaffolding from transport bring-up and, once a
/// TUI has taken over the terminal, any stray stderr corrupts the rendered canvas. `ssl12tui --verbose`
/// (and the `ssl12ctl` diagnostic subcommands) turn them back on via [`set_verbose`].
static VERBOSE: AtomicBool = AtomicBool::new(false);

/// Enable/disable the transport's diagnostic stderr output (see [`VERBOSE`]).
pub fn set_verbose(on: bool) {
    VERBOSE.store(on, Ordering::Relaxed);
}

fn verbose() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

/// `eprintln!` only when [`set_verbose`] has been called with `true`.
macro_rules! diag {
    ($($arg:tt)*) => { if $crate::device::verbose() { eprintln!($($arg)*); } };
}

pub const SSL_VID: u16 = 0x31E9; // 12777
pub const SSL12_PID_CONTROL: u16 = 0x0024; // 36
pub const SSL12_PID_AUDIO: u16 = 0x0005; // 5 (audio function of the composite device)

const IO_TIMEOUT: Duration = Duration::from_millis(2000);
/// Short timeout for IN reads. A long timeout means each empty poll cancels a URB after
/// seconds; rapid timeout/cancel churn on some xHCI stacks can wedge the endpoint. Keep it
/// short so the read loop stays responsive and drains the meter stream promptly.
const READ_TIMEOUT: Duration = Duration::from_millis(250);

/// Build marker so a stale binary is obvious in the field. Bump on transport changes.
pub const TRANSPORT_BUILD: &str =
    "transport-2026-06-23-nusb (pure-Rust nusb + status-strip + ftdi open + tile-init + 0x1b keepalive)";

/// USB message code 0x1B carries the SSL 12's host->device **keepalive** (a rolling 0..3
/// counter). The official software emits it continuously during normal metering; without it
/// the device's watchdog re-enumerates after ~5 s (observed as "No such device").
const USB_KEEPALIVE: u8 = 0x1B;
/// Send a keepalive at least this often. Watchdog fires ~5 s; this gives a wide margin while
/// keeping bus traffic negligible (5 bytes per tick).
const KEEPALIVE_INTERVAL: Duration = Duration::from_millis(500);

/// USB message codes that touch firmware/flash/FPGA/power and could brick or damage
/// the device. The transport refuses to send these. See README "Safety".
const FORBIDDEN_USB_CODES: &[u8] = &[
    10, // USB_START_DOWNLOAD_CMD
    11, // USB_SEND_BLOCK_CMD
    12, // USB_SAVE_DOWNLOAD_CMD
    13, // USB_PROGRAM_ERROR
    14, // USB_PROGRAM_DONE
    15, // USB_RELOAD_FPGA
    16, // USB_START_FIRMWARE_UPDATE
    17, // USB_SEND_BLOCK_TYPE_BYTE
    18, // USB_SEND_BLOCK_LENGTH
    27, // USB_DO_FLASH for arbitrary payloads — but the SSL 12's keepalive reuses code 27 with a
        // benign 1-byte rolling counter; [`Ssl12::heartbeat`] sends THAT directly, bypassing this
        // guard. The guard still blocks any other (potentially flash-triggering) 0x1B payload.
];

/// Whether a USB message code is on the firmware/flash/power denylist — a brick/damage risk the
/// transport refuses to send via [`Ssl12::send_usb`]. The sole sanctioned exception is the keepalive
/// (code 27 with a 1-byte counter), which [`Ssl12::heartbeat`] writes directly, bypassing this guard.
pub fn is_forbidden_usb_code(code: u8) -> bool {
    FORBIDDEN_USB_CODES.contains(&code)
}

#[derive(Debug)]
pub enum Error {
    /// Enumeration / open / claim failures (nusb's general error type).
    Usb(nusb::Error),
    /// A bulk or control transfer failed (stall, disconnect, fault, …). Timeouts surface as
    /// `TransferError::Cancelled` and are handled in the read loop rather than raised here.
    Transfer(TransferError),
    DeviceNotFound,
    VendorInterfaceNotFound,
    HandshakeFailed(String),
    Forbidden(u8),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Usb(e) => write!(f, "usb error: {e}"),
            Error::Transfer(e) => write!(f, "usb transfer error: {e}"),
            Error::DeviceNotFound => write!(f, "SSL 12 not found (VID 0x31E9)"),
            Error::VendorInterfaceNotFound => {
                write!(f, "vendor (0xFF) interface with bulk endpoints not found")
            }
            Error::HandshakeFailed(s) => write!(f, "handshake failed: {s}"),
            Error::Forbidden(c) => write!(
                f,
                "refusing to send firmware/flash USB code {c} (brick risk)"
            ),
        }
    }
}
impl std::error::Error for Error {}
impl From<nusb::Error> for Error {
    fn from(e: nusb::Error) -> Self {
        Error::Usb(e)
    }
}
impl From<TransferError> for Error {
    fn from(e: TransferError) -> Self {
        Error::Transfer(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone)]
pub struct DeviceVersions {
    /// DSP `protocol version reply` (code 2). The SSL 12 reports 1. Readiness is
    /// gated on this (the device must report exactly 1).
    pub protocol_version: u16,
    /// DSP `DSP version reply` (code 15) reply. SSL 12 reports 1. SSL360 waits for
    /// this too; we collect it best-effort (it can arrive just after code 2).
    pub dsp_version: Option<u16>,
    /// `USB_GET_HW_VERSION` (0x4E) reply, if the device answered. SSL 12 = 4.
    pub hw_version: Option<u16>,
    /// `USB_GET_SOFTWARE_VERSION_INT` (0x4B) reply, if the device answered.
    pub sw_version: Option<u32>,
}

/// The control I/F is an FTDI(-style) UART bridge: every bulk IN packet is prefixed with
/// 2 status bytes (modem + line status, e.g. `31 60`) and capped at `wMaxPacketSize`. Those
/// status bytes must be stripped per packet before reassembling the serial byte stream.
const FTDI_PACKET: usize = 64; // wMaxPacketSize of the bulk endpoints
const FTDI_STATUS_BYTES: usize = 2;

// FTDI vendor control requests (host->device). The libusb bmRequestType 0x40 (vendor | host->device
// | device recipient) is expressed structurally in nusb via `ControlType::Vendor` + `Recipient::Device`
// on `control_out` (which implies the host->device direction).
const FTDI_REQ_RESET: u8 = 0x00;
const FTDI_REQ_SET_FLOW_CTRL: u8 = 0x02;
const FTDI_REQ_SET_BAUD_RATE: u8 = 0x03;
const FTDI_REQ_SET_DATA: u8 = 0x04;
const FTDI_REQ_SET_LATENCY: u8 = 0x09;

pub struct Ssl12 {
    /// The claimed vendor interface — keeps the claim alive (released on drop) and carries the
    /// FTDI vendor control transfers. Cloneable (it's an `Arc` internally) but we hold the one.
    interface: Interface,
    /// Bulk endpoints. nusb endpoints are stateful transfer queues, so I/O takes `&mut self`.
    ep_in: Endpoint<Bulk, In>,
    ep_out: Endpoint<Bulk, Out>,
    /// Endpoint addresses, kept for the `info`/status display (the `Endpoint`s above don't expose
    /// them cheaply once moved into the struct).
    ep_in_addr: u8,
    ep_out_addr: u8,
    /// Reassembly buffer for the de-status-ed IN byte stream (frames can span USB packets).
    rx_buf: Vec<u8>,
    /// Rolling 0..3 keepalive sequence counter (matches the captured 0x1B payload sequence).
    hb_counter: u8,
    /// When the last keepalive was sent; `None` until the first one.
    last_hb: Option<Instant>,
    pub ready: bool,
    pub versions: Option<DeviceVersions>,
}

/// Strip the 2 FTDI status bytes from each `FTDI_PACKET`-sized block and append the data.
fn destatus_ftdi(raw: &[u8], out: &mut Vec<u8>) {
    let mut i = 0;
    while i < raw.len() {
        let end = (i + FTDI_PACKET).min(raw.len());
        let block = &raw[i..end];
        if block.len() > FTDI_STATUS_BYTES {
            out.extend_from_slice(&block[FTDI_STATUS_BYTES..]);
        }
        i = end;
    }
}

impl Ssl12 {
    /// Find and open the SSL 12 (tries the control PID, then any SSL-VID device),
    /// claim the vendor interface, and run the handshake.
    pub fn open() -> Result<Self> {
        let mut dev = Self::open_handle()?;
        dev.handshake()?;
        Ok(dev)
    }

    /// Open and claim the vendor interface but **skip** the version handshake (`ready`
    /// stays false). Diagnostic escape hatch: if the handshake doesn't complete, this still
    /// lets you drain the IN endpoint (`listen`/`meters`) since metering is pushed regardless
    /// — useful for seeing whether `protocol version reply` (code 2) ever arrives.
    pub fn open_no_handshake() -> Result<Self> {
        Self::open_handle()
    }

    fn open_handle() -> Result<Self> {
        for info in nusb::list_devices().wait()? {
            if info.vendor_id() != SSL_VID {
                continue;
            }
            let device = match info.open().wait() {
                Ok(d) => d,
                Err(_) => continue,
            };
            // Locate a vendor-specific (class 0xFF) interface with bulk IN+OUT endpoints. Collect
            // the addresses as owned values so the descriptor borrow ends before we claim/open.
            let config = match device.active_configuration() {
                Ok(c) => c,
                Err(_) => continue,
            };
            let mut found: Option<(u8, u8, u8)> = None;
            'ifaces: for interface in config.interfaces() {
                for alt in interface.alt_settings() {
                    if alt.class() != 0xFF {
                        continue;
                    }
                    let mut ep_in = None;
                    let mut ep_out = None;
                    for ep in alt.endpoints() {
                        if ep.transfer_type() != TransferType::Bulk {
                            continue;
                        }
                        match ep.direction() {
                            Direction::In => ep_in = Some(ep.address()),
                            Direction::Out => ep_out = Some(ep.address()),
                        }
                    }
                    if let (Some(ep_in), Some(ep_out)) = (ep_in, ep_out) {
                        found = Some((alt.interface_number(), ep_in, ep_out));
                        break 'ifaces;
                    }
                }
            }
            let Some((iface, ep_in_addr, ep_out_addr)) = found else {
                continue;
            };
            // Claim the interface, detaching any kernel driver first (Linux). On other platforms
            // this is a plain claim.
            let interface = device.detach_and_claim_interface(iface).wait()?;
            let mut ep_in = interface.endpoint::<Bulk, In>(ep_in_addr)?;
            let mut ep_out = interface.endpoint::<Bulk, Out>(ep_out_addr)?;
            // Clear any lingering endpoint STALL left by a previous client that wedged the device
            // (a halted bulk endpoint returns Stall and would otherwise swallow the handshake on
            // this fresh open).
            let _ = ep_in.clear_halt().wait();
            let _ = ep_out.clear_halt().wait();
            let dev = Ssl12 {
                interface,
                ep_in,
                ep_out,
                ep_in_addr,
                ep_out_addr,
                rx_buf: Vec::new(),
                hb_counter: 0,
                last_hb: None,
                ready: false,
                versions: None,
            };
            // Open the FTDI UART the way the Windows driver does, so the device's MCU sees the
            // port as live and starts streaming (meters etc.).
            dev.ftdi_open();
            return Ok(dev);
        }
        Err(Error::DeviceNotFound)
    }

    pub fn endpoints(&self) -> (u8, u8) {
        (self.ep_in_addr, self.ep_out_addr)
    }

    /// One FTDI vendor control transfer (best-effort). Goes out on the interface's control pipe;
    /// `control_out` implies host->device, and `Vendor`/`Device` reproduce the libusb `0x40`
    /// request type.
    fn ftdi_ctrl(
        &self,
        request: u8,
        value: u16,
        index: u16,
    ) -> std::result::Result<(), TransferError> {
        self.interface
            .control_out(
                ControlOut {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Device,
                    request,
                    value,
                    index,
                    data: &[],
                },
                IO_TIMEOUT,
            )
            .wait()
    }

    /// Open the FTDI UART **exactly** as SSL360 does — extracted byte-for-byte from the working
    /// capture (`connect_unfiltered.txt --setup`): reset, 8N1, no flow control, 115200
    /// (divisor 0x1A), 2 ms latency, purge RX/TX. All `wIndex = 0`. Notably SSL360 sends **no**
    /// MODEM_CTRL (DTR/RTS). Reports each transfer so a non-compliant bridge is obvious.
    fn ftdi_open(&self) {
        const IDX: u16 = 0;
        let steps: [(&str, u8, u16); 7] = [
            ("reset", FTDI_REQ_RESET, 0x0000),
            ("data=8N1", FTDI_REQ_SET_DATA, 0x0008),
            ("flow=none", FTDI_REQ_SET_FLOW_CTRL, 0x0000),
            ("baud=115200", FTDI_REQ_SET_BAUD_RATE, 0x001A),
            ("latency=2ms", FTDI_REQ_SET_LATENCY, 0x0002),
            ("purge_rx", FTDI_REQ_RESET, 0x0001),
            ("purge_tx", FTDI_REQ_RESET, 0x0002),
        ];
        let mut ok = 0;
        for (name, req, val) in steps {
            match self.ftdi_ctrl(req, val, IDX) {
                Ok(_) => ok += 1,
                Err(e) => diag!("  ftdi {name}: ERR {e}"),
            }
        }
        diag!(
            "ftdi_open: {ok}/{} control transfers ok (wIndex={IDX})",
            steps.len()
        );
    }

    /// Raw send of a USB-coded serial frame. Refuses firmware/flash codes.
    pub fn send_usb(&mut self, code: u8, payload: &[u8]) -> Result<()> {
        if is_forbidden_usb_code(code) {
            return Err(Error::Forbidden(code));
        }
        let buf = protocol::frame(code, payload);
        self.ep_out
            .transfer_blocking(buf.into(), IO_TIMEOUT)
            .into_result()?;
        Ok(())
    }

    /// Send a DSP message (wrapped in USB_SEND_DSP). No-op-guards on readiness are
    /// the caller's responsibility except during handshake.
    pub fn send_dsp(&mut self, dsp_msg: &[u8]) -> Result<()> {
        self.send_usb(protocol::USB_SEND_DSP, dsp_msg)
    }

    /// Send one keepalive (USB code 0x1B) carrying the next rolling 0..3 counter value, and
    /// advance the counter + timer. Sent directly via `write_bulk` so it bypasses the
    /// firmware-code guard (0x1B is guarded for arbitrary payloads, but this fixed 1-byte
    /// counter payload is the device's designed keepalive — see [`USB_KEEPALIVE`]).
    /// The device's watchdog requires this regularly or it re-enumerates after ~5 s.
    pub fn heartbeat(&mut self) -> Result<()> {
        let buf = protocol::frame(USB_KEEPALIVE, &[self.hb_counter]);
        self.ep_out
            .transfer_blocking(buf.into(), IO_TIMEOUT)
            .into_result()?;
        self.hb_counter = (self.hb_counter + 1) & 0x03;
        self.last_hb = Some(Instant::now());
        Ok(())
    }

    /// Send a keepalive only if `KEEPALIVE_INTERVAL` has elapsed since the last one (or none
    /// has been sent yet). Called from the read loop so any consumer keeps the device alive.
    fn keepalive_if_due(&mut self) {
        let due = self
            .last_hb
            .is_none_or(|t| t.elapsed() >= KEEPALIVE_INTERVAL);
        if due {
            if let Err(e) = self.heartbeat() {
                diag!("keepalive send failed: {e}");
            }
        }
    }

    /// Diagnostic: one raw bulk IN read, returning the bytes exactly as delivered (no frame
    /// parsing). Lets us see whether the endpoint yields nothing, transport status bytes, or
    /// meter data we're misframing. Empty vec = timeout (no data).
    pub fn read_raw(&mut self, timeout_ms: u64) -> Result<Vec<u8>> {
        let c = self
            .ep_in
            .transfer_blocking(Buffer::new(4096), Duration::from_millis(timeout_ms));
        match c.status {
            Ok(()) => Ok(c.buffer[..c.actual_len].to_vec()),
            // A cancelled transfer is our timeout (no data this round).
            Err(TransferError::Cancelled) => Ok(Vec::new()),
            // A STALLed IN endpoint is recoverable: clear the halt and report no data.
            Err(TransferError::Stall) => {
                let _ = self.ep_in.clear_halt().wait();
                Ok(Vec::new())
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Read one bulk transfer, strip the per-packet FTDI status bytes, and parse all complete
    /// serial frames. Partial frames (and frames split across transfers) are buffered in
    /// `rx_buf` until complete.
    pub fn read_frames(&mut self) -> Result<Vec<protocol::ParsedFrame>> {
        // Feed the device watchdog before reading so the meter stream never lapses.
        self.keepalive_if_due();
        let c = self
            .ep_in
            .transfer_blocking(Buffer::new(4096), READ_TIMEOUT);
        match c.status {
            Ok(()) => destatus_ftdi(&c.buffer[..c.actual_len], &mut self.rx_buf),
            // Cancelled == our read timeout: nothing new this round.
            Err(TransferError::Cancelled) => {}
            // A STALLed IN endpoint is recoverable: clear the halt and report no data this round
            // rather than killing the read loop.
            Err(TransferError::Stall) => {
                let _ = self.ep_in.clear_halt().wait();
            }
            Err(e) => return Err(e.into()),
        }

        let mut out = Vec::new();

        while let Some((frame, consumed)) = protocol::parse_frame(&self.rx_buf) {
            out.push(frame);
            self.rx_buf.drain(..consumed);
        }

        // If the buffer holds bytes but no frame start, it's noise — don't let it grow forever.
        if !self.rx_buf.contains(&protocol::START_CODE) && self.rx_buf.len() > FTDI_PACKET {
            self.rx_buf.clear();
        }
        Ok(out)
    }

    /// Version exchange (must succeed before parameter writes take effect).
    ///
    /// Replicates the **exact** USB-layer connect handshake observed in the working capture
    /// (`connect_unfiltered.txt`), which runs after [`Self::ftdi_open`]:
    ///   GET_IS_TILE(0x01) → GET_TILE_ID(0x02) → INIT_TILE(0x05) → GET_SW_VERSION(0x4B)
    ///   → GET_HW_VERSION(0x4E), all empty-payload, then the DSP version requests.
    /// **The device begins streaming meters immediately after `INIT_TILE` + the version
    /// queries** — the tile-init step is REQUIRED (earlier removal was a misread of a capture
    /// whose ≤32-byte host frames had been filtered out). Gates ready on the DSP protocol
    /// version; the DSP version (code 15) is best-effort.
    pub fn handshake(&mut self) -> Result<DeviceVersions> {
        // Tile init — the device answers each (IS_TILE=0x1234, TILE_ID=0xa112, INIT_TILE=0) and
        // will not start the meter stream without it.
        self.send_usb(protocol::USB_GET_IS_TILE, &[])?;
        self.send_usb(protocol::USB_GET_TILE_ID, &[])?;
        self.send_usb(protocol::USB_INIT_TILE, &[])?;
        // USB-layer version queries (device answers on IN with codes 0x4B / 0x4E).
        let _ = self.send_usb(protocol::USB_GET_SOFTWARE_VERSION_INT, &[]);
        let _ = self.send_usb(protocol::USB_GET_HW_VERSION, &[]);
        // DSP-layer version handshake — both requests, matching SSL360.
        self.send_dsp(&protocol::dsp_bare(DspCode::RequestProtocolVersion.num()))?;
        self.send_dsp(&protocol::dsp_bare(DspCode::RequestDspVersion.num()))?;

        let mut protocol_version: Option<u16> = None;
        let mut dsp_version: Option<u16> = None;
        let mut hw_version: Option<u16> = None;
        let mut sw_version: Option<u32> = None;

        // Bound the loop so a silent device fails instead of hanging. Break once the protocol
        // version is in hand AND we've either seen the DSP version or given it a few extra
        // reads to show up.
        let mut polls_after_pv = 0;
        for _ in 0..64 {
            for f in self.read_frames()? {
                match f.code {
                    protocol::USB_GET_HW_VERSION if f.payload.len() >= 2 => {
                        hw_version = Some(u16::from_le_bytes([f.payload[0], f.payload[1]]));
                    }
                    protocol::USB_GET_SOFTWARE_VERSION_INT if f.payload.len() >= 4 => {
                        sw_version = Some(u32::from_le_bytes([
                            f.payload[0],
                            f.payload[1],
                            f.payload[2],
                            f.payload[3],
                        ]));
                    }
                    protocol::USB_RECV_DSP if f.payload.len() >= 4 => {
                        let inner = u16::from_le_bytes([f.payload[0], f.payload[1]]);
                        let value = u16::from_le_bytes([f.payload[2], f.payload[3]]);
                        match DspCode::try_from(inner) {
                            Ok(DspCode::ProtocolVersionInformation) => {
                                protocol_version = Some(value)
                            }
                            Ok(DspCode::DspVersionInformation) => dsp_version = Some(value),
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
            if protocol_version.is_some() {
                // Got the required gate; give code 15 a short grace window, then proceed.
                if dsp_version.is_some() || polls_after_pv >= 8 {
                    break;
                }
                polls_after_pv += 1;
            }
        }

        let Some(pv) = protocol_version else {
            return Err(Error::HandshakeFailed(
                "no protocol version reply (code 2) on IN endpoint — is the vendor interface the right one?".into(),
            ));
        };
        if pv != PROTOCOL_VERSION as u16 {
            return Err(Error::HandshakeFailed(format!(
                "unsupported protocol version {pv} (this client speaks {PROTOCOL_VERSION})"
            )));
        }

        let versions = DeviceVersions {
            protocol_version: pv,
            dsp_version,
            hw_version,
            sw_version,
        };
        self.versions = Some(versions.clone());
        self.ready = true;
        Ok(versions)
    }

    // ---- High-level controls (parameters) ----

    pub fn set_phantom(&mut self, input: u8, on: bool) -> Result<()> {
        self.send_dsp(&protocol::dsp_bool(
            DspCode::ParamUpdateBool.num(),
            Param::InputPhantomPower.num(),
            input as u16,
            on,
        ))
    }
    pub fn set_hpf(&mut self, input: u8, on: bool) -> Result<()> {
        self.send_dsp(&protocol::dsp_bool(
            DspCode::ParamUpdateBool.num(),
            Param::InputHpf.num(),
            input as u16,
            on,
        ))
    }
    pub fn set_line_input(&mut self, input: u8, line: bool) -> Result<()> {
        self.send_dsp(&protocol::dsp_bool(
            DspCode::ParamUpdateBool.num(),
            Param::InputLineInput.num(),
            input as u16,
            line,
        ))
    }
    pub fn set_instrument_hiz(&mut self, input: u8, hiz: bool) -> Result<()> {
        self.send_dsp(&protocol::dsp_bool(
            DspCode::ParamUpdateBool.num(),
            Param::InputInstrumentInput.num(),
            input as u16,
            hiz,
        ))
    }
    pub fn set_input_polarity(&mut self, channel: u8, invert: bool) -> Result<()> {
        self.send_dsp(&protocol::dsp_bool(
            DspCode::ParamUpdateBool.num(),
            Param::InputPolarity.num(),
            channel as u16,
            invert,
        ))
    }
    pub fn set_clock_source(&mut self, source: ClockSource) -> Result<()> {
        self.send_dsp(&protocol::dsp_selection(
            DspCode::ParamUpdateSelection.num(),
            Param::ClockSelection.num(),
            0,
            source as u16,
        ))
    }
    /// Monitor / headphone output level in dB (index 0 = main, 4/6 = phones).
    pub fn set_output_level_db(&mut self, index: u16, db: f64) -> Result<()> {
        self.send_dsp(&protocol::dsp_q625(
            DspCode::ParamUpdateQ625.num(),
            Param::OutputLevel.num(),
            index,
            protocol::db_to_q625(db),
        ))
    }

    // ---- High-level controls (coefficients) ----

    /// Monitor-mix matrix cell (index 0..=239) set in dB.
    pub fn set_crosspoint_db(&mut self, index: u16, db: f64) -> Result<()> {
        self.send_dsp(&protocol::dsp_q625(
            DspCode::CoefficientUpdateQ625.num(),
            Coeff::MixerCrosspointTable.num(),
            index,
            protocol::db_to_q625(db),
        ))
    }

    /// Route a mixer source into a destination output at the given gain (dB), writing the
    /// crosspoint cells for both legs. Uses the recovered slot/block map in [`crate::mixer`].
    pub fn set_mix(
        &mut self,
        source: crate::mixer::Source,
        dest: crate::mixer::Destination,
        db: f64,
    ) -> Result<()> {
        for cell in crate::mixer::cells_for(source, dest) {
            self.set_crosspoint_db(cell, db)?;
        }
        Ok(())
    }
    /// Ask the device to dump all current control states (`USB_REQUEST_CONTROL_STATES`).
    /// SSL360 issues a state sync right after the version handshake; doing the same may be
    /// what flips the device into its live/streaming mode (and stops `USB_RECONNECT_REQUIRED`).
    pub fn request_control_states(&mut self) -> Result<()> {
        self.send_usb(protocol::USB_REQUEST_CONTROL_STATES, &[])
    }

    /// Ask for one parameter's current value (`value request`). Device replies with the
    /// matching `value reply` message on IN.
    pub fn request_param_value(&mut self, number: u16, index: u16) -> Result<()> {
        self.send_dsp(&protocol::dsp_value_request(number, index))
    }

    pub fn mute_hardware_outputs(&mut self, mute: bool) -> Result<()> {
        self.send_dsp(&protocol::dsp_bool(
            DspCode::CoefficientUpdateBool.num(),
            Coeff::MuteHardwareOutputs.num(),
            0,
            mute,
        ))
    }
    pub fn set_stereo_link(&mut self, index: u16, linked: bool) -> Result<()> {
        self.send_dsp(&protocol::dsp_bool(
            DspCode::CoefficientUpdateBool.num(),
            Coeff::StereoLinkChannels.num(),
            index,
            linked,
        ))
    }
    pub fn set_loopback_source(&mut self, source: LoopbackSource) -> Result<()> {
        self.send_dsp(&protocol::dsp_selection(
            DspCode::CoefficientUpdateSelection.num(),
            Coeff::LoopbackSource.num(),
            0,
            source as u16,
        ))
    }
    pub fn set_line_output_operating_level(
        &mut self,
        index: u16,
        level: OutputOperatingLevel,
    ) -> Result<()> {
        self.send_dsp(&protocol::dsp_selection(
            DspCode::CoefficientUpdateSelection.num(),
            Coeff::LineOutputOperatingLevel.num(),
            index,
            level as u16,
        ))
    }
    pub fn set_headphone_gain_mode(&mut self, index: u16, mode: HeadphoneGainMode) -> Result<()> {
        self.send_dsp(&protocol::dsp_selection(
            DspCode::CoefficientUpdateSelection.num(),
            Coeff::HeadphonesGainMode.num(),
            index,
            mode as u16,
        ))
    }
    pub fn set_bus_output_mode(&mut self, index: u16, mode: BusOutputMode) -> Result<()> {
        self.send_dsp(&protocol::dsp_selection(
            DspCode::CoefficientUpdateSelection.num(),
            Coeff::BusOutputMode.num(),
            index,
            mode as u16,
        ))
    }
}

// No explicit `Drop`: nusb releases the claimed interface (and cancels pending transfers) when the
// `Interface`/`Endpoint`s drop.

#[cfg(test)]
mod tests {
    use super::*;

    /// Every firmware/flash/power code must stay on the denylist — this is the brick guard, so a
    /// regression here is dangerous. Codes from `FORBIDDEN_USB_CODES` (the block-download family
    /// 10–18 plus 27 `USB_DO_FLASH`).
    #[test]
    fn firmware_and_flash_codes_stay_forbidden() {
        for code in [10u8, 11, 12, 13, 14, 15, 16, 17, 18, 27] {
            assert!(
                is_forbidden_usb_code(code),
                "code {code} must stay on the brick-risk denylist"
            );
        }
    }

    /// Normal control traffic must NOT be blocked, or the transport can't talk to the device.
    #[test]
    fn normal_control_codes_are_allowed() {
        for code in [
            protocol::USB_SEND_DSP,
            protocol::USB_GET_IS_TILE,
            protocol::USB_GET_TILE_ID,
            protocol::USB_INIT_TILE,
            protocol::USB_GET_SOFTWARE_VERSION_INT,
            protocol::USB_GET_HW_VERSION,
            protocol::USB_REQUEST_CONTROL_STATES,
        ] {
            assert!(
                !is_forbidden_usb_code(code),
                "code {code} is normal control traffic"
            );
        }
    }

    /// The keepalive code is on the denylist, so a stray `send_usb(0x1B, …)` is refused. `heartbeat`
    /// is the ONLY sanctioned 0x1B path and writes via `write_bulk` directly, bypassing the guard.
    #[test]
    fn keepalive_code_is_guarded_against_stray_sends() {
        assert!(
            is_forbidden_usb_code(USB_KEEPALIVE),
            "0x1B must be guarded except via heartbeat()"
        );
    }
}
