//! Decode USBPcap hex-dump captures into SSL 12 DSP messages — offline, no hardware.
//!
//!   capturedecode <capture.txt>                 # one line per DSP message
//!   capturedecode <capture.txt> --summary       # group by (control, number, index)
//!   capturedecode <capture.txt> --filter CROSS  # only messages whose control matches
//!
//! Mapping the crosspoint table: move ONE mixer cell in SSL 360, capture, then
//!   capturedecode move.txt --filter CROSSPOINT --summary
//! and read off the index that changed.

use std::collections::BTreeMap;
use std::process::exit;

use ssl12_ctl::capture::{Decoded, Value};

fn main() {
    let mut path = None;
    let mut summary = false;
    let mut meters = false;
    let mut trace = false;
    let mut endpoints = false;
    let mut rawframes = false;
    let mut setup = false;
    let mut filter: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--summary" | "-s" => summary = true,
            "--meters" | "-m" => meters = true,
            "--metertrace" | "-t" => trace = true,
            "--endpoints" | "-e" => endpoints = true,
            "--rawframes" | "-r" => rawframes = true,
            "--setup" => setup = true,
            "--filter" | "-f" => filter = args.next(),
            _ if a.starts_with('-') => {
                eprintln!("unknown flag: {a}");
                exit(2);
            }
            _ => path = Some(a),
        }
    }
    let Some(path) = path else {
        eprintln!("usage: capturedecode <capture.txt> [--summary] [--filter <substr>]");
        exit(2);
    };

    if endpoints || rawframes || setup {
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                if endpoints {
                    scan_endpoints(&text);
                } else if setup {
                    dump_setup(&text);
                } else {
                    dump_rawframes(&text);
                }
            }
            Err(e) => {
                eprintln!("cannot read {path}: {e}");
                exit(1);
            }
        }
        return;
    }

    let file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("cannot read {path}: {e}");
            exit(1);
        }
    };
    let mut msgs = match ssl12_ctl::capture::decode_reader(std::io::BufReader::new(file)) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("read error: {e}");
            exit(1);
        }
    };
    if let Some(f) = &filter {
        let f = f.to_uppercase();
        msgs.retain(|m| m.control.to_uppercase().contains(&f));
    }

    if msgs.is_empty() {
        eprintln!("no DSP messages decoded (is this a hex-dump export?)");
        exit(1);
    }

    if trace {
        print_meter_trace(&msgs);
    } else if meters {
        print_meters(&msgs);
    } else if summary {
        print_summary(&msgs);
    } else {
        for m in &msgs {
            println!(
                "{} usb=0x{:02x} {:<26} num={:>3} idx={:>3} {}{}",
                m.direction(),
                m.usb_code,
                m.control,
                m.number.map(|n| n as i32).unwrap_or(-1),
                m.index.map(|i| i as i32).unwrap_or(-1),
                m.value,
                if m.crc_ok { "" } else { "  [BAD CRC]" },
            );
        }
        eprintln!("\n{} messages decoded.", msgs.len());
    }
}

/// Summary grouping: `(direction+control label, msg_code, number, index) → (count, values seen)`.
type SummaryGroups = BTreeMap<(String, u16, i32, i32), (usize, Vec<Value>)>;

/// Group by (direction, control, number, index); show count + value span.
/// This is what makes the index obvious: only the control you touched shows up.
fn print_summary(msgs: &[Decoded]) {
    let mut groups: SummaryGroups = BTreeMap::new();
    for m in msgs {
        let label = if m.usb_code == 0x6b || m.usb_code == 0x6c {
            format!("{} {}", m.direction(), m.control)
        } else {
            format!("{} usb=0x{:02x} {}", m.direction(), m.usb_code, m.control)
        };
        let key = (
            label,
            m.msg_code,
            m.number.map(|n| n as i32).unwrap_or(-1),
            m.index.map(|i| i as i32).unwrap_or(-1),
        );
        let e = groups.entry(key).or_default();
        e.0 += 1;
        e.1.push(m.value.clone());
    }
    println!(
        "{:<32} {:>4} {:>4} {:>6}  value span",
        "direction / control", "num", "idx", "count"
    );
    println!("{}", "-".repeat(78));
    for ((dir_ctrl, _code, num, idx), (count, vals)) in &groups {
        println!(
            "{:<32} {:>4} {:>4} {:>6}  {}",
            dir_ctrl,
            num,
            idx,
            count,
            span(vals),
        );
    }
}

/// Aggregate meter table (code 9) frames: peak level + clip(MSB) seen per meter index.
fn print_meters(msgs: &[Decoded]) {
    use ssl12_ctl::capture::meter_sample;
    let mut peak: BTreeMap<(u16, u16), (u16, bool)> = BTreeMap::new();
    let mut frames = 0usize;
    // Independence check: is MSB a true clip latch, or just == (level==0x7FFF)?
    let mut over_below_full = 0usize; // MSB set but level < 0x7FFF (latch holds after level drops)
    let mut full_without_over = 0usize; // level == 0x7FFF but MSB clear (saturated, not flagged)
    let mut over_min_level = 0x7FFFu16; // lowest level seen on any MSB-set sample
    for m in msgs {
        if let Value::Meter {
            table,
            offset,
            samples,
            ..
        } = &m.value
        {
            frames += 1;
            for (i, &w) in samples.iter().enumerate() {
                let (level, over) = meter_sample(w);
                if over {
                    if level < 0x7FFF {
                        over_below_full += 1;
                    }
                    over_min_level = over_min_level.min(level);
                } else if level == 0x7FFF {
                    full_without_over += 1;
                }
                let e = peak
                    .entry((*table, offset + i as u16))
                    .or_insert((0, false));
                e.0 = e.0.max(level);
                e.1 |= over;
            }
        }
    }
    if frames == 0 {
        eprintln!("no meter table (code 9) frames found — capture with SSL 360 meters active.");
        return;
    }
    println!("{frames} meter frames. Peak level (15-bit) and clip(MSB) per meter index:");
    println!("{:<6} {:>6} {:>8}  clip", "table", "index", "peak");
    println!("{}", "-".repeat(34));
    for ((table, index), (level, over)) in &peak {
        println!(
            "{table:<6} {index:>6} {level:>8}  {}",
            if *over { "CLIP" } else { "" }
        );
    }
    println!("\nMSB independence check:");
    println!("  samples with MSB set but level < 0x7FFF (latch holds): {over_below_full}");
    println!(
        "  samples at level==0x7FFF but MSB clear (saturated, unflagged): {full_without_over}"
    );
    println!("  lowest level on any MSB-set sample: {over_min_level} (0x{over_min_level:04X})");
}

/// Tally which USB endpoint each serial/DSP frame rode on, by reading the
/// USBPcap pseudoheader (endpoint @ off 21, transfer-type @ 22) of each record.
/// Reveals the bulk OUT/IN endpoint addresses for the vendor interface.
fn scan_endpoints(text: &str) {
    use ssl12_ctl::capture::{decode_record, parse_records};
    use std::collections::BTreeMap;
    // key: (endpoint, transfer_type, usb_code) -> count
    let mut tally: BTreeMap<(u8, u8, u8), usize> = BTreeMap::new();
    for rec in parse_records(text) {
        if rec.len() < 23 {
            continue;
        }
        let endpoint = rec[21];
        let transfer = rec[22];
        // Only count records that actually carry a serial frame (FF-led, DSP/other).
        if let Some(d) = decode_record(&rec) {
            *tally.entry((endpoint, transfer, d.usb_code)).or_insert(0) += 1;
        }
    }
    if tally.is_empty() {
        eprintln!("no serial frames found in records.");
        return;
    }
    let ttype = |t: u8| match t {
        0 => "ISO",
        1 => "INT",
        2 => "CTRL",
        3 => "BULK",
        _ => "?",
    };
    println!("serial frames by endpoint (USBPcap pseudoheader):");
    println!(
        "{:>4}  {:>4}  {:>6}  {:>8}  frames",
        "ep", "dir", "xfer", "usbcode"
    );
    for ((ep, xfer, code), n) in &tally {
        let dir = if ep & 0x80 != 0 { "IN" } else { "OUT" };
        println!(
            "0x{ep:02x}  {dir:>4}  {:>6}  0x{code:02x}      {n}",
            ttype(*xfer)
        );
    }
}

/// Dump every serial frame (no DSP-only filter) with endpoint, USB/frame code,
/// DSP msg_code (for DSP frames) and the raw frame body — so USB-layer replies
/// (version queries, etc.) and untargeted DSP messages stay visible.
/// Dump control-transfer SETUP packets (USBPcap control header: 28-byte pseudoheader,
/// then [stage?]+8-byte SETUP at the payload). Reveals the FTDI vendor open sequence.
fn dump_setup(text: &str) {
    use ssl12_ctl::capture::parse_records;
    let ftdi_req = |r: u8| match r {
        0x00 => "RESET",
        0x01 => "MODEM_CTRL",
        0x02 => "SET_FLOW",
        0x03 => "SET_BAUD",
        0x04 => "SET_DATA",
        0x05 => "SET_EVENT_CHAR",
        0x06 => "SET_ERROR_CHAR",
        0x09 => "SET_LATENCY",
        0x90 => "READ_EEPROM",
        _ => "?",
    };
    let mut seen = 0u32;
    for rec in parse_records(text) {
        if rec.len() < 28 || rec[22] != 0x02 || rec[27] != 0x00 {
            continue; // CTRL transfers, SETUP stage only (stage byte at off 27 == 0)
        }
        let hdr_len = u16::from_le_bytes([rec[0], rec[1]]) as usize;
        // The 8-byte SETUP packet is the first 8 bytes of the payload (right after header).
        let data = &rec[hdr_len.min(rec.len())..];
        let s = if data.len() >= 8 {
            &data[..8]
        } else {
            continue;
        };
        let bm = s[0];
        let req = s[1];
        let val = u16::from_le_bytes([s[2], s[3]]);
        let idx = u16::from_le_bytes([s[4], s[5]]);
        let len = u16::from_le_bytes([s[6], s[7]]);
        let kind = match bm & 0x60 {
            0x40 => "VENDOR",
            0x00 => "STD",
            0x20 => "CLASS",
            _ => "?",
        };
        let name = if bm & 0x60 == 0x40 { ftdi_req(req) } else { "" };
        println!("SETUP bmReq=0x{bm:02x}({kind}) bReq=0x{req:02x}{} wValue=0x{val:04x} wIndex=0x{idx:04x} wLen={len}", if name.is_empty() { String::new() } else { format!(" {name}") });
        seen += 1;
        if seen >= 80 {
            break;
        }
    }
    if seen == 0 {
        eprintln!("no control SETUP packets found (capture may be bulk-only).");
    }
}

fn dump_rawframes(text: &str) {
    use ssl12_ctl::capture::{decode_frame, parse_records, Value};
    use ssl12_ctl::protocol::parse_frame;
    // FTDI: each 64-byte IN packet is prefixed with 2 status bytes; strip them before parsing,
    // otherwise multi-packet frames (meters) get status bytes injected mid-frame -> bad CRC.
    fn destatus_in(raw: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < raw.len() {
            let end = (i + 64).min(raw.len());
            if end - i > 2 {
                out.extend_from_slice(&raw[i + 2..end]);
            }
            i = end;
        }
        out
    }
    for rec in parse_records(text) {
        if rec.len() < 23 {
            continue;
        }
        let ep = rec[21];
        let is_in = ep & 0x80 != 0;
        let dir = if is_in { "IN " } else { "OUT" };
        let hdr_len = u16::from_le_bytes([rec[0], rec[1]]) as usize;
        if rec.len() <= hdr_len {
            continue;
        }
        let raw = &rec[hdr_len..];
        let stream = if is_in {
            destatus_in(raw)
        } else {
            raw.to_vec()
        };
        // Parse every serial frame in the (de-statused) stream, in order.
        let mut data = &stream[..];
        while let Some((frame, consumed)) = parse_frame(data) {
            data = &data[consumed..];
            let d = decode_frame(frame.code, &frame.payload, frame.crc_ok);
            let crc = if frame.crc_ok { "" } else { " [BADCRC]" };
            let detail = match &d.value {
                Value::Meter {
                    table,
                    offset,
                    size,
                    ..
                } => {
                    format!("METER table {table} off {offset}/{size}")
                }
                Value::Raw(v) if frame.code != 0x6b && frame.code != 0x6c => {
                    format!("usbcode=0x{:02x} {:02x?}", frame.code, v)
                }
                _ => format!(
                    "{} num={:?} idx={:?} {}",
                    d.control, d.number, d.index, d.value
                ),
            };
            let devaddr = if rec.len() >= 21 {
                u16::from_le_bytes([rec[19], rec[20]])
            } else {
                0
            };
            println!(
                "{dir} dev={devaddr} ep=0x{ep:02x} hl={hdr_len} code=0x{:02x} {detail}{crc}",
                frame.code
            );
        }
    }
}

/// Temporal view: split meter frames into N segments over time and show the
/// peak level per meter index in each segment. Reveals sequential-stimulus order.
fn print_meter_trace(msgs: &[Decoded]) {
    use ssl12_ctl::capture::meter_sample;
    const SEGMENTS: usize = 12;
    const SHOW: u16 = 24; // meter indices to display (input + bus region)
    let frames: Vec<&Decoded> = msgs
        .iter()
        .filter(|m| matches!(m.value, Value::Meter { .. }))
        .collect();
    if frames.is_empty() {
        eprintln!("no meter table (code 9) frames found.");
        return;
    }
    let n = frames.len();
    let seg_len = n.div_ceil(SEGMENTS);
    println!("{n} meter frames, {SEGMENTS} time segments (~{seg_len} frames each).");
    println!(
        "Peak level per meter index per segment (cols = index 0..{}):",
        SHOW - 1
    );
    print!("{:>4} ", "seg");
    for i in 0..SHOW {
        print!("{i:>5}");
    }
    println!();
    for s in 0..SEGMENTS {
        let lo = s * seg_len;
        if lo >= n {
            break;
        }
        let hi = (lo + seg_len).min(n);
        let mut peak = vec![0u16; SHOW as usize];
        for m in &frames[lo..hi] {
            if let Value::Meter {
                offset, samples, ..
            } = &m.value
            {
                for (i, &w) in samples.iter().enumerate() {
                    let idx = *offset as usize + i;
                    if idx < SHOW as usize {
                        let (level, _) = meter_sample(w);
                        peak[idx] = peak[idx].max(level);
                    }
                }
            }
        }
        print!("{s:>4} ");
        for v in &peak {
            print!("{v:>5}");
        }
        println!();
    }
}

fn span(vals: &[Value]) -> String {
    // For Q6.25 show the dB range; otherwise show first/last.
    let dbs: Vec<f64> = vals
        .iter()
        .filter_map(|v| match v {
            Value::Q625 { db, raw } if *raw > 0 => Some(*db),
            _ => None,
        })
        .collect();
    if !dbs.is_empty() {
        let min = dbs.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = dbs.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        return format!("{min:+.2} .. {max:+.2} dB");
    }
    match (vals.first(), vals.last()) {
        (Some(a), Some(b)) if a == b => format!("{a}"),
        (Some(a), Some(b)) => format!("{a} .. {b}"),
        _ => "-".to_string(),
    }
}
