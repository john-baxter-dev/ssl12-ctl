# Architecture — `ssl12-ctl`

How the crate is organized, and where to look when extending it. For the wire protocol itself
see [`PROTOCOL.md`](PROTOCOL.md); this document is about the **code**.

## What this crate is (and isn't)

It speaks the SSL 12's **vendor control protocol** — the monitor mixer, input switches (48V,
Hi-Z, line, HPF, polarity), routing, output-bus controls, headphone modes, and the meter stream.
It does **not** do audio I/O: the SSL 12 is class-compliant USB Audio 2.0, so playback/capture and
master volume are handled by the kernel (`snd-usb-audio`). We only drive the SSL-specific DSP.

## Layered design

The code mirrors the three protocol layers plus a transport layer below them:

```
            ┌──────────────────────────────────────────────┐
  bins  →   │  ssl12ctl (CLI)   ssl12tui (TUI)   capturedecode (offline)
            └───────────────┬───────────────┬──────────────┘
                            │               │
  transport  →   transport::Transport  (trait: poll / send_dsp / is_ready)
                       ┌────┴─────────────────────┐
                  Ssl12 (live USB)           MockTransport (synthetic)
                  [feature = "usb"]          [always available]
                            │
  L0 FTDI     →   device.rs: ftdi_open, per-packet status strip, 0x1b keepalive
  L1 framing  →   protocol::frame / parse_frame   (FF | code | len | payload | crc)
  L2 usb code →   protocol:: USB_SEND_DSP 0x6B / USB_RECV_DSP 0x6C / tile + version codes
  L3 dsp msg  →   protocol::dsp_* builders + controls:: numbers + meters:: / mixer:: decoders
```

## Module map (`src/`)

| Module | Role | Hardware? |
|---|---|---|
| `protocol.rs` | Serial framing + CRC, USB message codes, DSP message builders, Q6.25 ↔ dB math. The pure spec, fully unit-tested. | no |
| `controls.rs` | The SSL 12 control map: `param::*` and `coeff::*` numbers, valid index sets, name lookups, version constants. | no |
| `meters.rs` | Decode the meter table (code 9): level + clip per sample, the 29-entry index→channel labels, dBFS. | no |
| `mixer.rs` | The monitor-mixer model: the 240-cell crosspoint matrix as `index = destination*30 + source_slot`, with the confirmed source/destination layout. | no |
| `capture.rs` | Offline decoder for USBPcap hex-dump text (used by `capturedecode`; behind the `dev-tools` feature). | no |
| `transport.rs` | The `Transport` trait + `MockTransport`. The seam that lets the UI run with or without a device. | no (mock) |
| `device.rs` | The live USB transport (pure-Rust `nusb`): device discovery, the FTDI transport layer (`ftdi_open`, status stripping, `heartbeat`), the connect handshake, and read/write of frames. | **yes** (`usb` feature) |

### Binaries (`src/bin/`)

- **`ssl12ctl`** — the CLI. One-shot control commands (`phantom`, `xpoint`, `mute`, …), `info`,
  `meters`/`listen`, plus diagnostics (`rawin`, `golive`, `probe`). Needs `usb`.
- **`ssl12tui`** — the terminal UI (ratatui). Picks the live device or the mock; needs `tui`.
- **`capturedecode`** — offline capture analysis (`--summary`, `--meters`, `--rawframes`,
  `--setup`, …). No hardware, no libusb. This is how the control/meter/crosspoint maps were derived.

## Cargo features

| Feature | Pulls in | Enables |
|---|---|---|
| `usb` (default) | `nusb` (pure Rust — no native deps) | the live `device.rs` transport; the `ssl12ctl` binary |
| `tui` (default) | `ratatui`, `crossterm` | the `ssl12tui` binary (pure Rust — no native deps) |

Because `usb` is the only thing needing a native lib, **anything not touching real hardware builds
with `--no-default-features`** (optionally `--features tui`). That's what makes off-bench
development possible:

```sh
cargo test --no-default-features                       # protocol/meters/mixer/mock tests
cargo run  --no-default-features --features tui --bin ssl12tui   # TUI against the mock
cargo run  --no-default-features --features dev-tools --bin capturedecode -- cap.txt  # offline analysis
```

## The transport seam (developing without hardware)

`transport::Transport` is the contract the UI codes against:

```rust
trait Transport {
    fn poll(&mut self) -> Result<Vec<ParsedFrame>, String>; // device→host frames this tick
    fn send_dsp(&mut self, dsp_msg: &[u8]) -> Result<(), String>;
    fn is_ready(&self) -> bool;
    fn description(&self) -> String;
}
```

- `Ssl12` implements it over real USB (only compiled with `usb`).
- `MockTransport` implements it with **synthetic 29-channel meters** (~30 Hz, animated) and by
  **echoing parameter writes back as the device's VALUE message** — so the UI's "device is the
  source of truth" reconciliation path runs exactly as it would on hardware. New UI features should
  be built against the trait and verified against the mock first, then sanity-checked on the device.

To extend the mock's fidelity (e.g. so a new control reflects in the UI), teach
`MockTransport::echo_for_update` to mirror that message family.

## Adding a control (the usual workflow)

1. Confirm the `(number, index, value-type)` in `controls.rs` / `PROTOCOL.md` (or map it by moving
   the control in SSL 360, capturing, and running `capturedecode --summary`).
2. Build the message with the matching `protocol::dsp_*` helper.
3. Send via `Transport::send_dsp` (or a typed helper on `Ssl12`).
4. Reconcile: handle the device's VALUE echo in the consumer (see `ssl12tui`'s `apply_value`).
5. Add a unit test in `protocol.rs`/`mixer.rs` pinning the exact bytes if it's wire-format-critical.

## Safety model

`device.rs` refuses the firmware/flash codes (10–18, 27) for arbitrary payloads — the only commands
that write non-volatile memory. Code 27 is reused by the device as the benign keepalive, which
`heartbeat()` sends directly with a fixed 1-byte payload (the guard still blocks any other 27
payload). Everything else this crate exposes is volatile DSP state that a power cycle resets.
