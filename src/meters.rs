//! Meter decoding: `meter table` (DSP code 9), the device→host level push.
//!
//! The SSL 12 streams meters continuously on the IN endpoint once connected. Each frame
//! carries the whole table (TableNumber 1, 29 samples). Per sample (u16 LE):
//! bits 0–14 = level (0..32767 = full scale), bit 15 (MSB) = over/clip flag.
//!
//! The index→channel map and the clip-bit semantics were confirmed empirically from
//! captures — see `docs/PROTOCOL.md` §8a.

use std::time::Duration;

use crate::protocol::DspCode;

pub const TABLE_NUMBER: u16 = 1;
pub const NUM_METERS: usize = 29;

/// One decoded meter sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    /// Linear level, 0..=32767 (0x7FFF = full scale).
    pub level: u16,
    /// Over/clip flag (the sample's MSB). Co-occurs with full-scale saturation.
    pub clip: bool,
}

impl Sample {
    /// Approximate dBFS (full scale 0x7FFF = 0 dB). Returns `-inf` at silence.
    pub fn dbfs(&self) -> f64 {
        if self.level == 0 {
            f64::NEG_INFINITY
        } else {
            20.0 * (self.level as f64 / 32767.0).log10()
        }
    }
}

/// Split a raw meter word into level + clip flag.
pub fn decode_sample(word: u16) -> Sample {
    Sample {
        level: word & 0x7FFF,
        clip: word & 0x8000 != 0,
    }
}

/// Peak-hold + clip-latch state for one meter channel, advanced as samples arrive. Models a studio
/// PPM: the held peak jumps up instantly to any new maximum, holds for [`PeakHold::HOLD`], then
/// falls back at [`PeakHold::DECAY_DB_PER_SEC`]. The clip flag **latches** the moment any sample's
/// clip bit is seen and stays lit until [`PeakHold::clear`], so a brief overload can't flash past
/// between frames unnoticed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PeakHold {
    /// Held peak level (0..=0x7FFF).
    pub level: u16,
    /// Latched clip flag (sticky until `clear`).
    pub clipped: bool,
    /// Time accumulated since `level` was last raised — drives the hold-then-decay fallback.
    held: Duration,
}

impl PeakHold {
    /// How long the peak stays pinned at a new maximum before it begins to fall.
    pub const HOLD: Duration = Duration::from_millis(1500);
    /// Fall-back rate once the hold expires (studio-PPM-ish ~12 dB/s).
    pub const DECAY_DB_PER_SEC: f64 = 12.0;

    /// Fold one freshly-decoded sample in, with `dt` elapsed since the previous update.
    pub fn update(&mut self, s: Sample, dt: Duration) {
        self.clipped |= s.clip;
        if s.level >= self.level {
            // New (or equal) maximum: snap up and restart the hold window.
            self.level = s.level;
            self.held = Duration::ZERO;
        } else {
            self.held += dt;
            if self.held > Self::HOLD {
                // Decay in dB: level *= 10^(-(rate·dt)/20). Never fall below the live sample.
                let factor = 10f64.powf(-(Self::DECAY_DB_PER_SEC * dt.as_secs_f64()) / 20.0);
                let decayed = (self.level as f64 * factor).round() as u16;
                self.level = decayed.max(s.level);
            }
        }
    }

    /// Reset the held peak and clear the latched clip (the Meters screen's `c` key).
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// The held peak as a `Sample`, so callers can reuse [`Sample::dbfs`]; `clip` carries the latch.
    pub fn as_sample(&self) -> Sample {
        Sample {
            level: self.level,
            clip: self.clipped,
        }
    }
}

/// A decoded meter update (one code-9 frame).
#[derive(Debug, Clone)]
pub struct MeterUpdate {
    pub table: u16,
    pub offset: u16,
    pub samples: Vec<Sample>,
}

impl MeterUpdate {
    /// Iterate `(absolute_index, label, sample)` for each sample in this update.
    pub fn labelled(&self) -> impl Iterator<Item = (u16, &'static str, Sample)> + '_ {
        self.samples.iter().enumerate().map(move |(i, s)| {
            let idx = self.offset + i as u16;
            (idx, label(idx), *s)
        })
    }
}

/// Parse a meter update from an DSP payload — the bytes *inside* a `USB_RECV_DSP`
/// (0x6C) serial frame, i.e. `ParsedFrame::payload`. Returns `None` if it isn't a
/// `meter table` (code 9) message.
///
/// Layout: `MsgCode(9) | TableNumber | TableOffset | TableSize | samples[Size]` (all u16 LE).
pub fn parse(dsp_payload: &[u8]) -> Option<MeterUpdate> {
    if dsp_payload.len() < 8 {
        return None;
    }
    let msg = u16::from_le_bytes([dsp_payload[0], dsp_payload[1]]);
    if msg != DspCode::MeterValueTable15Bit.num() {
        return None;
    }
    let table = u16::from_le_bytes([dsp_payload[2], dsp_payload[3]]);
    let offset = u16::from_le_bytes([dsp_payload[4], dsp_payload[5]]);
    let size = u16::from_le_bytes([dsp_payload[6], dsp_payload[7]]) as usize;
    let samples = dsp_payload[8..]
        .chunks_exact(2)
        .take(size)
        .map(|c| decode_sample(u16::from_le_bytes([c[0], c[1]])))
        .collect();
    Some(MeterUpdate {
        table,
        offset,
        samples,
    })
}

/// Channel label for a meter index (table 1). Confirmed: analogue inputs (0–3), the four
/// output buses (12–19), Playback 1–2 (20/21), and index 28 = talkback mic (HW-confirmed: holding
/// Talk moves meter 28). ADAT (4–11) and Playback 3–8 (22–27) positions are inferred by elimination.
/// See spec §8a.
pub fn label(index: u16) -> &'static str {
    match index {
        0 => "Analogue 1",
        1 => "Analogue 2",
        2 => "Analogue 3",
        3 => "Analogue 4",
        4 => "ADAT 1",
        5 => "ADAT 2",
        6 => "ADAT 3",
        7 => "ADAT 4",
        8 => "ADAT 5",
        9 => "ADAT 6",
        10 => "ADAT 7",
        11 => "ADAT 8",
        12 => "Monitor L",
        13 => "Monitor R",
        14 => "Line 3-4 L",
        15 => "Line 3-4 R",
        16 => "HP A L",
        17 => "HP A R",
        18 => "HP B L",
        19 => "HP B R",
        20 => "Playback 1",
        21 => "Playback 2",
        22 => "Playback 3",
        23 => "Playback 4",
        24 => "Playback 5",
        25 => "Playback 6",
        26 => "Playback 7",
        27 => "Playback 8",
        28 => "Talkback",
        _ => "(?)",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_clip_and_level() {
        assert_eq!(
            decode_sample(0x7FFF),
            Sample {
                level: 0x7FFF,
                clip: false
            }
        );
        assert_eq!(
            decode_sample(0xFFFF),
            Sample {
                level: 0x7FFF,
                clip: true
            }
        );
        assert_eq!(
            decode_sample(0x0000),
            Sample {
                level: 0,
                clip: false
            }
        );
        assert!(decode_sample(0).dbfs().is_infinite());
        assert!((decode_sample(0x7FFF).dbfs()).abs() < 0.01);
    }

    #[test]
    fn parses_a_table_1_frame() {
        // MsgCode 9, table 1, offset 0, size 2, then two samples: full-scale+clip, then mid.
        let payload = [
            0x09, 0x00, // msg = 9
            0x01, 0x00, // table 1
            0x00, 0x00, // offset 0
            0x02, 0x00, // size 2
            0xFF, 0xFF, // sample 0: level 0x7FFF, clip
            0x00, 0x40, // sample 1: level 0x4000, no clip
        ];
        let u = parse(&payload).expect("meter frame");
        assert_eq!(u.table, 1);
        assert_eq!(u.samples.len(), 2);
        assert!(u.samples[0].clip && u.samples[0].level == 0x7FFF);
        assert!(!u.samples[1].clip && u.samples[1].level == 0x4000);
        assert_eq!(u.labelled().next().unwrap().1, "Analogue 1");
    }

    #[test]
    fn peak_hold_rises_holds_then_decays() {
        let mut p = PeakHold::default();
        let dt = Duration::from_millis(33); // ~30 Hz meter frames

        // Rises instantly to a new maximum.
        p.update(
            Sample {
                level: 0x4000,
                clip: false,
            },
            dt,
        );
        assert_eq!(p.level, 0x4000);

        // A lower sample within the hold window leaves the held peak pinned.
        p.update(
            Sample {
                level: 0x0100,
                clip: false,
            },
            dt,
        );
        assert_eq!(p.level, 0x4000, "held during the hold window");

        // After the hold expires, it falls — but never below the live sample.
        let live = Sample {
            level: 0x0100,
            clip: false,
        };
        p.update(live, PeakHold::HOLD + Duration::from_millis(1)); // cross the hold threshold
        for _ in 0..200 {
            p.update(live, dt);
        }
        assert!(p.level < 0x4000, "decayed from the peak");
        assert!(p.level >= live.level, "never falls below the live level");
    }

    #[test]
    fn peak_hold_latches_and_clears_clip() {
        let mut p = PeakHold::default();
        let dt = Duration::from_millis(33);
        p.update(
            Sample {
                level: 0x7FFF,
                clip: true,
            },
            dt,
        );
        assert!(p.clipped, "clip latches on");
        // Subsequent clean samples must NOT clear the latch.
        for _ in 0..10 {
            p.update(
                Sample {
                    level: 0x0010,
                    clip: false,
                },
                dt,
            );
        }
        assert!(p.clipped, "latch survives clean samples");
        p.clear();
        assert_eq!(p, PeakHold::default(), "clear resets level + latch + hold");
    }

    #[test]
    fn ignores_non_meter() {
        // A bool value (code 5) payload is not a meter frame.
        assert!(parse(&[0x05, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x01]).is_none());
    }
}
