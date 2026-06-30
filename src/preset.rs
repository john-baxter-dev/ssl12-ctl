//! Save/load the monitor mix as a human-editable TOML preset, and (re)apply it on connect.
//!
//! The mix is **host-authoritative**: the device never reports its crosspoints (see
//! `docs/PROTOCOL.md` §8), so a real client persists the mix here and asserts it on connect.
//! Schema is `source name → { bus name → dB }`; a bus that's **off** (-inf) is simply omitted —
//! which also sidesteps the fact that neither TOML nor JSON can encode `-inf` as a number.
//!
//! Example `mix.toml`:
//! ```toml
//! [sends."Playback 1-2"]
//! "Main L-R" = 0.0
//! "HP A" = -3.0
//! # buses not listed here are off
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::mixer::{self, MixMatrix};

/// Host-owned monitor-output selections. Like the mix, these are DSP **coefficients** the device
/// never reports back (§8), so the host persists them and re-asserts them on connect. The selection
/// values are indices matching the TUI's `HP_MODES` / `LINE_LEVELS` / `LOOPBACK_SOURCES` label arrays:
///   `hp_*_mode`: 0 = Standard, 1 = High-sens, 2 = High-Z
///   `line_level_sel`: 0 = +9 dBu, 1 = +24 dBu
/// `dim_level_db` is the monitor dim *amount* (`OUTPUT_BUS_DIM_LEVEL`, Q6.25 dB): 0 = no cut,
/// negative = how much engaging Dim attenuates. Defaults to `default_dim_level_db` so an old preset
/// (or a fresh one) re-asserts a sensible cut rather than 0 dB / "dim does nothing".
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OutputSettings {
    #[serde(default)]
    pub hp_a_mode: u16,
    #[serde(default)]
    pub hp_b_mode: u16,
    #[serde(default)]
    pub line_level_sel: u16,
    #[serde(default)]
    pub loopback_source_sel: u16,
    #[serde(default = "default_dim_level_db")]
    pub dim_level_db: f64,
    /// Alt-speaker master enable (off by default) + trim in dB (bipolar, 0 = unity). The live Alt
    /// switch itself is a device param, not stored here.
    #[serde(default)]
    pub alt_enable: bool,
    #[serde(default)]
    pub alt_trim_db: f64,
    /// Per-cue-bus talkback send level (dB) + pan (−1..+1), indexed `[Line 3-4, HP A, HP B]` — the
    /// buses talkback feeds (Main excluded). Each pair drives `TALKBACK_LEVEL`'s two legs via the
    /// pan law. Default all-zero (unity, centered); also covers presets written before these existed.
    #[serde(default)]
    pub tb_db: [f64; 3],
    #[serde(default)]
    pub tb_pan: [f64; 3],
}

/// Monitor dim attenuation default (dB). Host-owned, so this is what we push when nothing's saved;
/// the exact factory value is unconfirmed (bench-verify). Shared by the serde default, the hand-
/// written `Default`, and the TUI (its no-preset init + the Space-to-reset action) so all agree.
pub const fn default_dim_level_db() -> f64 {
    -20.0
}

// Hand-written because the `dim_level_db` default is non-zero, which `#[derive(Default)]` can't
// express for a struct field. `Eq` is likewise dropped (an `f64` field isn't `Eq`).
impl Default for OutputSettings {
    fn default() -> Self {
        Self {
            hp_a_mode: 0,
            hp_b_mode: 0,
            line_level_sel: 0,
            loopback_source_sel: 0,
            dim_level_db: default_dim_level_db(),
            alt_enable: false,
            alt_trim_db: 0.0,
            tb_db: [0.0; 3],
            tb_pan: [0.0; 3],
        }
    }
}

/// Host-owned analogue stereo-link state (SSL 360 "input link"). Like the mix and output
/// selections, this is a DSP **coefficient** the device never reports back (§8), so the host owns,
/// persists, and re-asserts it on connect. Keys match the TUI's `LINK_PAIR_NAMES`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkSettings {
    #[serde(default, rename = "1-2")]
    pub pair_1_2: bool,
    #[serde(default, rename = "3-4")]
    pub pair_3_4: bool,
}

impl LinkSettings {
    /// As a `[ins 1-2, ins 3-4]` flag array (the TUI's in-memory representation).
    pub fn to_array(self) -> [bool; 2] {
        [self.pair_1_2, self.pair_3_4]
    }

    /// From a `[ins 1-2, ins 3-4]` flag array.
    pub fn from_array(a: [bool; 2]) -> Self {
        LinkSettings {
            pair_1_2: a[0],
            pair_3_4: a[1],
        }
    }
}

/// A saved monitor mix + output settings. `sends[source][bus] = dB`; an absent bus means off (-inf).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct MixPreset {
    #[serde(default)]
    pub sends: BTreeMap<String, BTreeMap<String, f64>>,
    /// Host-owned output selections (HP gain modes, line operating level). Defaults to all-zero for
    /// presets written before this field existed, so older `mix.toml` files still load.
    #[serde(default)]
    pub outputs: OutputSettings,
    /// Host-owned analogue stereo-link flags. Defaults to unlinked for presets written before this
    /// field existed.
    #[serde(default)]
    pub links: LinkSettings,
    /// Per-cell pan position (`-1.0` L … `+1.0` R); same `source → { bus = pan }` shape as `sends`.
    /// Centered cells are omitted, so presets written before pan existed load as all-centered.
    #[serde(default)]
    pub pans: BTreeMap<String, BTreeMap<String, f64>>,
}

impl MixPreset {
    /// Capture a matrix's finite (non-off) cells into a preset. Output and link settings default to
    /// zero/unlinked; callers that track them (the TUI) set `.outputs`/`.links` before saving.
    pub fn from_matrix(m: &MixMatrix) -> Self {
        let mut sends = BTreeMap::new();
        let mut pans = BTreeMap::new();
        for (si, s) in mixer::SOURCES.iter().enumerate() {
            let mut buses = BTreeMap::new();
            let mut cell_pans = BTreeMap::new();
            for (di, d) in mixer::DESTINATIONS.iter().enumerate() {
                let db = m.db(si, di);
                if db.is_finite() {
                    buses.insert(d.name.to_string(), db);
                    // Only record a pan for cells that are on and actually panned off-center.
                    if m.pan(si, di) != 0.0 {
                        cell_pans.insert(d.name.to_string(), m.pan(si, di));
                    }
                }
            }
            if !buses.is_empty() {
                sends.insert(s.name().to_string(), buses);
            }
            if !cell_pans.is_empty() {
                pans.insert(s.name().to_string(), cell_pans);
            }
        }
        MixPreset {
            sends,
            outputs: OutputSettings::default(),
            links: LinkSettings::default(),
            pans,
        }
    }

    /// Apply this preset onto a matrix: every cell is reset to off, then the listed cells are set.
    /// Unknown source/bus names are skipped (forward-compatible with hand edits).
    pub fn apply(&self, m: &mut MixMatrix) {
        for si in 0..mixer::NUM_SOURCES {
            for di in 0..mixer::NUM_DESTINATIONS {
                m.set_db(si, di, f64::NEG_INFINITY);
            }
        }
        for (src, buses) in &self.sends {
            let Some(si) = mixer::SOURCES.iter().position(|s| s.name() == src) else {
                continue;
            };
            for (bus, &db) in buses {
                if let Some(di) = mixer::DESTINATIONS.iter().position(|d| d.name == bus) {
                    m.set_db(si, di, db);
                }
            }
        }
        // Pans default to center; apply any the preset recorded (skip unknown names).
        for (src, cell_pans) in &self.pans {
            let Some(si) = mixer::SOURCES.iter().position(|s| s.name() == src) else {
                continue;
            };
            for (bus, &pan) in cell_pans {
                if let Some(di) = mixer::DESTINATIONS.iter().position(|d| d.name == bus) {
                    m.set_pan(si, di, pan);
                }
            }
        }
    }
}

/// Default preset path: `<config dir>/ssl12/mix.toml` (e.g. `~/.config/ssl12/mix.toml` on Linux).
pub fn default_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("ssl12").join("mix.toml"))
}

/// Load a preset from `path`. `Ok(None)` means the file doesn't exist yet (no preset — not an error).
pub fn load(path: &Path) -> Result<Option<MixPreset>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text)
            .map(Some)
            .map_err(|e| format!("parse {}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("read {}: {e}", path.display())),
    }
}

/// Save a preset to `path`, creating parent directories as needed.
pub fn save(preset: &MixPreset, path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let text = toml::to_string_pretty(preset).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(path, text).map_err(|e| format!("write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_preset() {
        let mut m = MixMatrix::default();
        m.set_db(0, 0, 0.0); // Playback 1-2 → Main
        m.set_db(0, 2, -3.0); // Playback 1-2 → HP A
        m.set_db(4, 0, -12.0); // Analogue 1 → Main
        let preset = MixPreset::from_matrix(&m);

        let mut back = MixMatrix::default();
        // Pre-dirty a cell that should get cleared by apply().
        back.set_db(7, 3, 5.0);
        preset.apply(&mut back);

        assert_eq!(back.db(0, 0), 0.0);
        assert_eq!(back.db(0, 2), -3.0);
        assert_eq!(back.db(4, 0), -12.0);
        assert!(back.db(7, 3).is_infinite(), "unlisted cell reset to off");
        assert!(back.db(1, 1).is_infinite(), "never-set cell stays off");
    }

    #[test]
    fn output_settings_round_trip() {
        let p = MixPreset {
            outputs: OutputSettings {
                hp_a_mode: 2,
                hp_b_mode: 1,
                line_level_sel: 1,
                loopback_source_sel: 0,
                dim_level_db: -12.0,
                alt_enable: true,
                alt_trim_db: -3.0,
                tb_db: [-6.0, -3.0, 0.0],
                tb_pan: [0.0, -0.5, 1.0],
            },
            ..Default::default()
        };
        let text = toml::to_string_pretty(&p).unwrap();
        let back: MixPreset = toml::from_str(&text).unwrap();
        assert_eq!(back.outputs, p.outputs);
    }

    #[test]
    fn preset_without_outputs_uses_defaults() {
        // A mix.toml written before the `outputs` field existed must still load — selections fall to
        // zero, but dim level falls to its non-zero default (so dim still cuts on an old preset).
        let text = r#"
[sends."Playback 1-2"]
"Main L-R" = 0.0
"#;
        let p: MixPreset = toml::from_str(text).unwrap();
        assert_eq!(p.outputs, OutputSettings::default());
        assert_eq!(p.outputs.dim_level_db, default_dim_level_db());
    }

    #[test]
    fn off_cells_are_omitted() {
        let m = MixMatrix::default(); // all off
        let preset = MixPreset::from_matrix(&m);
        assert!(
            preset.sends.is_empty(),
            "an all-off mix serializes to nothing"
        );
        assert!(preset.pans.is_empty(), "no pans recorded for an empty mix");
    }

    #[test]
    fn pans_round_trip_and_default_to_center() {
        let mut m = MixMatrix::default();
        m.set_db(4, 0, -3.0); // Analogue 1 → Main, on
        m.set_pan(4, 0, -0.5); // panned left
        m.set_db(0, 0, 0.0); // Playback 1-2 → Main, on but centered
        let preset = MixPreset::from_matrix(&m);
        // Only the off-center, on cell is recorded.
        assert_eq!(preset.pans["Analogue 1"]["Main L-R"], -0.5);
        assert!(
            !preset.pans.contains_key("Playback 1-2"),
            "centered cell omitted"
        );

        // Round-trips through TOML and back onto a matrix.
        let text = toml::to_string_pretty(&preset).unwrap();
        let back: MixPreset = toml::from_str(&text).unwrap();
        let mut m2 = MixMatrix::default();
        back.apply(&mut m2);
        assert_eq!(m2.pan(4, 0), -0.5);
        assert_eq!(m2.pan(0, 0), 0.0, "unlisted cell stays centered");
    }

    #[test]
    fn preset_without_pans_loads_centered() {
        // A mix.toml written before pans existed must load with every cell centered.
        let text = r#"
[sends."Analogue 1"]
"Main L-R" = -3.0
"#;
        let p: MixPreset = toml::from_str(text).unwrap();
        assert!(p.pans.is_empty());
        let mut m = MixMatrix::default();
        p.apply(&mut m);
        assert_eq!(m.pan(4, 0), 0.0);
    }

    #[test]
    fn links_round_trip_through_toml() {
        let mut p = MixPreset::from_matrix(&MixMatrix::default());
        p.links = LinkSettings::from_array([true, false]);
        let text = toml::to_string_pretty(&p).unwrap();
        let back: MixPreset = toml::from_str(&text).unwrap();
        assert_eq!(
            back.links.to_array(),
            [true, false],
            "link flags survive a save/load"
        );
    }

    #[test]
    fn missing_links_default_to_unlinked() {
        // A preset written before the [links] table existed still loads, unlinked.
        let preset: MixPreset = toml::from_str("[sends]\n").unwrap();
        assert_eq!(preset.links.to_array(), [false, false]);
    }

    #[test]
    fn parses_hand_written_toml() {
        let text = r#"
[sends."Playback 1-2"]
"Main L-R" = 0.0
"HP A" = -6.0
"#;
        let preset: MixPreset = toml::from_str(text).unwrap();
        let mut m = MixMatrix::default();
        preset.apply(&mut m);
        assert_eq!(m.db(0, 0), 0.0); // PB1-2 → Main
        assert_eq!(m.db(0, 2), -6.0); // PB1-2 → HP A
    }
}
