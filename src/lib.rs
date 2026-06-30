//! Userspace control of the Solid State Logic SSL 12 mixer/DSP over its USB vendor
//! protocol. Audio I/O itself is standard USB Audio Class 2.0 (handled by the kernel's
//! `snd-usb-audio`); this crate only speaks the vendor *control* protocol.
//!
//! Layers:
//!   1. serial framing  `FF | Code | Len | Payload | CRC`        (`protocol`)
//!   2. USB message codes (DSP = 0x6B/0x6C)                       (`protocol`)
//!   3. DSP messages (param/coefficient updates, Q6.25 gains)      (`protocol` + `controls`)
//!
//! `capture` decodes USBPcap hex-dump text offline (no hardware), behind the `dev-tools`
//! feature (reverse-engineering only). `device` is the live USB transport (pure-Rust nusb), behind `usb`.
//!
//! See `docs/PROTOCOL.md` for the full reverse-engineered specification, and
//! `docs/ARCHITECTURE.md` for how this crate is organized.

pub mod controls;
pub mod meters;
pub mod mixer;
pub mod protocol;
pub mod transport;

// Offline capture decoder — used only by the `capturedecode` RE tool, so it rides the same flag.
#[cfg(feature = "dev-tools")]
pub mod capture;

#[cfg(feature = "config")]
pub mod preset;

#[cfg(feature = "usb")]
pub mod device;
#[cfg(feature = "usb")]
pub use device::{Error, Result, Ssl12};
