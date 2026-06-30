//! SSL 12 monitor-mixer model: the `MIXER_CROSSPOINT_TABLE` (coeff 1) is a flat 240-cell
//! gain matrix addressed as `index = destination_block*30 + source_slot`.
//!
//! Slot/block labels were recovered empirically from captures (`cap_slots.txt`,
//! `cap_dests.txt`) decoded with `capturedecode`. See `docs/PROTOCOL.md` §9b.

pub const SLOTS_PER_BLOCK: u16 = 30;
/// Total `MIXER_CROSSPOINT_TABLE` cells (8 destination blocks × 30 slots).
pub const NUM_CELLS: usize = 240;

/// A stereo output (a pair of destination blocks: left leg, right leg).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Destination {
    pub name: &'static str,
    pub left_block: u16,
    pub right_block: u16,
}

pub const MAIN: Destination = Destination {
    name: "Main L-R",
    left_block: 0,
    right_block: 1,
};
pub const LINE_3_4: Destination = Destination {
    name: "Line 3-4",
    left_block: 2,
    right_block: 3,
};
pub const HP_A: Destination = Destination {
    name: "HP A",
    left_block: 4,
    right_block: 5,
};
pub const HP_B: Destination = Destination {
    name: "HP B",
    left_block: 6,
    right_block: 7,
};

pub const DESTINATIONS: [Destination; 4] = [MAIN, LINE_3_4, HP_A, HP_B];

/// A mixer source. Mono sources are one slot (panned across L/R); stereo sources occupy
/// a slot pair whose left leg feeds the destination's left block and right leg the right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Mono {
        name: &'static str,
        slot: u16,
    },
    Stereo {
        name: &'static str,
        left: u16,
        right: u16,
    },
}

impl Source {
    pub fn name(&self) -> &'static str {
        match self {
            Source::Mono { name, .. } | Source::Stereo { name, .. } => name,
        }
    }
}

// Source slots, confirmed from cap_slots2.txt + cap_pb.txt (the latter, an isolated
// PB5-6 vs PB7-8 capture, pinned the playback pairs). The 4 stereo playback returns are
// contiguous at slots 0..7; the 4 mono analogues at 8..11.
pub const PLAYBACK_1_2: Source = Source::Stereo {
    name: "Playback 1-2",
    left: 0,
    right: 1,
};
pub const PLAYBACK_3_4: Source = Source::Stereo {
    name: "Playback 3-4",
    left: 2,
    right: 3,
};
pub const PLAYBACK_5_6: Source = Source::Stereo {
    name: "Playback 5-6",
    left: 4,
    right: 5,
};
pub const PLAYBACK_7_8: Source = Source::Stereo {
    name: "Playback 7-8",
    left: 6,
    right: 7,
};
pub const ANALOGUE_1: Source = Source::Mono {
    name: "Analogue 1",
    slot: 8,
};
pub const ANALOGUE_2: Source = Source::Mono {
    name: "Analogue 2",
    slot: 9,
};
pub const ANALOGUE_3: Source = Source::Mono {
    name: "Analogue 3",
    slot: 10,
};
pub const ANALOGUE_4: Source = Source::Mono {
    name: "Analogue 4",
    slot: 11,
};
// NOTE: Talkback is NOT a crosspoint source — its fader writes TALKBACK_LEVEL (coeff 8),
// a separate injection into the monitor bus, not a MIXER_CROSSPOINT_TABLE cell.

/// dB by which **full scale** sits above the device's **0 dB coefficient reference** (1/√2 =
/// `protocol::ZERO_DB_REF`): `20·log₁₀(1 / (1/√2)) ≈ +3.01 dB`. A leg's coefficient dB (what
/// `db_to_q625` quantizes) is its absolute fader gain plus this offset plus the pan-law leg term.
/// This single constant subsumes the old "stereo +3.01 / mono 0" split: a stereo leg at fader unity
/// is full amplitude (`+3.01`), while a *centered* mono leg is −3.01 dB from the equal-power pan and
/// so lands exactly on the device reference (`0`) — matching the §4 "analogue-1 fader → 0 dB" capture.
/// Uses the exact `20·log₁₀(√2)` so that cancellation is precise (not the rounded 3.01 from captures).
pub const DEVICE_REF_OFFSET_DB: f64 = 3.010_299_956_639_812;

// ---- Pan / balance law -----------------------------------------------------------------
//
// A cell carries a fader gain *and* a pan position in `-1.0..=1.0` (−1 hard left, 0 center,
// +1 hard right). The leg coefficient = fader gain + `DEVICE_REF_OFFSET_DB` + the pan leg term.
// Pan distributes across the destination's two legs:
//   * **mono** sources (the analogues) use an **equal-power** law — each leg follows a cos/sin
//     taper, so a centered source sits −3.01 dB per leg (i.e. on the device reference) and perceived
//     loudness stays constant across the sweep.
//   * **stereo** sources (playback returns) use a **balance** law — center is unity on both legs
//     (no dip); panning one way only attenuates the *opposite* leg.

/// dB → linear amplitude (`-inf` → 0).
fn db_to_lin(db: f64) -> f64 {
    if db.is_finite() {
        10f64.powf(db / 20.0)
    } else {
        0.0
    }
}

/// Add a linear `0..=1` leg multiplier onto a dB value, in dB. A zero multiplier (or `-inf` base)
/// collapses the leg to `-inf` (off).
fn apply_leg_db(base_db: f64, leg_lin: f64) -> f64 {
    if !base_db.is_finite() || leg_lin <= 0.0 {
        f64::NEG_INFINITY
    } else {
        base_db + 20.0 * leg_lin.log10()
    }
}

/// The (left, right) linear leg multipliers for a pan position, by source type.
fn leg_multipliers(source: Source, pan: f64) -> (f64, f64) {
    let pan = pan.clamp(-1.0, 1.0);
    let (l, r) = match source {
        // Equal-power: pan −1 → θ=0 (L full, R off); 0 → θ=π/4 (both 1/√2); +1 → θ=π/2 (R full).
        Source::Mono { .. } => {
            let theta = (pan + 1.0) * std::f64::consts::FRAC_PI_4;
            (theta.cos(), theta.sin())
        }
        // Balance: near leg stays at unity, far leg ramps to silence.
        Source::Stereo { .. } => {
            if pan <= 0.0 {
                (1.0, 1.0 + pan)
            } else {
                (1.0 - pan, 1.0)
            }
        }
    };
    // Snap floating-point dust (e.g. `cos(π/2) ≈ 6e-17`) to a clean zero, so a hard pan fully
    // silences the far leg (`-inf`) instead of leaving it ~−320 dB. 1e-9 ≈ −180 dB is well below
    // anything audible or representable in the Q6.25 coefficient.
    let snap = |x: f64| if x < 1e-9 { 0.0 } else { x };
    (snap(l), snap(r))
}

/// Split a cell's **fader** dB + pan into the (left_leg, right_leg) **coefficient** dB the two
/// crosspoint cells receive (device-ref offset + pan law together). `-inf` (off) → two `-inf` legs.
pub fn fader_pan_to_leg_coeffs(source: Source, fader_db: f64, pan: f64) -> (f64, f64) {
    // Absolute gain referenced to the device's 0 dB coefficient (full scale = +DEVICE_REF_OFFSET_DB).
    let base_db = if fader_db.is_finite() {
        fader_db + DEVICE_REF_OFFSET_DB
    } else {
        f64::NEG_INFINITY
    };
    let (l, r) = leg_multipliers(source, pan);
    (apply_leg_db(base_db, l), apply_leg_db(base_db, r))
}

/// Recover (fader dB, pan) from a cell's two leg **coefficient** dB — the inverse of
/// [`fader_pan_to_leg_coeffs`]. Two silent legs map to (`-inf`, center).
pub fn leg_coeffs_to_fader_pan(source: Source, left_db: f64, right_db: f64) -> (f64, f64) {
    let (l, r) = (db_to_lin(left_db), db_to_lin(right_db));
    if l <= 0.0 && r <= 0.0 {
        return (f64::NEG_INFINITY, 0.0);
    }
    let (gain_lin, pan) = match source {
        Source::Mono { .. } => {
            let gain = (l * l + r * r).sqrt(); // cos²+sin² = 1 ⇒ this is the pre-pan gain
            let pan = r.atan2(l) / std::f64::consts::FRAC_PI_4 - 1.0;
            (gain, pan)
        }
        Source::Stereo { .. } => {
            if l >= r {
                (l, r / l - 1.0) // left is the near (unity·gain) leg ⇒ panned center/left
            } else {
                (r, 1.0 - l / r) // right is the near leg ⇒ panned right
            }
        }
    };
    // `gain_lin` is referenced to the device 0 dB coefficient; back out the device-ref offset to
    // recover the absolute fader dB.
    let fader_db = 20.0 * gain_lin.log10() - DEVICE_REF_OFFSET_DB;
    (fader_db, pan.clamp(-1.0, 1.0))
}

/// Crosspoint cell index for a (source slot, destination block) pair.
pub const fn crosspoint_index(slot: u16, block: u16) -> u16 {
    block * SLOTS_PER_BLOCK + slot
}

/// The crosspoint cells (and the source slot feeding each) that carry `source` into
/// `dest`. Mono → both destination legs from the one slot; stereo → L→L, R→R.
pub fn cells_for(source: Source, dest: Destination) -> Vec<u16> {
    match source {
        Source::Mono { slot, .. } => vec![
            crosspoint_index(slot, dest.left_block),
            crosspoint_index(slot, dest.right_block),
        ],
        Source::Stereo { left, right, .. } => vec![
            crosspoint_index(left, dest.left_block),
            crosspoint_index(right, dest.right_block),
        ],
    }
}

/// The mixer sources in display order: the four stereo playback returns, then the four mono
/// analogue inputs. (These are the strips a host "monitor mix" UI shows feeding each bus.)
pub const SOURCES: [Source; 8] = [
    PLAYBACK_1_2,
    PLAYBACK_3_4,
    PLAYBACK_5_6,
    PLAYBACK_7_8,
    ANALOGUE_1,
    ANALOGUE_2,
    ANALOGUE_3,
    ANALOGUE_4,
];

pub const NUM_SOURCES: usize = SOURCES.len();
pub const NUM_DESTINATIONS: usize = DESTINATIONS.len();

/// dB bounds for a mix cell. Below `MIN_DB` collapses to `-inf` (off); above `MAX_DB` is clamped.
pub const MIN_DB: f64 = -60.0;
pub const MAX_DB: f64 = 12.0;

/// Host-side monitor-mix model: a **fader** dB per `(source, destination)` — the SSL 360 scale the
/// UI shows and edits, so 0 dB reads as unity. The SSL 12 DSP has no "fader"/"pan" concept; on the
/// way out, `cell_writes` applies the fader + pan law ([`fader_pan_to_leg_coeffs`]) and collapses to
/// `MIXER_CROSSPOINT_TABLE` cell writes. `-inf` (`f64::NEG_INFINITY`) means off into that bus.
///
/// This basic model writes both destination legs at the same gain (centered, no pan); a richer
/// UI would distribute across L/R per a pan law and write the legs independently.
#[derive(Debug, Clone)]
pub struct MixMatrix {
    gains_db: [[f64; NUM_DESTINATIONS]; NUM_SOURCES],
    /// Pan position per cell, `-1.0` (hard left) … `0.0` (center) … `+1.0` (hard right).
    pans: [[f64; NUM_DESTINATIONS]; NUM_SOURCES],
}

impl Default for MixMatrix {
    fn default() -> Self {
        MixMatrix {
            gains_db: [[f64::NEG_INFINITY; NUM_DESTINATIONS]; NUM_SOURCES],
            pans: [[0.0; NUM_DESTINATIONS]; NUM_SOURCES],
        }
    }
}

impl MixMatrix {
    /// Gain (dB, or `-inf`) of `source`→`dest`, both 0-based indices into `SOURCES`/`DESTINATIONS`.
    pub fn db(&self, source: usize, dest: usize) -> f64 {
        self.gains_db[source][dest]
    }

    /// Set a cell's gain, applying the `MIN_DB`→`-inf` floor and the `MAX_DB` ceiling.
    pub fn set_db(&mut self, source: usize, dest: usize, db: f64) {
        self.gains_db[source][dest] = if db < MIN_DB {
            f64::NEG_INFINITY
        } else {
            db.min(MAX_DB)
        };
    }

    /// Pan position of `source`→`dest` (`-1.0` L … `0.0` C … `+1.0` R).
    pub fn pan(&self, source: usize, dest: usize) -> f64 {
        self.pans[source][dest]
    }

    /// Set a cell's pan, clamped to `-1.0..=1.0`.
    pub fn set_pan(&mut self, source: usize, dest: usize, pan: f64) {
        self.pans[source][dest] = pan.clamp(-1.0, 1.0);
    }

    /// Nudge a cell's pan by `delta`, clamped to `-1.0..=1.0`.
    pub fn nudge_pan(&mut self, source: usize, dest: usize, delta: f64) {
        self.set_pan(source, dest, self.pans[source][dest] + delta);
    }

    /// Nudge a cell by `delta` dB. Stepping **up** from `-inf` enters the scale at `MIN_DB`;
    /// stepping down from `MIN_DB` (or below) drops to `-inf`.
    pub fn nudge(&mut self, source: usize, dest: usize, delta: f64) {
        let cur = self.gains_db[source][dest];
        let next = if cur.is_infinite() {
            if delta > 0.0 {
                MIN_DB
            } else {
                return;
            }
        } else {
            cur + delta
        };
        self.set_db(source, dest, next);
    }

    /// The `(cell_index, q6.25_raw)` crosspoint writes that realize `source`→`dest` at its current
    /// fader gain. The stored fader dB is mapped through the fader law to the device coefficient dB
    /// before quantizing. Feed each into `protocol::dsp_q625(Q6.25 coeff update, MIXER_CROSSPOINT_TABLE, …)`.
    pub fn cell_writes(&self, source: usize, dest: usize) -> Vec<(u16, i32)> {
        let src = SOURCES[source];
        let (left_db, right_db) =
            fader_pan_to_leg_coeffs(src, self.gains_db[source][dest], self.pans[source][dest]);
        // `cells_for` returns [left_leg_cell, right_leg_cell]; pair each with its leg's coefficient.
        let raws = [
            crate::protocol::db_to_q625(left_db),
            crate::protocol::db_to_q625(right_db),
        ];
        cells_for(src, DESTINATIONS[dest])
            .into_iter()
            .zip(raws)
            .collect()
    }

    /// Reverse-map a crosspoint cell index back to its `(source, dest)` indices, if it belongs to a
    /// known source/destination. Lets a UI fold device/echoed crosspoint VALUEs back into the grid.
    pub fn locate(index: u16) -> Option<(usize, usize)> {
        for (si, s) in SOURCES.iter().enumerate() {
            for (di, d) in DESTINATIONS.iter().enumerate() {
                if cells_for(*s, *d).contains(&index) {
                    return Some((si, di));
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_captured_indices() {
        // cap_slots: Analogue 1 (mono slot 8) into Main → idx 8 and 38.
        assert_eq!(cells_for(ANALOGUE_1, MAIN), vec![8, 38]);
        // cap_dests: Analogue 1 into HP A → idx 128 and 158.
        assert_eq!(cells_for(ANALOGUE_1, HP_A), vec![128, 158]);
        // cap_slots2: Playback 1-2 (stereo 0/1) into Main → idx 0 and 31.
        assert_eq!(cells_for(PLAYBACK_1_2, MAIN), vec![0, 31]);
        // cap_pb: Playback 5-6 (stereo 4/5) into Main → idx 4 and 35; Playback 7-8 (6/7) → 6 and 37.
        assert_eq!(cells_for(PLAYBACK_5_6, MAIN), vec![4, 35]);
        assert_eq!(cells_for(PLAYBACK_7_8, MAIN), vec![6, 37]);
        // cap_dests: Analogue 1 into Line 3-4 → idx 68 and 98.
        assert_eq!(cells_for(ANALOGUE_1, LINE_3_4), vec![68, 98]);
    }

    #[test]
    fn matrix_defaults_to_silence() {
        let m = MixMatrix::default();
        for s in 0..NUM_SOURCES {
            for d in 0..NUM_DESTINATIONS {
                assert!(m.db(s, d).is_infinite());
            }
        }
    }

    #[test]
    fn matrix_nudge_floor_and_ceiling() {
        let mut m = MixMatrix::default();
        // Up from -inf enters at MIN_DB.
        m.nudge(0, 0, 1.0);
        assert_eq!(m.db(0, 0), MIN_DB);
        // Down from MIN_DB drops back to -inf.
        m.nudge(0, 0, -1.0);
        assert!(m.db(0, 0).is_infinite());
        // Ceiling clamps.
        m.set_db(0, 0, 999.0);
        assert_eq!(m.db(0, 0), MAX_DB);
    }

    #[test]
    fn matrix_cell_writes_target_the_right_cells() {
        let mut m = MixMatrix::default();
        // Source 0 = Playback 1-2 (stereo 0/1) into dest 0 = Main → cells 0 and 31, centered.
        m.set_db(0, 0, 0.0);
        let writes = m.cell_writes(0, 0);
        let indices: Vec<u16> = writes.iter().map(|(i, _)| *i).collect();
        assert_eq!(indices, vec![0, 31]);
        // Stereo fader 0 dB, centered → each leg at full scale (+3.01 dB above the mono reference).
        let expected = crate::protocol::db_to_q625(DEVICE_REF_OFFSET_DB);
        assert!(writes.iter().all(|(_, raw)| *raw == expected));
        assert!(expected > crate::protocol::ZERO_DB_REF);
    }

    #[test]
    fn anchors_match_captures() {
        // Mono analogue at fader 0 dB, centered → each leg lands exactly on the device 0 dB
        // reference (the equal-power center −3.01 dB cancels the +3.01 device-ref offset). This is
        // the PROTOCOL §4 "analogue-1 fader → 0 dB" worked example.
        let mut m = MixMatrix::default();
        m.set_db(4, 0, 0.0); // source 4 = Analogue 1 (mono), pan defaults to center
        assert!(m
            .cell_writes(4, 0)
            .iter()
            .all(|(_, raw)| *raw == crate::protocol::ZERO_DB_REF));

        // Stereo source at fader 0, centered → both legs at full scale (+3.01 dB).
        let (l, r) = fader_pan_to_leg_coeffs(PLAYBACK_1_2, 0.0, 0.0);
        assert!((l - DEVICE_REF_OFFSET_DB).abs() < 1e-9 && (r - DEVICE_REF_OFFSET_DB).abs() < 1e-9);
    }

    #[test]
    fn pan_law_round_trips_and_extremes() {
        for &src in &[PLAYBACK_1_2, ANALOGUE_1] {
            // fader + pan → legs → fader + pan round-trips across the throw and the sweep.
            for fader in [-40.0, -6.0, 0.0, 6.0] {
                for pan in [-1.0, -0.5, 0.0, 0.37, 1.0] {
                    let (l, r) = fader_pan_to_leg_coeffs(src, fader, pan);
                    let (f2, p2) = leg_coeffs_to_fader_pan(src, l, r);
                    assert!(
                        (f2 - fader).abs() < 1e-6,
                        "{src:?} fader {fader} pan {pan} → {f2}"
                    );
                    assert!(
                        (p2 - pan).abs() < 1e-6,
                        "{src:?} fader {fader} pan {pan} → pan {p2}"
                    );
                }
            }
            // Hard pan silences the far leg.
            let (l, r) = fader_pan_to_leg_coeffs(src, 0.0, -1.0);
            assert!(l.is_finite() && r.is_infinite(), "hard left: right leg off");
            let (l, r) = fader_pan_to_leg_coeffs(src, 0.0, 1.0);
            assert!(l.is_infinite() && r.is_finite(), "hard right: left leg off");
            // Off passes straight through.
            let (l, r) = fader_pan_to_leg_coeffs(src, f64::NEG_INFINITY, 0.0);
            assert!(l.is_infinite() && r.is_infinite());
        }
        // A centered mono source is −3.01 dB per leg (equal-power); a centered stereo source is 0 dB
        // relative to its own legs (no center dip) — i.e. unity per leg.
        let (lm, _) = fader_pan_to_leg_coeffs(ANALOGUE_1, 0.0, 0.0);
        let (ls, _) = fader_pan_to_leg_coeffs(PLAYBACK_1_2, 0.0, 0.0);
        assert!(
            (ls - lm - DEVICE_REF_OFFSET_DB).abs() < 1e-9,
            "stereo center is +3.01 dB hotter per leg"
        );
    }

    #[test]
    fn matrix_locate_is_inverse_of_cells() {
        // Every cell index a source/dest writes maps back to that same source/dest.
        for (s, &src) in SOURCES.iter().enumerate() {
            for (d, &dest) in DESTINATIONS.iter().enumerate() {
                for idx in cells_for(src, dest) {
                    assert_eq!(MixMatrix::locate(idx), Some((s, d)));
                }
            }
        }
    }
}
