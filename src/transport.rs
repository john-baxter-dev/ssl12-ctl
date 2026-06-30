//! Backend abstraction so the UI can run against either the live USB device or a
//! hardware-free **mock**. This is what makes the TUI developable on the road (macOS,
//! a plane, anywhere without the SSL 12 plugged in): the mock synthesizes the same
//! meter stream and echoes control writes back as the device's VALUE messages, so the
//! whole app — meters, mixer state, reconciliation — exercises the real code paths.
//!
//! The trait is deliberately thin and feature-agnostic (no USB-backend types leak through),
//! so this module compiles with `--no-default-features --features tui`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::protocol::{self, frame, DspCode, ParsedFrame, USB_RECV_DSP};

/// What the UI needs from a backend. Errors are stringly-typed to avoid coupling the UI
/// to the `usb`-gated [`crate::device::Error`].
pub trait Transport {
    /// Pump I/O once: send any due keepalive, then return whatever device→host frames are
    /// available right now. An empty vec means "nothing new this tick" (not an error).
    fn poll(&mut self) -> Result<Vec<ParsedFrame>, String>;

    /// Send a DSP message (the DSP payload, e.g. from `protocol::dsp_bool(...)`).
    fn send_dsp(&mut self, dsp_msg: &[u8]) -> Result<(), String>;

    /// Whether the handshake has completed and writes will take effect.
    fn is_ready(&self) -> bool {
        true
    }

    /// Terse backend tag for the title bar: "USB" for the live device, "MOCK" for the stub. Kept
    /// short on purpose — the title front already says "SSL 12", and the build string is dev detail.
    fn label(&self) -> &'static str;
}

// ---- Live device implementation (only with the `usb` feature) -------------------------

#[cfg(feature = "usb")]
impl Transport for crate::device::Ssl12 {
    fn poll(&mut self) -> Result<Vec<ParsedFrame>, String> {
        self.read_frames().map_err(|e| e.to_string())
    }
    fn send_dsp(&mut self, dsp_msg: &[u8]) -> Result<(), String> {
        crate::device::Ssl12::send_dsp(self, dsp_msg).map_err(|e| e.to_string())
    }
    fn is_ready(&self) -> bool {
        self.ready
    }
    fn label(&self) -> &'static str {
        "USB"
    }
}

// ---- Mock backend ---------------------------------------------------------------------

/// A hardware-free SSL 12 stand-in. Emits an animated 29-channel meter table at ~30 Hz and
/// echoes parameter writes back as the matching VALUE message, so the UI's device-truth
/// reconciliation path runs exactly as it would against real hardware.
pub struct MockTransport {
    start: Instant,
    last_meter: Option<Instant>,
    meter_period: Duration,
    /// Frames queued to hand back on the next `poll` (control-write echoes).
    pending: Vec<ParsedFrame>,
    /// The device's connect-time parameter VALUE dump, handed out on the first `poll` (mirrors the
    /// real SSL 12, which streams ~71 `value reply` frames after the version exchange). Drained once.
    connect_dump: Vec<ParsedFrame>,
    /// Mirror of values the host has written, so requests/echoes are consistent. Keyed by
    /// (family, number, index) — family separates the param and coefficient number-spaces (see
    /// `FAMILY_PARAM`/`FAMILY_COEFF`). Values are the raw little-endian body bytes.
    param_state: HashMap<(u16, u16, u16), Vec<u8>>,
    tick: u64,
}

/// Wrap a DSP message as the device→host (`USB_RECV_DSP`) frame a consumer would read off IN.
fn wrap_in(dsp_msg: &[u8]) -> ParsedFrame {
    let bytes = frame(USB_RECV_DSP, dsp_msg);
    protocol::parse_frame(&bytes)
        .map(|(f, _)| f)
        .expect("self-built IN frame parses")
}

impl Default for MockTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl MockTransport {
    pub fn new() -> Self {
        let mut m = MockTransport {
            start: Instant::now(),
            last_meter: None,
            meter_period: Duration::from_millis(33), // ~30 fps, like the real stream
            pending: Vec::new(),
            connect_dump: Vec::new(),
            param_state: HashMap::new(),
            tick: 0,
        };
        m.build_connect_dump();
        m
    }

    /// Populate the connect-time parameter VALUE dump (device→host), matching the real device's
    /// behavior of reporting its current parameter state right after the version exchange. Only
    /// **parameters** are reported — the device never dumps the coefficient mix (see PROTOCOL §8).
    /// A couple of phantom channels are pre-set ON so the UI visibly hydrates from device state
    /// rather than from blank defaults.
    fn build_connect_dump(&mut self) {
        use crate::controls::Param;
        // (param, index, bool value). A scattered-but-plausible state so the Inputs grid
        // visibly hydrates from "device truth" rather than all-off defaults.
        let bools: &[(Param, u16, bool)] = &[
            (Param::InputPhantomPower, 0, true),
            (Param::InputPhantomPower, 1, false),
            (Param::InputPhantomPower, 2, true),
            (Param::InputPhantomPower, 3, false),
            (Param::InputHpf, 0, true),
            (Param::InputHpf, 1, false),
            (Param::InputHpf, 2, false),
            (Param::InputHpf, 3, false),
            (Param::InputLineInput, 0, false),
            (Param::InputLineInput, 1, true),
            (Param::InputLineInput, 2, false),
            (Param::InputLineInput, 3, false),
            (Param::InputInstrumentInput, 0, true), // Hi-Z on input 1 (valid only for inputs 1–2)
            (Param::InputInstrumentInput, 1, false),
            (Param::InputPolarity, 0, false),
            (Param::InputPolarity, 1, false),
            (Param::InputPolarity, 2, false),
            (Param::InputPolarity, 3, true), // polarity invert on input 4
            // Monitor-bus params (idx 0) — device-reported, hydrate the Outputs screen.
            (Param::OutputBusMono, 0, false),
            (Param::OutputBusDim, 0, true), // dim engaged
            (Param::OutputBusCut, 0, false),
        ];
        for &(number, index, val) in bools {
            self.param_state
                .insert((FAMILY_PARAM, number.num(), index), vec![val as u8]);
            self.connect_dump.push(wrap_in(&value_message(
                DspCode::ParamValueBool.num(),
                number.num(),
                index,
                &[val as u8],
            )));
        }
    }

    /// Build a synthetic meter table (DSP code 9) wrapped in a `USB_RECV_DSP` frame, with
    /// levels that move so the UI looks alive. Channels are shaped to resemble the real
    /// capture: a couple of analogue inputs active, the monitor/HP buses and Playback 1–2
    /// driven, the rest idle.
    fn meter_frame(&self) -> ParsedFrame {
        let t = self.start.elapsed().as_secs_f64();
        let mut payload = Vec::with_capacity(8 + protocol_meters::NUM * 2);
        payload.extend_from_slice(&DspCode::MeterValueTable15Bit.num().to_le_bytes());
        payload.extend_from_slice(&1u16.to_le_bytes()); // table 1
        payload.extend_from_slice(&0u16.to_le_bytes()); // offset 0
        payload.extend_from_slice(&(protocol_meters::NUM as u16).to_le_bytes());
        for i in 0..protocol_meters::NUM {
            payload.extend_from_slice(&mock_level(i, t).to_le_bytes());
        }
        wrap_in(&payload)
    }

    /// Translate a host UPDATE message into the device→host frame the mock hands back, so the UI's
    /// reconciliation path runs. Two families, kept in distinct namespaces (param `number` and
    /// coefficient `number` overlap — e.g. param 1 = phantom, coeff 1 = crosspoint table):
    ///
    /// * **Parameter** UPDATEs echo as the device's real "+1 odd sibling" VALUE message
    ///   (4→5, 10→11, 16→17, 12→13).
    /// * **Coefficient** UPDATEs (crosspoints, bus levels, …) are host-owned; real hardware emits no
    ///   distinct `*_VALUE` sibling for them, so the mock re-emits the **same** coefficient code
    ///   (6/7/8/18) on the IN stream as a confirmation. A consumer routes device→host coefficient
    ///   codes into the coefficient namespace, avoiding the param/coeff number collision.
    ///
    /// Returns None for messages with no echo (e.g. version requests).
    fn echo_for_update(&mut self, dsp: &[u8]) -> Option<Vec<u8>> {
        if dsp.len() < 6 {
            return None;
        }
        let msg = u16::from_le_bytes([dsp[0], dsp[1]]);
        let number = u16::from_le_bytes([dsp[2], dsp[3]]);
        let index = u16::from_le_bytes([dsp[4], dsp[5]]);
        let body = &dsp[6..];

        // Parameter UPDATE -> the device's +1 VALUE sibling (hardware-faithful).
        let param_value = match DspCode::try_from(msg) {
            Ok(DspCode::ParamUpdateBool) => Some(DspCode::ParamValueBool.num()),
            Ok(DspCode::ParamUpdateSelection) => Some(DspCode::ParamValueSelection.num()),
            Ok(DspCode::ParamUpdateInt) => Some(DspCode::ParamValueInt.num()),
            Ok(DspCode::ParamUpdateQ625) => Some(DspCode::ParamValueQ625.num()),
            _ => None,
        };
        if let Some(vc) = param_value {
            self.param_state
                .insert((FAMILY_PARAM, number, index), body.to_vec());
            return Some(value_message(vc, number, index, body));
        }

        // Coefficient UPDATE -> re-emit the same coefficient code on IN (mock convention).
        let is_coeff = matches!(
            DspCode::try_from(msg),
            Ok(DspCode::CoefficientUpdateBool
                | DspCode::CoefficientUpdateSelection
                | DspCode::CoefficientUpdateInt
                | DspCode::CoefficientUpdateQ625)
        );
        if is_coeff {
            self.param_state
                .insert((FAMILY_COEFF, number, index), body.to_vec());
            return Some(value_message(msg, number, index, body));
        }

        // Value request: serve the stored parameter value (default a single 0 byte).
        if msg == DspCode::ParamValueRequest.num() {
            let v = self
                .param_state
                .get(&(FAMILY_PARAM, number, index))
                .cloned()
                .unwrap_or_else(|| vec![0]);
            return Some(value_message(
                DspCode::ParamValueBool.num(),
                number,
                index,
                &v,
            ));
        }
        None
    }
}

/// Storage-namespace discriminators so param `number` N and coefficient `number` N don't collide.
const FAMILY_PARAM: u16 = 0;
const FAMILY_COEFF: u16 = 1;

/// Assemble a DSP VALUE message: `code | number | index | body`.
fn value_message(code: u16, number: u16, index: u16, body: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(6 + body.len());
    v.extend_from_slice(&code.to_le_bytes());
    v.extend_from_slice(&number.to_le_bytes());
    v.extend_from_slice(&index.to_le_bytes());
    v.extend_from_slice(body);
    v
}

mod protocol_meters {
    pub const NUM: usize = crate::meters::NUM_METERS;
}

/// Deterministic, lively meter level (0..=0x7FFF) for channel `i` at time `t` seconds.
///
/// Models a PPM-style peak meter watching a music mix at 128 BPM:
///   - slow (8 s) song-level envelope for verse/chorus dynamics
///   - beat hits at 128 BPM + eighth-note subdivisions (half-rectified, shaped for sharp attack)
///   - channel-specific harmonic content (two inharmonic sines create amplitude beating)
///   - PPM ballistics: instant attack, ~1.3 s slow decay (look-back peak-hold window)
///
/// Output buses (monitor / HP / playback) share beat timing; analogue inputs are independent.
/// Stereo L/R pairs track the same programme with a small decorrelation nudge.
fn mock_level(i: usize, t: f64) -> u16 {
    use std::f64::consts::PI;

    let driven = matches!(i, 0 | 2 | 12 | 13 | 16 | 17 | 18 | 19 | 20 | 21 | 25);
    if !driven {
        return 0;
    }

    // Stereo pairs share a base phase so L/R track the same programme.
    let partner = match i {
        13 => 12,
        17 => 16,
        19 => 18,
        21 => 20,
        _ => i,
    };
    let ph = partner as f64 * 1.618 + 0.5; // golden-ratio phase spread
    let r_nudge = if i != partner { 0.11 } else { 0.0 }; // tiny L/R decorrelation

    // Output buses all carry the same mix — share beat/song timing.
    let is_output = matches!(i, 12 | 13 | 16 | 17 | 18 | 19 | 20 | 21 | 25);

    let signal_at = |s: f64| -> f64 {
        // 8-second song-level envelope (verse / chorus dynamics).
        let song_ph = if is_output { 0.0 } else { ph * 0.15 };
        let song = 0.50 + 0.50 * (2.0 * PI * 0.125 * s + song_ph).sin();

        // Beat at 128 BPM (2.133 Hz): half-rectified, powf-shaped for a sharp transient hit.
        let beat_ph = if is_output { 0.0 } else { ph };
        let beat = (2.0 * PI * 2.133 * s + beat_ph + r_nudge)
            .sin()
            .max(0.0)
            .powf(0.3);

        // Eighth-note subdivisions (4.267 Hz), quieter.
        let sub = (2.0 * PI * 4.267 * s + beat_ph + PI * 0.5 + r_nudge)
            .sin()
            .max(0.0)
            .powf(0.7)
            * 0.45;

        // Channel-specific harmonic content: two inharmonic sines create amplitude beating.
        let f1 = 5.5 + ph % 3.0;
        let f2 = 8.3 + ph % 2.0;
        let harm = ((2.0 * PI * f1 * s + ph + r_nudge).sin() * 0.55
            + (2.0 * PI * f2 * s + ph + 1.4).sin() * 0.45)
            .abs();

        song * (beat * 0.60 + sub * 0.20 + harm * 0.45).clamp(0.0, 1.0)
    };

    // PPM ballistics: instant attack, slow decay.
    // Look back 1.3 s in 50 ms steps; keep the highest decayed past peak.
    // 0.912^26 ≈ 0.10 → roughly −10 dB in 1.3 s, matching studio PPM release time.
    const STEP: f64 = 0.050;
    const STEPS: usize = 26;
    const DECAY: f64 = 0.912;
    let mut peak = signal_at(t);
    for k in 1..=STEPS {
        let past = signal_at(t - k as f64 * STEP);
        let decayed = past * DECAY.powi(k as i32);
        if decayed > peak {
            peak = decayed;
        }
    }

    // Map [0, 1] → dBFS. Nominal peaks land around −6 to −3 dBFS (yellow zone);
    // loud transients reach 0 dBFS (clipped to 0x7FFF). Noise floor ~−48 dBFS.
    let db = -48.0 + peak * 50.0;
    if db <= -48.0 {
        return 0;
    }
    let linear = 10f64.powf(db.min(0.0) / 20.0);
    (linear * 0x7FFF as f64).round() as u16 & 0x7FFF
}

impl Transport for MockTransport {
    fn poll(&mut self) -> Result<Vec<ParsedFrame>, String> {
        // Connect-time parameter dump first (drained once), then any echoes, then a meter tick.
        let mut out = std::mem::take(&mut self.connect_dump);
        out.append(&mut self.pending);
        let due = self
            .last_meter
            .is_none_or(|t| t.elapsed() >= self.meter_period);
        if due {
            self.last_meter = Some(Instant::now());
            self.tick += 1;
            out.push(self.meter_frame());
        }
        Ok(out)
    }

    fn send_dsp(&mut self, dsp_msg: &[u8]) -> Result<(), String> {
        if let Some(echo) = self.echo_for_update(dsp_msg) {
            self.pending.push(wrap_in(&echo));
        }
        Ok(())
    }

    fn label(&self) -> &'static str {
        "MOCK"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_first_poll_includes_param_connect_dump() {
        let mut m = MockTransport::new();
        let frames = m.poll().unwrap();
        // Phantom input 0 was seeded ON in the dump — the UI should be able to hydrate it.
        let phantom0 = frames.iter().find(|f| {
            f.code == USB_RECV_DSP
                && f.payload.len() >= 7
                && u16::from_le_bytes([f.payload[0], f.payload[1]]) == DspCode::ParamValueBool.num()
                && u16::from_le_bytes([f.payload[2], f.payload[3]]) == 1 // INPUT_PHANTOM_POWER
                && u16::from_le_bytes([f.payload[4], f.payload[5]]) == 0 // index 0
        });
        assert!(phantom0.is_some(), "connect dump should report phantom 0");
        assert_eq!(phantom0.unwrap().payload[6], 1, "phantom 0 should be ON");
        // The dump is drained: a later poll carries no more param-bool frames (only meters).
        let later = m.poll().unwrap();
        assert!(later
            .iter()
            .all(|f| crate::meters::parse(&f.payload).is_some()
                || f.payload.first() != Some(&(DspCode::ParamValueBool.num() as u8))));
    }

    #[test]
    fn mock_emits_meter_frames() {
        let mut m = MockTransport::new();
        // The first poll also carries the connect dump, so find the meter frame among the batch.
        let frames = m.poll().unwrap();
        let upd = frames
            .iter()
            .find_map(|f| crate::meters::parse(&f.payload))
            .expect("a meter update is present");
        assert_eq!(upd.table, 1);
        assert_eq!(upd.samples.len(), crate::meters::NUM_METERS);
    }

    #[test]
    fn mock_echoes_phantom_write_as_value() {
        let mut m = MockTransport::new();
        // Host turns phantom ON for analogue input 0.
        let upd = protocol::dsp_bool(DspCode::ParamUpdateBool.num(), 1, 0, true);
        m.send_dsp(&upd).unwrap();
        // Next poll should include the device's VALUE echo (code 5) plus the meter frame.
        let frames = m.poll().unwrap();
        let echo = frames
            .iter()
            .find(|f| f.payload.first() == Some(&(DspCode::ParamValueBool.num() as u8)))
            .expect("phantom value echo present");
        // code(2) number(2) index(2) value(1)
        assert_eq!(echo.payload[2..4], 1u16.to_le_bytes()); // number = phantom
        assert_eq!(echo.payload[6], 1); // value = on
    }

    #[test]
    fn mock_echoes_crosspoint_coeff_write() {
        let mut m = MockTransport::new();
        // Host writes crosspoint cell 37 (MIXER_CROSSPOINT_TABLE = coeff number 1) to 0 dB.
        let raw = protocol::ZERO_DB_REF;
        let upd = protocol::dsp_q625(DspCode::CoefficientUpdateQ625.num(), 1, 37, raw);
        m.send_dsp(&upd).unwrap();
        // The echo re-emits the SAME coefficient code (6) on IN — NOT a value reply code, so it
        // can't be confused with a parameter that happens to share number 1.
        let frames = m.poll().unwrap();
        let echo = frames
            .iter()
            .find(|f| f.payload.first() == Some(&(DspCode::CoefficientUpdateQ625.num() as u8)))
            .expect("coefficient echo present");
        assert_eq!(echo.payload[2..4], 1u16.to_le_bytes()); // number = crosspoint table
        assert_eq!(echo.payload[4..6], 37u16.to_le_bytes()); // index = cell 37
        assert_eq!(
            i32::from_le_bytes(echo.payload[6..10].try_into().unwrap()),
            raw
        );
    }
}
