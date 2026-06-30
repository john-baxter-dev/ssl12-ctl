//! Offline decoding of USBPcap **hex-dump text** captures into SSL 12 DSP messages.
//!
//! Workflow for mapping unknowns (e.g. the 240 crosspoint cells): in SSL 360 move ONE
//! control, capture to a text hex dump (the `0000  ab cd ..  ascii` format), then run
//! `capturedecode` — the only writes you'll see are the control you moved, with its exact
//! `(control, number, index)` and value.
//!
//! Note: this parses the column-formatted USBPcap/Wireshark "Hex Dump" export, not the
//! verbose per-field text export.

use crate::controls;
use crate::protocol::{self};

/// One decoded host<->device DSP message.
#[derive(Debug, Clone)]
pub struct Decoded {
    pub usb_code: u8, // 0x6B = host->device (send), 0x6C = device->host (recv)
    pub msg_code: u16,
    pub number: Option<u16>,
    pub index: Option<u16>,
    pub control: &'static str,
    pub value: Value,
    pub crc_ok: bool,
}

impl Decoded {
    pub fn direction(&self) -> &'static str {
        match self.usb_code {
            protocol::USB_SEND_DSP => "TX",
            protocol::USB_RECV_DSP => "RX",
            _ => "??",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    None,
    Bool(bool),
    Selection(u16),
    Int(i32),
    Q625 {
        raw: i32,
        db: f64,
    },
    /// meter table (code 9): a chunk of a meter table starting at `offset`.
    /// Each sample is a 16-bit word; low 15 bits = level, MSB = candidate over/clip flag.
    Meter {
        table: u16,
        offset: u16,
        size: u16,
        samples: Vec<u16>,
    },
    Raw(Vec<u8>),
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::None => write!(f, "-"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Selection(s) => write!(f, "sel={s}"),
            Value::Int(i) => write!(f, "{i}"),
            Value::Q625 { raw, db } => {
                if raw <= &0 {
                    write!(f, "0x{:08x} (-inf dB)", *raw as u32)
                } else {
                    write!(f, "0x{:08x} ({:+.2} dB)", *raw as u32, db)
                }
            }
            Value::Meter {
                table,
                offset,
                size,
                samples,
            } => {
                write!(
                    f,
                    "meter table {table} off {offset}/{size} ({} samples)",
                    samples.len()
                )
            }
            Value::Raw(v) => write!(f, "{v:02x?}"),
        }
    }
}

/// Split a 15-bit meter word into (level 0..=32767, over/clip MSB).
pub fn meter_sample(word: u16) -> (u16, bool) {
    (word & 0x7FFF, word & 0x8000 != 0)
}

/// Split a USBPcap hex-dump text file into one byte-record per packet block
/// (blocks are separated by blank lines).
pub fn parse_records(text: &str) -> Vec<Vec<u8>> {
    let mut records = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            if !cur.is_empty() {
                records.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if let Some(bytes) = parse_hexdump_line(line) {
            cur.extend_from_slice(&bytes);
        }
    }
    if !cur.is_empty() {
        records.push(cur);
    }
    records
}

/// Parse one `OFFSET␣␣HH HH …␣␣␣ASCII` line into its data bytes.
/// Restricts to the hex column window so the ASCII gutter can't leak in.
fn parse_hexdump_line(line: &str) -> Option<Vec<u8>> {
    let off = line.split_whitespace().next()?;
    if off.is_empty() || !off.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None; // not a hex-dump data line (e.g. a Wireshark header line)
    }
    // 4-digit offset + 2 spaces = col 6; 16 bytes * 3 chars = col 54.
    let bytes = line.as_bytes();
    let start = 6.min(bytes.len());
    let end = 54.min(bytes.len());
    let region = std::str::from_utf8(&bytes[start..end]).ok()?;
    let mut out = Vec::new();
    for tok in region.split_whitespace() {
        if tok.len() == 2 && tok.bytes().all(|b| b.is_ascii_hexdigit()) {
            out.push(u8::from_str_radix(tok, 16).ok()?);
        }
    }
    Some(out)
}

/// Decode a full USBPcap record (pseudoheader + payload) into a DSP message, if it is one.
pub fn decode_record(record: &[u8]) -> Option<Decoded> {
    if record.len() < 2 {
        return None;
    }
    // The USBPcap pseudoheader length is the leading u16; the USB payload follows it.
    let hdr_len = u16::from_le_bytes([record[0], record[1]]) as usize;
    if record.len() <= hdr_len {
        return None;
    }
    let payload = &record[hdr_len..];
    let (frame, _) = protocol::parse_frame(payload)?;
    Some(decode_frame(frame.code, &frame.payload, frame.crc_ok))
}

/// Decode a serial frame. Only DSP frames (0x6B/0x6C) carry DSP messages; any other
/// USB message code (LED/status/handshake/etc.) is returned with its raw payload so it
/// can still be identified, without guessing a DSP structure that isn't there.
pub fn decode_frame(usb_code: u8, sp: &[u8], crc_ok: bool) -> Decoded {
    if usb_code != protocol::USB_SEND_DSP && usb_code != protocol::USB_RECV_DSP {
        return Decoded {
            usb_code,
            msg_code: 0,
            number: None,
            index: None,
            control: "(non-DSP usb frame)",
            value: Value::Raw(sp.to_vec()),
            crc_ok,
        };
    }
    if sp.len() < 2 {
        return Decoded {
            usb_code,
            msg_code: 0,
            number: None,
            index: None,
            control: "(non-DSP)",
            value: Value::Raw(sp.to_vec()),
            crc_ok,
        };
    }
    let msg_code = u16::from_le_bytes([sp[0], sp[1]]);
    let has_ni = msg_has_number_index(msg_code) && sp.len() >= 6;
    let (number, index, body) = if has_ni {
        (
            Some(u16::from_le_bytes([sp[2], sp[3]])),
            Some(u16::from_le_bytes([sp[4], sp[5]])),
            &sp[6..],
        )
    } else {
        (None, None, &sp[2..])
    };
    Decoded {
        usb_code,
        msg_code,
        number,
        index,
        control: control_name(msg_code, number),
        value: decode_value(msg_code, body),
        crc_ok,
    }
}

fn msg_has_number_index(code: u16) -> bool {
    matches!(
        code,
        3 | 4 | 5 | 6 | 7 | 8 | 10 | 11 | 12 | 13 | 16 | 17 | 18
    )
}

fn is_coeff(code: u16) -> bool {
    matches!(code, 6 | 7 | 8 | 18)
}

fn control_name(msg_code: u16, number: Option<u16>) -> &'static str {
    if msg_code == crate::protocol::DspCode::MeterValueTable15Bit.num() {
        return "meter table (code 9)";
    }
    match number {
        Some(n) if is_coeff(msg_code) => controls::coeff_name(n),
        Some(n) => controls::param_name(n),
        None => "(no-target)",
    }
}

fn decode_value(code: u16, body: &[u8]) -> Value {
    match code {
        4 | 5 | 7 => body
            .first()
            .map(|&b| Value::Bool(b != 0))
            .unwrap_or(Value::None),
        8 | 10 | 11 => {
            if body.len() >= 2 {
                Value::Selection(u16::from_le_bytes([body[0], body[1]]))
            } else {
                Value::None
            }
        }
        16..=18 => {
            if body.len() >= 4 {
                Value::Int(i32::from_le_bytes([body[0], body[1], body[2], body[3]]))
            } else {
                Value::None
            }
        }
        6 | 12 | 13 => {
            if body.len() >= 4 {
                let raw = i32::from_le_bytes([body[0], body[1], body[2], body[3]]);
                Value::Q625 {
                    raw,
                    db: protocol::q625_to_db(raw),
                }
            } else {
                Value::None
            }
        }
        9 => {
            // meter table (code 9): TableNumber, TableOffset, TableSize, then u16 samples.
            if body.len() >= 6 {
                let table = u16::from_le_bytes([body[0], body[1]]);
                let offset = u16::from_le_bytes([body[2], body[3]]);
                let size = u16::from_le_bytes([body[4], body[5]]);
                let samples = body[6..]
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect();
                Value::Meter {
                    table,
                    offset,
                    size,
                    samples,
                }
            } else {
                Value::None
            }
        }
        _ if body.is_empty() => Value::None,
        _ => Value::Raw(body.to_vec()),
    }
}

/// Decode every DSP message in a hex-dump capture file's text.
pub fn decode_text(text: &str) -> Vec<Decoded> {
    parse_records(text)
        .iter()
        .filter_map(|r| decode_record(r))
        .collect()
}

/// Stream a (possibly multi-GB) hex-dump capture line by line, keeping only DSP frames.
/// Bounds memory to the handful of control messages regardless of capture size.
pub fn decode_reader<R: std::io::BufRead>(reader: R) -> std::io::Result<Vec<Decoded>> {
    let mut out = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    let flush = |cur: &mut Vec<u8>, out: &mut Vec<Decoded>| {
        if !cur.is_empty() {
            if let Some(d) = decode_record(cur) {
                if d.usb_code == protocol::USB_SEND_DSP || d.usb_code == protocol::USB_RECV_DSP {
                    out.push(d);
                }
            }
            cur.clear();
        }
    };
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            flush(&mut cur, &mut out);
        } else if let Some(bytes) = parse_hexdump_line(&line) {
            cur.extend_from_slice(&bytes);
        }
    }
    flush(&mut cur, &mut out);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
0000  1b 00 50 90 47 de 8a de ff ff 5f 53 54 41 09 00   ..P.G....._STA..
0010  00 06 00 03 00 02 03 0e 00 00 00 ff 6b 0a 06 00   ............k...
0020  01 00 0a 00 e6 09 6a 01 e0                        ......j..
";

    #[test]
    fn decodes_crosspoint_0db() {
        let d = decode_text(SAMPLE);
        assert_eq!(d.len(), 1);
        let m = &d[0];
        assert_eq!(m.direction(), "TX");
        assert_eq!(m.control, "MIXER_CROSSPOINT_TABLE");
        assert_eq!(m.number, Some(1));
        assert_eq!(m.index, Some(10));
        assert!(m.crc_ok);
        match m.value {
            Value::Q625 { raw, db } => {
                assert_eq!(raw, 0x016A_09E6);
                assert!(db.abs() < 1e-6);
            }
            _ => panic!("expected Q6.25"),
        }
    }
}
