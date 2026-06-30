//! Layers 1–3 of the SSL 12 vendor protocol: serial framing, USB message codes,
//! and DSP message serialization. All multi-byte fields are little-endian.
//!
//! Frame:  FF | Code | Len | Payload[Len] | CRC
//!         CRC = (Code + Len + Σ Payload) & 0xFF   (the 0xFF start byte is excluded)

/// Serial-frame start-of-frame sync byte.
pub const START_CODE: u8 = 0xFF;

/// USB message codes (layer 2). DSP/mixer traffic rides on these codes.
pub const USB_SEND_DSP: u8 = 0x6B; // 107, host -> device
pub const USB_RECV_DSP: u8 = 0x6C; // 108, device -> host
pub const USB_REQUEST_CONTROL_STATES: u8 = 0x2B; // 43, bulk state dump
pub const USB_GET_IS_TILE: u8 = 0x01; // 1, tile-init query
pub const USB_GET_TILE_ID: u8 = 0x02; // 2, tile-init query
pub const USB_INIT_TILE: u8 = 0x05; // 5, initialize the control session (empty payload)
pub const USB_RECONNECT_REQUIRED: u8 = 0x06; // 6, device->host: "you're not properly connected"
pub const USB_GET_SOFTWARE_VERSION_INT: u8 = 0x4B; // 75, device replies on IN with a u32
pub const USB_GET_HW_VERSION: u8 = 0x4E; // 78, device replies on IN with a u16

/// DSP message codes (layer 3) — the `code` field of a [`DspMessage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum DspCode {
    None = 0,
    RequestProtocolVersion = 1,
    ProtocolVersionInformation = 2,
    ParamValueRequest = 3,
    ParamUpdateBool = 4,
    ParamValueBool = 5,
    CoefficientUpdateQ625 = 6,
    CoefficientUpdateBool = 7,
    CoefficientUpdateSelection = 8,
    MeterValueTable15Bit = 9,
    ParamUpdateSelection = 10,
    ParamValueSelection = 11,
    ParamUpdateQ625 = 12,
    ParamValueQ625 = 13,
    RequestDspVersion = 14,
    DspVersionInformation = 15,
    ParamUpdateInt = 16,
    ParamValueInt = 17,
    CoefficientUpdateInt = 18,
}

impl DspCode {
    /// The DSP message code carried on the wire.
    pub const fn num(self) -> u16 {
        self as u16
    }

    /// Human-readable label for the message code (descriptive, for diagnostics).
    pub const fn name(self) -> &'static str {
        match self {
            DspCode::None => "none",
            DspCode::RequestProtocolVersion => "request protocol version",
            DspCode::ProtocolVersionInformation => "protocol version reply",
            DspCode::ParamValueRequest => "value request",
            DspCode::ParamUpdateBool => "bool update",
            DspCode::ParamValueBool => "bool value",
            DspCode::CoefficientUpdateQ625 => "Q6.25 coeff update",
            DspCode::CoefficientUpdateBool => "bool coeff update",
            DspCode::CoefficientUpdateSelection => "selection coeff update",
            DspCode::MeterValueTable15Bit => "meter table (15-bit)",
            DspCode::ParamUpdateSelection => "selection update",
            DspCode::ParamValueSelection => "selection value",
            DspCode::ParamUpdateQ625 => "Q6.25 update",
            DspCode::ParamValueQ625 => "Q6.25 value",
            DspCode::RequestDspVersion => "request DSP version",
            DspCode::DspVersionInformation => "DSP version reply",
            DspCode::ParamUpdateInt => "int update",
            DspCode::ParamValueInt => "int value",
            DspCode::CoefficientUpdateInt => "int coeff update",
        }
    }
}

impl TryFrom<u16> for DspCode {
    /// The unrecognized code, on failure.
    type Error = u16;

    fn try_from(code: u16) -> Result<Self, Self::Error> {
        Ok(match code {
            0 => DspCode::None,
            1 => DspCode::RequestProtocolVersion,
            2 => DspCode::ProtocolVersionInformation,
            3 => DspCode::ParamValueRequest,
            4 => DspCode::ParamUpdateBool,
            5 => DspCode::ParamValueBool,
            6 => DspCode::CoefficientUpdateQ625,
            7 => DspCode::CoefficientUpdateBool,
            8 => DspCode::CoefficientUpdateSelection,
            9 => DspCode::MeterValueTable15Bit,
            10 => DspCode::ParamUpdateSelection,
            11 => DspCode::ParamValueSelection,
            12 => DspCode::ParamUpdateQ625,
            13 => DspCode::ParamValueQ625,
            14 => DspCode::RequestDspVersion,
            15 => DspCode::DspVersionInformation,
            16 => DspCode::ParamUpdateInt,
            17 => DspCode::ParamValueInt,
            18 => DspCode::CoefficientUpdateInt,
            other => return Err(other),
        })
    }
}

/// Build a complete serial frame around a payload for the given USB message code.
pub fn frame(code: u8, payload: &[u8]) -> Vec<u8> {
    assert!(
        payload.len() <= u8::MAX as usize,
        "payload too long for u8 length field"
    );
    let len = payload.len() as u8;
    let mut crc = code.wrapping_add(len);
    for &b in payload {
        crc = crc.wrapping_add(b);
    }
    let mut out = Vec::with_capacity(payload.len() + 4);
    out.push(START_CODE);
    out.push(code);
    out.push(len);
    out.extend_from_slice(payload);
    out.push(crc);
    out
}

/// A parsed serial frame (e.g. read from the IN endpoint).
#[derive(Debug, Clone)]
pub struct ParsedFrame {
    pub code: u8,
    pub payload: Vec<u8>,
    pub crc_ok: bool,
}

/// Parse one frame starting at the first 0xFF in `buf`. Returns the frame and the
/// number of bytes consumed, or None if there isn't a complete frame yet.
pub fn parse_frame(buf: &[u8]) -> Option<(ParsedFrame, usize)> {
    let start = buf.iter().position(|&b| b == START_CODE)?;
    let rest = &buf[start..];
    if rest.len() < 4 {
        return None; // FF, code, len, crc minimum
    }
    let code = rest[1];
    let len = rest[2] as usize;
    let total = 3 + len + 1; // header + payload + crc
    if rest.len() < total {
        return None;
    }
    let payload = rest[3..3 + len].to_vec();
    let mut crc = code.wrapping_add(rest[2]);
    for &b in &payload {
        crc = crc.wrapping_add(b);
    }
    let crc_ok = crc == rest[3 + len];
    Some((
        ParsedFrame {
            code,
            payload,
            crc_ok,
        },
        start + total,
    ))
}

/// The typed value carried by a [`DspMessage`]. Owned (not a borrowed slice) so one type can both
/// build and parse a message — the body's byte width follows the variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DspValue {
    /// No value body (e.g. a `value request`).
    None,
    Bool(bool),
    Selection(u16),
    Int(i32),
    /// A Q6.25 fixed-point coefficient as its raw 32-bit word (see [`q625_to_db`]).
    Q625(i32),
}

/// A DSP message in the `code | number | index | value` family — the DSP body of a
/// USB_(SEND|RECV)_DSP frame. Owns its [`DspValue`], so the same type serializes
/// ([`to_bytes`](Self::to_bytes)) and parses ([`parse`](Self::parse)).
///
/// Two DSP shapes are deliberately NOT covered here (they aren't number/index/value messages):
///   * **Meters** (`meter table`, code 9) — `table | offset | size | samples`; see
///     [`crate::meters::parse`].
///   * **Version info** (codes 1/2/14/15) — `code | u16`, only 4 bytes, the value where `number`
///     would be. The `< 6` length guard in [`parse`](Self::parse) excludes them; the connect
///     handshake decodes those itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DspMessage {
    pub code: u16,
    pub number: u16,
    pub index: u16,
    pub value: DspValue,
}

impl DspMessage {
    /// Parse one DSP body (the bytes inside a USB_RECV_DSP frame, i.e. `ParsedFrame::payload`).
    /// Returns `None` for too-short input, a meter frame, or a body too short for its code's value
    /// (a truncated message is skipped, never decoded as a bogus `false`/`0`).
    pub fn parse(dsp: &[u8]) -> Option<Self> {
        use DspCode as C;
        if dsp.len() < 6 {
            return None;
        }
        let code = u16::from_le_bytes([dsp[0], dsp[1]]);
        let number = u16::from_le_bytes([dsp[2], dsp[3]]);
        let index = u16::from_le_bytes([dsp[4], dsp[5]]);
        let body = &dsp[6..];

        let value = match DspCode::try_from(code) {
            // Meters carry a different layout — meters::parse owns them.
            Ok(C::MeterValueTable15Bit) => return None,
            Ok(C::ParamUpdateBool | C::ParamValueBool | C::CoefficientUpdateBool) => {
                DspValue::Bool(*body.first()? != 0)
            }
            Ok(
                C::ParamUpdateSelection | C::ParamValueSelection | C::CoefficientUpdateSelection,
            ) => DspValue::Selection(u16::from_le_bytes(body.get(..2)?.try_into().ok()?)),
            Ok(C::ParamUpdateQ625 | C::ParamValueQ625 | C::CoefficientUpdateQ625) => {
                DspValue::Q625(i32::from_le_bytes(body.get(..4)?.try_into().ok()?))
            }
            Ok(C::ParamUpdateInt | C::ParamValueInt | C::CoefficientUpdateInt) => {
                DspValue::Int(i32::from_le_bytes(body.get(..4)?.try_into().ok()?))
            }
            // value request, unknown codes, and any code that carries no value body.
            _ => DspValue::None,
        };

        Some(Self {
            code,
            number,
            index,
            value,
        })
    }

    /// Serialize to the DSP body bytes (`code | number | index | value`); `None` writes no body.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(10);
        v.extend_from_slice(&self.code.to_le_bytes());
        v.extend_from_slice(&self.number.to_le_bytes());
        v.extend_from_slice(&self.index.to_le_bytes());
        match self.value {
            DspValue::None => {}
            DspValue::Bool(b) => v.push(b as u8),
            DspValue::Selection(s) => v.extend_from_slice(&s.to_le_bytes()),
            DspValue::Int(i) | DspValue::Q625(i) => v.extend_from_slice(&i.to_le_bytes()),
        }
        v
    }

    /// A `value request` (code 3): ask the device for one parameter's current value. No body.
    pub fn value_request(number: u16, index: u16) -> Self {
        Self {
            code: DspCode::ParamValueRequest.num(),
            number,
            index,
            value: DspValue::None,
        }
    }

    pub fn bool(msg_code: u16, number: u16, index: u16, value: bool) -> Self {
        Self {
            code: msg_code,
            number,
            index,
            value: DspValue::Bool(value),
        }
    }

    pub fn selection(msg_code: u16, number: u16, index: u16, selection: u16) -> Self {
        Self {
            code: msg_code,
            number,
            index,
            value: DspValue::Selection(selection),
        }
    }

    pub fn int(msg_code: u16, number: u16, index: u16, value: i32) -> Self {
        Self {
            code: msg_code,
            number,
            index,
            value: DspValue::Int(value),
        }
    }

    pub fn q625(msg_code: u16, number: u16, index: u16, raw: i32) -> Self {
        Self {
            code: msg_code,
            number,
            index,
            value: DspValue::Q625(raw),
        }
    }
}

// The `dsp_*` builders are thin wrappers over `DspMessage` so the DSP byte layout lives in exactly
// one place (`DspMessage::to_bytes`). Retained for call-site ergonomics; the byte-exact tests below
// (and the existing capture-anchored ones) pin their output.

/// A bare DSP message with only a message code (e.g. version requests). Not a number/index message,
/// so it stays outside `DspMessage` (which is the `code | number | index | value` family).
pub fn dsp_bare(msg_code: u16) -> Vec<u8> {
    msg_code.to_le_bytes().to_vec()
}

/// A `value request` (code 3): ask the device to send back the current value of
/// one parameter (number, index). No value field.
pub fn dsp_value_request(number: u16, index: u16) -> Vec<u8> {
    DspMessage::value_request(number, index).to_bytes()
}

pub fn dsp_bool(msg_code: u16, number: u16, index: u16, value: bool) -> Vec<u8> {
    DspMessage::bool(msg_code, number, index, value).to_bytes()
}

pub fn dsp_selection(msg_code: u16, number: u16, index: u16, selection: u16) -> Vec<u8> {
    DspMessage::selection(msg_code, number, index, selection).to_bytes()
}

pub fn dsp_int(msg_code: u16, number: u16, index: u16, value: i32) -> Vec<u8> {
    DspMessage::int(msg_code, number, index, value).to_bytes()
}

pub fn dsp_q625(msg_code: u16, number: u16, index: u16, raw: i32) -> Vec<u8> {
    DspMessage::q625(msg_code, number, index, raw).to_bytes()
}

// ---- Q6.25 fixed-point (gains / faders / crosspoints) ----

pub const Q625_FRAC_BITS: u32 = 25;
/// Device "0 dB" reference coefficient (= 1/√2 ≈ 0.7071 in Q6.25).
pub const ZERO_DB_REF: i32 = 0x016A_09E6;

/// Linear amplitude coefficient -> Q6.25 raw int.
pub fn coeff_to_q625(coeff: f64) -> i32 {
    (coeff * (1u64 << Q625_FRAC_BITS) as f64).round() as i32
}

/// Q6.25 raw int -> linear amplitude coefficient.
pub fn q625_to_coeff(raw: i32) -> f64 {
    raw as f64 / (1u64 << Q625_FRAC_BITS) as f64
}

/// dB (relative to the device 0 dB reference) -> Q6.25 raw int.
/// `-inf`/very low returns 0 (mute).
pub fn db_to_q625(db: f64) -> i32 {
    if !db.is_finite() || db <= -150.0 {
        return 0;
    }
    (ZERO_DB_REF as f64 * 10f64.powf(db / 20.0)).round() as i32
}

/// Q6.25 raw int -> dB relative to the device 0 dB reference.
pub fn q625_to_db(raw: i32) -> f64 {
    if raw <= 0 {
        return f64::NEG_INFINITY;
    }
    20.0 * (raw as f64 / ZERO_DB_REF as f64).log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fader_0db_frame_matches_capture() {
        // analogue-1 crosspoint @ 0 dB: Q6.25 coeff update, number 1, index 10
        let dsp = dsp_q625(DspCode::CoefficientUpdateQ625.num(), 1, 10, ZERO_DB_REF);
        let f = frame(USB_SEND_DSP, &dsp);
        assert_eq!(
            f,
            vec![
                0xFF, 0x6B, 0x0A, 0x06, 0x00, 0x01, 0x00, 0x0A, 0x00, 0xE6, 0x09, 0x6A, 0x01, 0xE0
            ]
        );
    }

    #[test]
    fn phantom_on_input1_frame() {
        let dsp = dsp_bool(DspCode::ParamUpdateBool.num(), 1, 0, true);
        let f = frame(USB_SEND_DSP, &dsp);
        assert_eq!(
            f,
            vec![0xFF, 0x6B, 0x07, 0x04, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x78]
        );
    }

    #[test]
    fn dsp_version_request_crc_is_7b() {
        let f = frame(USB_SEND_DSP, &dsp_bare(DspCode::RequestDspVersion.num()));
        assert_eq!(f, vec![0xFF, 0x6B, 0x02, 0x0E, 0x00, 0x7B]);
    }

    #[test]
    fn db_roundtrip() {
        assert_eq!(db_to_q625(0.0), ZERO_DB_REF);
        assert!((q625_to_db(ZERO_DB_REF)).abs() < 1e-6);
        assert!((q625_to_db(0x0072_7C97) - -10.0).abs() < 0.05);
        assert!((q625_to_db(0x0024_3431) - -20.0).abs() < 0.05);
    }

    #[test]
    fn dsp_message_round_trips_each_value_type() {
        use DspCode::*;
        let cases = [
            DspMessage::bool(ParamUpdateBool.num(), 1, 0, true),
            DspMessage::bool(CoefficientUpdateBool.num(), 31, 0, false),
            DspMessage::selection(CoefficientUpdateSelection.num(), 16, 4, 2),
            DspMessage::q625(CoefficientUpdateQ625.num(), 1, 10, ZERO_DB_REF),
            DspMessage::int(ParamUpdateInt.num(), 12, 0, -7),
            DspMessage::value_request(1, 3),
        ];
        for m in cases {
            assert_eq!(
                DspMessage::parse(&m.to_bytes()),
                Some(m),
                "build → to_bytes → parse: {m:?}"
            );
        }
    }

    #[test]
    fn dsp_builders_delegate_to_dsp_message() {
        // The free `dsp_*` wrappers must emit exactly the same bytes as `DspMessage::to_bytes`,
        // so the byte layout has a single source of truth.
        assert_eq!(
            dsp_bool(DspCode::ParamUpdateBool.num(), 1, 0, true),
            DspMessage::bool(DspCode::ParamUpdateBool.num(), 1, 0, true).to_bytes(),
        );
        assert_eq!(
            dsp_q625(DspCode::CoefficientUpdateQ625.num(), 1, 10, ZERO_DB_REF),
            DspMessage::q625(DspCode::CoefficientUpdateQ625.num(), 1, 10, ZERO_DB_REF).to_bytes(),
        );
        assert_eq!(
            dsp_value_request(1, 3),
            DspMessage::value_request(1, 3).to_bytes(),
        );
    }

    #[test]
    fn dsp_message_parse_rejects_meters_versions_and_truncated() {
        // Header incomplete (< 6 bytes) — also how version-info replies (4 bytes) are excluded.
        assert!(DspMessage::parse(&[0x04, 0x00, 0x01]).is_none());
        // A meter frame (code 9) is owned by meters::parse, not DspMessage.
        let meter = [0x09, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00];
        assert!(DspMessage::parse(&meter).is_none());
        // A bool-coded message with no value byte is rejected, not decoded as `false`.
        let truncated_bool = [0x04, 0x00, 0x01, 0x00, 0x00, 0x00];
        assert!(DspMessage::parse(&truncated_bool).is_none());
    }

    #[test]
    fn dsp_message_parses_an_existing_capture_frame() {
        // Anchor parse to the same bytes the build test pins: phantom-on input 1.
        let body = dsp_bool(DspCode::ParamUpdateBool.num(), 1, 0, true);
        let m = DspMessage::parse(&body).expect("parses");
        assert_eq!(m.code, DspCode::ParamUpdateBool.num());
        assert_eq!((m.number, m.index), (1, 0));
        assert_eq!(m.value, DspValue::Bool(true));
    }
}
