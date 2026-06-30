//! `App` — model and control logic for ssl12tui (state, reconcile, key handling). Rendering lives
//! in `view`; the entry point and event loop in `main`.

use std::time::{Duration, Instant};

use crossterm::event::KeyCode;

use ssl12_ctl::controls::{Coeff, Param};
use ssl12_ctl::meters::{self, Sample, NUM_METERS};
use ssl12_ctl::mixer::{self, MixMatrix};
use ssl12_ctl::preset::{self, MixPreset};
use ssl12_ctl::protocol::{self, DspCode};
use ssl12_ctl::transport::Transport;

pub(crate) const NUM_INPUTS: usize = 4; // the four analogue inputs

/// The per-input bool controls shown in the Inputs grid: `(label, DSP param number, # valid inputs)`.
/// Hi-Z only applies to inputs 1–2, so its valid count is 2; the rest cover all four.
pub(crate) const INPUT_CONTROLS: [(&str, u16, usize); 5] = [
    ("48V", Param::InputPhantomPower.num(), 4),
    ("HPF", Param::InputHpf.num(), 4),
    ("Line", Param::InputLineInput.num(), 4),
    ("Hi-Z", Param::InputInstrumentInput.num(), 2),
    ("Ø", Param::InputPolarity.num(), 4),
];
const NUM_INPUT_CONTROLS: usize = INPUT_CONTROLS.len();

/// The two independent analogue stereo-link pairs (SSL 360 "input link"), named by their channels.
/// Pair `p` owns input rows `2p` and `2p+1` (and the matching `STEREO_LINK_CHANNELS` index `2p`:
/// 0 = ins 1-2, 2 = ins 3-4). Linking couples the pair's switches and mixer sends so an edit to one
/// applies to both, mirroring SSL 360.
const LINK_PAIR_NAMES: [&str; 2] = ["1-2", "3-4"];

/// MixMatrix source row for analogue input `row` (the analogues follow the four playback sources).
const fn analogue_source(row: usize) -> usize {
    mixer::NUM_SOURCES - NUM_INPUTS + row
}

/// Headphone gain-mode selection labels (`HEADPHONES_GAIN_MODE').
const HP_MODES: [&str; 3] = ["Standard", "High-sens", "High-Z"];
/// Line-output operating-level selection labels (`LINE_OUTPUT_OPERATING_LEVEL`).
const LINE_LEVELS: [&str; 2] = ["+9 dBu", "+24 dBu"];
/// Loopback source selection labels (`LOOPBACK_SOURCE`).
const LOOPBACK_SOURCES: [&str; 9] = [
    "Off",
    "PB 1-2",
    "PB 3-4",
    "PB 5-6",
    "PB 7-8",
    "Output 1-2",
    "Output 3-4",
    "Output 5-6",
    "Output 7-8",
];

/// Monitor dim-level (`OUTPUT_BUS_DIM_LEVEL`, Q6.25 dB) UI bounds + nudge step: 0 dB = no cut,
/// negative = how much engaging Dim attenuates. The default lives in `preset::default_dim_level_db`
/// (shared with persistence); the range/step here are our choice — bench-verify the device's taper.
const DIM_MIN_DB: f64 = -40.0;
const DIM_MAX_DB: f64 = 0.0;
const DIM_STEP_DB: f64 = 1.0;

/// Alt-speaker trim (`OUTPUT_BUS_ALT_TRIM_LEVEL`, Q6.25 dB) — bipolar level-match to the mains, so
/// 0 = unity, ± to boost/cut. Host-owned (persisted + pushed on connect). _Open (🔧):_ exact range.
const ALT_TRIM_MIN_DB: f64 = -12.0;
const ALT_TRIM_MAX_DB: f64 = 12.0;
const ALT_TRIM_STEP_DB: f64 = 1.0;

/// Talkback sends behave like mono mix cells: a per-cue-bus level + pan, split across the bus's L/R
/// legs by the same pan law as the crosspoints (`fader_pan_to_leg_coeffs`) and written to
/// `TALKBACK_LEVEL` (idx 2..=7). Drawn as a separate row under the Mixer matrix — *not* a crosspoint
/// source (PROTOCOL §9b). The mic is mono, so the leg split uses the equal-power law; this stand-in
/// `Source::Mono` only selects that law (its `slot` is unused by the leg math).
const TALK_SOURCE: mixer::Source = mixer::Source::Mono {
    name: "Talkback",
    slot: 0,
};
/// Mixer destination columns that carry a talkback send: `DESTINATIONS` 1..=3 (Line 3-4 / HP A /
/// HP B). Main (0) is excluded — talk feeds the cue/phones, not the control-room mains. The talk
/// slot (`tb_db`/`tb_pan` index) for destination column `d` is `d - 1`.
const TALKBACK_DESTS: [usize; 3] = [1, 2, 3];

/// The Mixer's talkback send row sits one row past the crosspoint source rows (it's drawn as a
/// separate strip below the matrix — talkback is not a crosspoint source, see PROTOCOL §9b).
pub(crate) const MIXER_TALK_ROW: usize = mixer::NUM_SOURCES;

/// The talk slot (`tb_db`/`tb_pan` index) for a mixer destination column, or `None` if that bus has
/// no talkback send (Main). Lets the talk row share the matrix's column cursor, skipping Main.
pub(crate) fn talkback_slot(dest: usize) -> Option<usize> {
    TALKBACK_DESTS.contains(&dest).then(|| dest - 1)
}

/// Coefficient indices for the per-output-bus selections on the Outputs screen. The headphone
/// gain-mode coefficient is indexed by the destination's mixer **block**, so HP A/B derive from the
/// `mixer` constants rather than bare 4/6. The line operating level is index 0 (the line 1-2
/// output) — not a crosspoint block, so it's kept explicit.
const HP_A_BUS: u16 = mixer::HP_A.left_block; // 4
const HP_B_BUS: u16 = mixer::HP_B.left_block; // 6
const LINE_OUT_IDX: u16 = 0;
/// The Outputs screen's rows, in display order. Each variant owns its label and behavior (see the
/// `impl` below), so the row's identity lives in one place instead of as a bare index duplicated
/// across `output_activate` / `output_cycle` / the draw loop. Adding a row = add a variant + an
/// `ALL` entry; the compiler then forces every method's `match` to handle it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutRow {
    Mono,
    Dim,
    DimLevel,
    Cut,
    PhaseL,
    AltEnable,
    Alt,
    AltTrim,
    MuteAll,
    HpAMode,
    HpBMode,
    LineLevel,
    Loopback,
}

impl OutRow {
    /// Display order — the single source of truth for the screen's layout.
    pub(crate) const ALL: [OutRow; 13] = [
        OutRow::Mono,
        OutRow::Dim,
        OutRow::DimLevel,
        OutRow::Cut,
        OutRow::PhaseL,
        OutRow::AltEnable,
        OutRow::Alt,
        OutRow::AltTrim,
        OutRow::MuteAll,
        OutRow::HpAMode,
        OutRow::HpBMode,
        OutRow::LineLevel,
        OutRow::Loopback,
    ];

    /// This row's index in `ALL` (its on-screen position). Lets tests refer to a row by identity
    /// instead of a hard-coded slot, so inserting a row can't silently shift them.
    #[cfg(test)]
    pub(crate) fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|&r| r == self)
            .expect("row in ALL")
    }

    /// Rows that are inert until the alt-speaker feature is enabled: the live Alt switch and its
    /// trim are greyed + do nothing while `ALT_SPK_ENABLE` is off (mirrors SSL 360).
    pub(crate) fn is_disabled(self, app: &App) -> bool {
        matches!(self, OutRow::Alt | OutRow::AltTrim) && !app.alt_enable
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            OutRow::Mono => "Monitor — Mono",
            OutRow::Dim => "Monitor — Dim",
            OutRow::DimLevel => "Monitor — Dim level",
            OutRow::Cut => "Monitor — Cut",
            OutRow::PhaseL => "Monitor — Ø L",
            OutRow::AltEnable => "Alt spk enable",
            OutRow::Alt => "Monitor — Alt",
            OutRow::AltTrim => "Alt spk trim",
            OutRow::MuteAll => "Mute all outputs",
            OutRow::HpAMode => "Headphone A mode",
            OutRow::HpBMode => "Headphone B mode",
            OutRow::LineLevel => "Line out op level",
            OutRow::Loopback => "Loopback source",
        }
    }

    /// The row's value text and, for bool rows, `Some(on)`; selection rows return `None`.
    pub(crate) fn value(self, app: &App) -> (String, Option<bool>) {
        let sel = |labels: &[&'static str], i: u16| {
            labels.get(i as usize).copied().unwrap_or("?").to_string()
        };
        match self {
            OutRow::Mono => (onoff(app.out_mono), Some(app.out_mono)),
            OutRow::Dim => (onoff(app.out_dim), Some(app.out_dim)),
            // A continuous dB level, not a bool/selection: render the value (Some/None bool is `None`,
            // so it draws in the selection style) and adjust it with ←/→ (see `cycle`).
            OutRow::DimLevel => (format!("{:.1} dB", app.dim_level_db), None),
            OutRow::Cut => (onoff(app.out_cut), Some(app.out_cut)),
            OutRow::PhaseL => (onoff(app.out_phase_l), Some(app.out_phase_l)),
            OutRow::AltEnable => (onoff(app.alt_enable), Some(app.alt_enable)),
            OutRow::Alt => (onoff(app.out_alt), Some(app.out_alt)),
            OutRow::AltTrim => (format!("{:+.1} dB", app.alt_trim_db), None),
            OutRow::MuteAll => (onoff(app.muted), Some(app.muted)),
            OutRow::HpAMode => (sel(&HP_MODES, app.hp_a_mode), None),
            OutRow::HpBMode => (sel(&HP_MODES, app.hp_b_mode), None),
            OutRow::LineLevel => (sel(&LINE_LEVELS, app.line_level_sel), None),
            OutRow::Loopback => (sel(&LOOPBACK_SOURCES, app.loopback_sel), None),
        }
    }

    /// Space/Enter: toggle a bool/mute row, or advance a selection by one.
    fn activate(self, app: &mut App) {
        // The live Alt switch + its trim do nothing until the alt feature is enabled.
        if self.is_disabled(app) {
            return;
        }
        match self {
            OutRow::Mono => {
                app.out_mono = !app.out_mono;
                app.send_out_bool(Param::OutputBusMono, app.out_mono);
            }
            OutRow::Dim => {
                app.out_dim = !app.out_dim;
                app.send_out_bool(Param::OutputBusDim, app.out_dim);
            }
            OutRow::Cut => {
                app.out_cut = !app.out_cut;
                app.send_out_bool(Param::OutputBusCut, app.out_cut);
            }
            OutRow::PhaseL => {
                app.out_phase_l = !app.out_phase_l;
                app.send_out_bool(Param::OutputBusPhaseL, app.out_phase_l);
            }
            OutRow::MuteAll => app.toggle_mute(),
            // Alt-speaker master enable: a host-owned coefficient bool (gates the rows below it).
            OutRow::AltEnable => {
                app.alt_enable = !app.alt_enable;
                app.send_out_coeff_bool(Coeff::OutputBusAltSpkEnable, app.alt_enable);
            }
            // The live Alt switch is a device param, like Mono/Dim/Cut.
            OutRow::Alt => {
                app.out_alt = !app.out_alt;
                app.send_out_bool(Param::OutputBusAlt, app.out_alt);
            }
            // Level rows reset on Space (mirrors the Mixer's `0`).
            OutRow::DimLevel => {
                app.dim_level_db = preset::default_dim_level_db();
                app.push_dim_level();
            }
            OutRow::AltTrim => {
                app.alt_trim_db = 0.0;
                app.push_alt_trim();
            }
            OutRow::HpAMode | OutRow::HpBMode | OutRow::LineLevel | OutRow::Loopback => {
                self.cycle(app, 1)
            }
        }
    }

    /// ←/→: cycle a selection row (the bool/mute rows ignore it).
    fn cycle(self, app: &mut App, delta: i32) {
        if self.is_disabled(app) {
            return;
        }
        match self {
            OutRow::HpAMode => {
                app.hp_a_mode = cycle(app.hp_a_mode, HP_MODES.len(), delta);
                app.send_out_sel(Coeff::HeadphonesGainMode, HP_A_BUS, app.hp_a_mode);
            }
            OutRow::HpBMode => {
                app.hp_b_mode = cycle(app.hp_b_mode, HP_MODES.len(), delta);
                app.send_out_sel(Coeff::HeadphonesGainMode, HP_B_BUS, app.hp_b_mode);
            }
            OutRow::LineLevel => {
                app.line_level_sel = cycle(app.line_level_sel, LINE_LEVELS.len(), delta);
                app.send_out_sel(
                    Coeff::LineOutputOperatingLevel,
                    LINE_OUT_IDX,
                    app.line_level_sel,
                );
            }
            OutRow::Loopback => {
                app.loopback_sel = cycle(app.loopback_sel, LOOPBACK_SOURCES.len(), delta);
                app.send_out_sel(Coeff::LoopbackSource, 0, app.loopback_sel);
            }
            // A dB level, so nudge + clamp rather than wrap an index like the selection rows.
            OutRow::DimLevel => {
                app.dim_level_db =
                    (app.dim_level_db + delta as f64 * DIM_STEP_DB).clamp(DIM_MIN_DB, DIM_MAX_DB);
                app.push_dim_level();
            }
            // Bipolar trim: nudge + clamp to ±ALT_TRIM_MAX_DB.
            OutRow::AltTrim => {
                app.alt_trim_db = (app.alt_trim_db + delta as f64 * ALT_TRIM_STEP_DB)
                    .clamp(ALT_TRIM_MIN_DB, ALT_TRIM_MAX_DB);
                app.push_alt_trim();
            }
            OutRow::Mono
            | OutRow::Dim
            | OutRow::Cut
            | OutRow::PhaseL
            | OutRow::AltEnable
            | OutRow::Alt
            | OutRow::MuteAll => {}
        }
    }
}

/// Number of rows on the Outputs screen — derived from `OutRow::ALL` so it can't drift.
pub(crate) const NUM_OUT_ROWS: usize = OutRow::ALL.len();

/// Pan nudge per `[`/`]` keypress, as a fraction of the −1..+1 pan range (10%).
const PAN_STEP: f64 = 0.1;

/// Which screen the body shows. Tab cycles between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Screen {
    Meters,
    Inputs,
    Outputs,
    Mixer,
}

pub(crate) struct App {
    // Fields are `pub(crate)` so the `view` module can read UI state to render it; only `app` mutates.
    pub(crate) transport: Box<dyn Transport>,
    pub(crate) backend_label: String,
    pub(crate) screen: Screen,
    pub(crate) levels: [Sample; NUM_METERS],
    /// Per-channel peak-hold + latched-clip state, advanced from the meter stream (see `pump`).
    pub(crate) peaks: [meters::PeakHold; NUM_METERS],
    /// When the last meter frame was folded in, for the peak-hold decay `dt`.
    pub(crate) last_meter_at: Option<Instant>,
    /// Per-input bool control state, `[input][control]` indexed by `INPUT_CONTROLS`.
    pub(crate) in_state: [[bool; NUM_INPUT_CONTROLS]; NUM_INPUTS],
    /// Selected input cell: (input row, control column).
    pub(crate) in_sel: (usize, usize),
    /// Stereo-link state per analogue pair (`[0]` = ins 1-2, `[1]` = ins 3-4). Host-owned: a
    /// coefficient the device never reports back, so we track it locally. See `LINK_PAIR_NAMES`.
    pub(crate) link: [bool; 2],
    pub(crate) muted: bool,
    /// Talkback mic open (`OUTPUT_BUS_TALKBACK_ENABLE`). Momentary in spirit, but terminals don't
    /// deliver reliable key-up, so it's a press-to-toggle latch with a prominent title indicator.
    pub(crate) talking: bool,
    /// Monitor-bus param toggles (idx 0, device-reported → hydrate).
    pub(crate) out_mono: bool,
    pub(crate) out_dim: bool,
    pub(crate) out_cut: bool,
    pub(crate) out_phase_l: bool,
    /// Live "switch to alt speakers" toggle (`OUTPUT_BUS_ALT` param, device-reported → hydrate).
    pub(crate) out_alt: bool,
    /// Alt-speaker master enable (`OUTPUT_BUS_ALT_SPK_ENABLE` coeff, host-owned). Off by default;
    /// gates the live Alt switch + trim (see `OutRow::is_disabled`). Persisted, pushed on connect.
    pub(crate) alt_enable: bool,
    /// Alt-speaker trim in dB (`OUTPUT_BUS_ALT_TRIM_LEVEL` coeff, Q6.25, bipolar) — host-owned.
    pub(crate) alt_trim_db: f64,
    /// Output selections (coefficients, host-owned → local state): HP A/B gain mode, line op level.
    pub(crate) hp_a_mode: u16,
    pub(crate) hp_b_mode: u16,
    pub(crate) line_level_sel: u16,
    pub(crate) loopback_sel: u16,
    /// Monitor dim *amount* in dB (`OUTPUT_BUS_DIM_LEVEL`, Q6.25 coeff) — host-owned, persisted.
    pub(crate) dim_level_db: f64,
    /// Per-cue-bus talkback send level (dB) + pan (−1..+1), indexed `[Line 3-4, HP A, HP B]` (see
    /// `talkback_slot`). Each pair drives `TALKBACK_LEVEL`'s two legs via the mono pan law. Host-owned.
    pub(crate) tb_db: [f64; 3],
    pub(crate) tb_pan: [f64; 3],
    /// Selected Outputs row.
    pub(crate) out_sel: usize,
    /// Host-side monitor mix; the grid edits this and writes crosspoints.
    pub(crate) matrix: MixMatrix,
    /// Last crosspoint coefficient dB seen per cell index, so a device/echo VALUE for one leg can be
    /// recombined with the other leg to recover the cell's (fader, pan). See `apply_value`.
    pub(crate) xpoint_coeff_db: [f64; mixer::NUM_CELLS],
    /// Selected mix cell: (source row, destination column).
    pub(crate) mix_sel: (usize, usize),
    /// Per-source cut (mute) + solo overlays on the mix. Host-side and **ephemeral** (not persisted):
    /// the `matrix` keeps the real levels; a source is silenced on the device when `source_muted` is
    /// true (cut, or some other source soloed), but its stored gain/pan is preserved for un-cut.
    pub(crate) cut: [bool; mixer::NUM_SOURCES],
    pub(crate) solo: [bool; mixer::NUM_SOURCES],
    pub(crate) frames_seen: u64,
    pub(crate) last_fps_calc: Instant,
    pub(crate) fps: f64,
    pub(crate) fps_counter: u64,
    /// Transient status line (last save/load result, etc.).
    pub(crate) status: String,
    /// Whether the modal keybinding-help overlay is up (`?` toggles; any key dismisses).
    pub(crate) show_help: bool,
    pub(crate) should_quit: bool,
}

impl App {
    pub(crate) fn new(transport: Box<dyn Transport>) -> Self {
        let backend_label = transport.label().to_string();
        let mut app = App {
            transport,
            backend_label,
            screen: Screen::Meters,
            levels: [Sample {
                level: 0,
                clip: false,
            }; NUM_METERS],
            peaks: [meters::PeakHold::default(); NUM_METERS],
            last_meter_at: None,
            in_state: [[false; NUM_INPUT_CONTROLS]; NUM_INPUTS],
            in_sel: (0, 0),
            link: [false; 2],
            muted: false,
            talking: false,
            out_mono: false,
            out_dim: false,
            out_cut: false,
            out_phase_l: false,
            out_alt: false,
            alt_enable: false,
            alt_trim_db: 0.0,
            hp_a_mode: 0,
            hp_b_mode: 0,
            line_level_sel: 0,
            loopback_sel: 0,
            dim_level_db: preset::default_dim_level_db(),
            tb_db: [0.0; 3],
            tb_pan: [0.0; 3],
            out_sel: 0,
            matrix: MixMatrix::default(),
            xpoint_coeff_db: [f64::NEG_INFINITY; mixer::NUM_CELLS],
            mix_sel: (0, 0),
            cut: [false; mixer::NUM_SOURCES],
            solo: [false; mixer::NUM_SOURCES],
            frames_seen: 0,
            last_fps_calc: Instant::now(),
            fps: 0.0,
            fps_counter: 0,
            status: String::new(),
            show_help: false,
            should_quit: false,
        };
        // Construction stays pure: only the in-memory seeded default mix — no disk I/O and no device
        // traffic. The connect-time side effects (load preset, push host-owned state, hydrate params)
        // live in `connect`, which `main` calls — so unit tests can build an `App` without reading
        // the real config dir or depending on machine state.
        app.seed_default_mix();
        app
    }

    /// Run the connect-time sequence against the (now-open) transport: adopt the saved preset, then
    /// assert the host-owned state onto the device and pull the device-authoritative parameters.
    /// Kept out of `new` so construction has no side effects (no disk read, no DSP writes) and unit
    /// tests stay deterministic regardless of the machine's saved `mix.toml`.
    pub(crate) fn connect(&mut self) {
        // Adopt the saved mix + output/link settings over the seeded default, if a preset exists.
        self.load_mix();
        // The mix is host-authoritative: assert whatever we ended up with (loaded preset or the
        // seeded default) onto the device, so what's shown matches what's playing.
        self.push_full_mix();
        // Output selections (HP gain modes, line operating level) are host-owned coefficients too —
        // the device never reports them — so assert the loaded/default values on connect, the same
        // way the mix is pushed.
        self.push_output_settings();
        // Same for the analogue stereo-link flags (host-owned coefficient).
        self.push_links();
        // Parameters are device-authoritative: pull their current values so the Inputs/Outputs
        // screens hydrate from real hardware state.
        self.request_param_hydration();
    }

    /// Assert the host-owned output selections onto the device on connect. These are DSP
    /// coefficients the device never reports back, so the host owns + persists them (see
    /// `preset.rs`) and re-pushes them — mirroring `push_full_mix` for the crosspoint matrix.
    fn push_output_settings(&mut self) {
        self.send_out_sel(Coeff::HeadphonesGainMode, HP_A_BUS, self.hp_a_mode);
        self.send_out_sel(Coeff::HeadphonesGainMode, HP_B_BUS, self.hp_b_mode);
        self.send_out_sel(
            Coeff::LineOutputOperatingLevel,
            LINE_OUT_IDX,
            self.line_level_sel,
        );
        self.send_out_sel(Coeff::LoopbackSource, 0, self.loopback_sel);
        self.push_dim_level();
        // Alt-speaker host-owned coeffs (the live Alt switch is a device param, not pushed here).
        self.send_out_coeff_bool(Coeff::OutputBusAltSpkEnable, self.alt_enable);
        self.push_alt_trim();
        // Talkback sends (cue buses only — see TALKBACK_DESTS) re-assert level + pan on connect.
        for slot in 0..self.tb_db.len() {
            self.push_talkback(slot);
        }
    }

    /// Assert each analogue pair's stereo-link flag (`STEREO_LINK_CHANNELS`, idx 0 = ins 1-2,
    /// idx 2 = ins 3-4) on connect. The per-channel sends are restored by `push_full_mix`; this just
    /// re-tells the DSP which pairs are bonded so it tracks them the way SSL 360 does.
    fn push_links(&mut self) {
        for (pair, &on) in self.link.iter().enumerate() {
            let msg = protocol::dsp_bool(
                DspCode::CoefficientUpdateBool.num(),
                Coeff::StereoLinkChannels.num(),
                (pair * 2) as u16,
                on,
            );
            let _ = self.transport.send_dsp(&msg);
        }
    }

    /// Adopt persisted output selections into local state, clamped to the valid option counts in
    /// case the preset was hand-edited out of range.
    fn apply_output_settings(&mut self, o: &preset::OutputSettings) {
        self.hp_a_mode = o.hp_a_mode.min(HP_MODES.len() as u16 - 1);
        self.hp_b_mode = o.hp_b_mode.min(HP_MODES.len() as u16 - 1);
        self.line_level_sel = o.line_level_sel.min(LINE_LEVELS.len() as u16 - 1);
        self.loopback_sel = o.loopback_source_sel.min(LOOPBACK_SOURCES.len() as u16 - 1);
        self.dim_level_db = o.dim_level_db.clamp(DIM_MIN_DB, DIM_MAX_DB);
        self.alt_enable = o.alt_enable;
        self.alt_trim_db = o.alt_trim_db.clamp(ALT_TRIM_MIN_DB, ALT_TRIM_MAX_DB);
        for slot in 0..self.tb_db.len() {
            // Talk level shares the crosspoint dB bounds; below the floor it collapses to off (-inf).
            let db = o.tb_db[slot];
            self.tb_db[slot] = if db < mixer::MIN_DB {
                f64::NEG_INFINITY
            } else {
                db.min(mixer::MAX_DB)
            };
            self.tb_pan[slot] = o.tb_pan[slot].clamp(-1.0, 1.0);
        }
    }

    /// Ask the device for the current value of every input switch and monitor-bus param, so the
    /// Inputs/Outputs screens hydrate from hardware. Parameters are device-authoritative, but the
    /// SSL 12 doesn't reliably volunteer them: the bulk dump (`USB_REQUEST_CONTROL_STATES`, 0x2B) is
    /// unsupported, and any unsolicited `value reply` burst arrives during the version handshake's
    /// read loop and is discarded there. So we pull each one explicitly with `value request`;
    /// the replies arrive during the normal `pump` loop and fold in via `apply_value`. The mock
    /// answers these from its seeded state, exercising the same path. (Coefficients — the mix — are
    /// host-owned and never reported back, so there's nothing to pull for them.)
    fn request_param_hydration(&mut self) {
        // Per-input switches: request every valid (param, index) pair shown in the grid.
        for &(_, number, valid_rows) in INPUT_CONTROLS.iter() {
            for index in 0..valid_rows as u16 {
                let _ = self
                    .transport
                    .send_dsp(&protocol::dsp_value_request(number, index));
            }
        }
        // Monitor-bus params (idx 0) shown on the Outputs screen.
        for number in [
            Param::OutputBusMono,
            Param::OutputBusDim,
            Param::OutputBusCut,
            Param::OutputBusPhaseL,
            Param::OutputBusAlt,
        ] {
            let _ = self
                .transport
                .send_dsp(&protocol::dsp_value_request(number.num(), 0));
        }
    }

    /// A sensible starting monitor mix so the grid isn't empty: each playback pair routed to its
    /// "natural" bus at unity, plus Playback 1-2 into both headphone buses. Used as the fallback
    /// when no saved preset exists.
    fn seed_default_mix(&mut self) {
        // (source, dest, dB).  sources: 0=PB1-2 1=PB3-4 2=PB5-6 3=PB7-8.  dests: 0=Main 1=Line3-4 2=HP A 3=HP B.
        for &(s, d) in &[(0, 0), (0, 2), (0, 3), (1, 1), (2, 2), (3, 3)] {
            self.matrix.set_db(s, d, 0.0);
        }
    }

    /// Load a saved mix preset over the seeded default, if one exists. Sets the status line.
    fn load_mix(&mut self) {
        let Some(path) = preset::default_path() else {
            self.status = "no config dir for presets".into();
            return;
        };
        match preset::load(&path) {
            Ok(Some(p)) => {
                p.apply(&mut self.matrix);
                self.apply_output_settings(&p.outputs);
                self.link = p.links.to_array();
                self.status = format!("loaded mix from {}", path.display());
            }
            Ok(None) => self.status = "no saved mix — default loaded (s to save)".into(),
            Err(e) => self.status = format!("mix load error: {e}"),
        }
    }

    /// Save the current mix to the default preset path.
    fn save_mix(&mut self) {
        let Some(path) = preset::default_path() else {
            self.status = "no config dir for presets".into();
            return;
        };
        let mut p = MixPreset::from_matrix(&self.matrix);
        p.outputs = preset::OutputSettings {
            hp_a_mode: self.hp_a_mode,
            hp_b_mode: self.hp_b_mode,
            line_level_sel: self.line_level_sel,
            loopback_source_sel: self.loopback_sel,
            dim_level_db: self.dim_level_db,
            alt_enable: self.alt_enable,
            alt_trim_db: self.alt_trim_db,
            tb_db: self.tb_db,
            tb_pan: self.tb_pan,
        };
        p.links = preset::LinkSettings::from_array(self.link);
        self.status = match preset::save(&p, &path) {
            Ok(()) => format!("saved mix to {}", path.display()),
            Err(e) => format!("mix save error: {e}"),
        };
    }

    /// Push every mix cell to the device (host asserts its mix on connect). Wrapped in the
    /// `MUTE_HARDWARE_OUTPUTS` reconfigure guard — the same thing SSL 360 does — so rewriting all
    /// 240 crosspoints one at a time doesn't pop/zipper the outputs. This guard is independent of
    /// the user-facing `muted` flag.
    fn push_full_mix(&mut self) {
        self.set_hw_mute(true);
        // Make the crosspoint matrix authoritative for every bus we drive. The device's power-on
        // default has the secondary buses (Line 3-4, HP A, HP B) set to "follow mix 1-2" — they
        // mirror the Main mix and the DSP ignores their own crosspoints (PROTOCOL.md §9b). Since
        // coefficients are host-authoritative and never reported back, we must clear follow here,
        // or per-bus edits (e.g. removing Playback 1-2 from the headphones) have no audible effect.
        self.clear_bus_follow();
        for s in 0..mixer::NUM_SOURCES {
            for d in 0..mixer::NUM_DESTINATIONS {
                self.write_mix_cell(s, d);
            }
        }
        self.set_hw_mute(false);
    }

    /// Disable `OUTPUT_BUSES_FOLLOW_1_2` for every non-Main destination bus, so its crosspoint
    /// cells (not a mirror of the Main mix) determine what it outputs. The per-bus coefficient
    /// index is the bus's left destination block (Main 0, Line 3-4 2, HP A 4, HP B 6).
    fn clear_bus_follow(&mut self) {
        for dest in mixer::DESTINATIONS {
            if dest == mixer::MAIN {
                continue; // Main is the source of the follow; it can't follow itself.
            }
            let msg = protocol::dsp_bool(
                DspCode::CoefficientUpdateBool.num(),
                Coeff::OutputBusesFollow1_2.num(),
                dest.left_block,
                false,
            );
            let _ = self.transport.send_dsp(&msg);
        }
    }

    /// Send the `MUTE_HARDWARE_OUTPUTS` coefficient directly (does NOT touch the user `muted` flag).
    fn set_hw_mute(&mut self, on: bool) {
        let msg = protocol::dsp_bool(
            DspCode::CoefficientUpdateBool.num(),
            Coeff::MuteHardwareOutputs.num(),
            0,
            on,
        );
        let _ = self.transport.send_dsp(&msg);
    }

    /// Pump the transport and fold device→host frames into UI state.
    pub(crate) fn pump(&mut self) {
        let frames = match self.transport.poll() {
            Ok(f) => f,
            Err(e) => {
                // Surface the error in the bottom status line rather than tearing down the UI (and
                // without clobbering the backend tag in the title).
                self.status = format!("transport error: {e}");
                return;
            }
        };
        for f in frames {
            if f.code != protocol::USB_RECV_DSP {
                continue;
            }
            if let Some(upd) = meters::parse(&f.payload) {
                self.frames_seen += 1;
                self.fps_counter += 1;
                let now = Instant::now();
                let dt = self
                    .last_meter_at
                    .map_or(Duration::ZERO, |t| now.saturating_duration_since(t));
                self.last_meter_at = Some(now);
                for (idx, _label, s) in upd.labelled() {
                    if (idx as usize) < NUM_METERS {
                        self.levels[idx as usize] = s;
                        self.peaks[idx as usize].update(s, dt);
                    }
                }
                continue;
            }
            self.apply_value(&f.payload);
        }
        if self.last_fps_calc.elapsed() >= Duration::from_millis(500) {
            self.fps = self.fps_counter as f64 / self.last_fps_calc.elapsed().as_secs_f64();
            self.fps_counter = 0;
            self.last_fps_calc = Instant::now();
        }
    }

    /// Reconcile a device VALUE message into UI state. Parameters are device-authoritative (hydrated
    /// from the connect dump and unsolicited changes); the mix is host-owned but the mock echoes it.
    fn apply_value(&mut self, dsp: &[u8]) {
        use protocol::{DspMessage, DspValue};
        let Some(m) = DspMessage::parse(dsp) else {
            return;
        };

        // Bool parameter (device-authoritative) → Inputs grid or the monitor-bus toggles.
        if m.code == DspCode::ParamValueBool.num() {
            if let DspValue::Bool(val) = m.value {
                for (col, &(_, pnum, valid_rows)) in INPUT_CONTROLS.iter().enumerate() {
                    if pnum == m.number && (m.index as usize) < valid_rows {
                        self.in_state[m.index as usize][col] = val;
                        return;
                    }
                }
                if m.index == 0 {
                    match Param::try_from(m.number) {
                        Ok(Param::OutputBusMono) => self.out_mono = val,
                        Ok(Param::OutputBusDim) => self.out_dim = val,
                        Ok(Param::OutputBusCut) => self.out_cut = val,
                        Ok(Param::OutputBusPhaseL) => self.out_phase_l = val,
                        Ok(Param::OutputBusAlt) => self.out_alt = val,
                        _ => {}
                    }
                }
            }
            return;
        }

        // Output selection coefficient (HP gain mode / line op level). Host-owned, but the mock
        // echoes the write back so the reconcile path runs.
        if m.code == DspCode::CoefficientUpdateSelection.num() {
            if let DspValue::Selection(sel) = m.value {
                match (Coeff::try_from(m.number), m.index) {
                    (Ok(Coeff::HeadphonesGainMode), HP_A_BUS) => self.hp_a_mode = sel,
                    (Ok(Coeff::HeadphonesGainMode), HP_B_BUS) => self.hp_b_mode = sel,
                    (Ok(Coeff::LineOutputOperatingLevel), LINE_OUT_IDX) => {
                        self.line_level_sel = sel
                    }
                    _ => {}
                }
            }
            return;
        }

        // Crosspoint coefficient (device→host echo carries the Q6.25 coeff update code).
        if m.code == DspCode::CoefficientUpdateQ625.num()
            && m.number == Coeff::MixerCrosspointTable.num()
        {
            if let DspValue::Q625(raw) = m.value {
                let index = m.index;
                if (index as usize) < mixer::NUM_CELLS {
                    self.xpoint_coeff_db[index as usize] = protocol::q625_to_db(raw);
                }
                if let Some((s, d)) = MixMatrix::locate(index) {
                    // While a source is cut/solo-silenced we deliberately write its cells to off; an
                    // echo of that 0 must not clobber the stored level. Real hardware never echoes
                    // coefficients, so this only guards the mock's echo — keeping un-cut restorable.
                    if !self.source_muted(s) {
                        // A cell spans two legs; recombine both cached coefficients into (fader, pan).
                        // The device reports *coefficient* dB per leg; the grid holds *fader* dB + pan.
                        let legs = mixer::cells_for(mixer::SOURCES[s], mixer::DESTINATIONS[d]);
                        let leg_db = |cell: u16| self.xpoint_coeff_db[cell as usize];
                        let (fader_db, pan) = mixer::leg_coeffs_to_fader_pan(
                            mixer::SOURCES[s],
                            leg_db(legs[0]),
                            leg_db(legs[1]),
                        );
                        self.matrix.set_db(s, d, fader_db);
                        self.matrix.set_pan(s, d, pan);
                    }
                }
            }
        }
    }

    /// Toggle the selected input control, writing the param and updating local state optimistically.
    /// If the input is stereo-linked, the same value is applied to its partner (where valid).
    fn toggle_input(&mut self, row: usize, col: usize) {
        let (_, number, valid_rows) = INPUT_CONTROLS[col];
        if row >= valid_rows {
            return; // e.g. Hi-Z on inputs 3/4 — not a valid control
        }
        let next = !self.in_state[row][col];
        self.set_input(row, col, number, next);
        if let Some(partner) = self.linked_partner_input(row) {
            if partner < valid_rows {
                self.set_input(partner, col, number, next);
            }
        }
    }

    /// Set one input switch's local state and send its `bool update` (no link mirroring).
    fn set_input(&mut self, row: usize, col: usize, number: u16, value: bool) {
        self.in_state[row][col] = value;
        let msg = protocol::dsp_bool(DspCode::ParamUpdateBool.num(), number, row as u16, value);
        let _ = self.transport.send_dsp(&msg);
    }

    /// The partner input row of `row` if its pair is stereo-linked, else `None`. Pairs are
    /// (0,1) and (2,3); the partner is `row ^ 1`.
    fn linked_partner_input(&self, row: usize) -> Option<usize> {
        self.link
            .get(row / 2)
            .copied()
            .unwrap_or(false)
            .then_some(row ^ 1)
    }

    /// The partner MixMatrix source of analogue source `s` if its pair is linked, else `None`.
    /// Playback sources (below the analogue block) are never linked.
    pub(crate) fn linked_partner_source(&self, s: usize) -> Option<usize> {
        let base = analogue_source(0);
        let row = s.checked_sub(base)?; // None for the playback sources
        self.linked_partner_input(row).map(analogue_source)
    }

    /// Toggle the stereo link for the pair owning input `row`. On linking, asserts
    /// `STEREO_LINK_CHANNELS`, copies the pair's lower channel onto its partner (switches + mixer
    /// sends), and **seeds a hard L/R stereo spread** (lower channel → L, upper → R) as the default —
    /// each channel can then be panned independently; on unlinking, just clears the flag and leaves
    /// values as-is.
    fn toggle_link(&mut self, row: usize) {
        let pair = row / 2;
        let on = !self.link[pair];
        self.link[pair] = on;
        let msg = protocol::dsp_bool(
            DspCode::CoefficientUpdateBool.num(),
            Coeff::StereoLinkChannels.num(),
            (pair * 2) as u16, // idx 0 = ins 1-2, idx 2 = ins 3-4
            on,
        );
        let _ = self.transport.send_dsp(&msg);
        if on {
            self.match_pair(pair);
        }
        self.status = format!(
            "inputs {} {}",
            LINK_PAIR_NAMES[pair],
            if on { "linked" } else { "unlinked" }
        );
    }

    /// Match a freshly-linked pair (lower channel = row `2*pair`): copy every valid input switch onto
    /// the partner, then mirror the lower channel's send gain to the upper and **seed a hard L/R
    /// spread** (lower → hard-left, upper → hard-right) as the default pan across every destination
    /// (the user can pan each channel off this default afterward).
    fn match_pair(&mut self, pair: usize) {
        let (lo, hi) = (pair * 2, pair * 2 + 1);
        for (col, &(_, number, valid_rows)) in INPUT_CONTROLS.iter().enumerate() {
            if hi < valid_rows && self.in_state[hi][col] != self.in_state[lo][col] {
                let v = self.in_state[lo][col];
                self.set_input(hi, col, number, v);
            }
        }
        let (s_lo, s_hi) = (analogue_source(lo), analogue_source(hi));
        for d in 0..mixer::NUM_DESTINATIONS {
            self.matrix.set_db(s_hi, d, self.matrix.db(s_lo, d));
            self.matrix.set_pan(s_lo, d, -1.0); // lower channel hard-left
            self.matrix.set_pan(s_hi, d, 1.0); // upper channel hard-right
            self.write_mix_cell(s_lo, d);
            self.write_mix_cell(s_hi, d);
        }
    }

    /// Whether any source is soloed (so every non-soloed source is silenced).
    fn any_solo(&self) -> bool {
        self.solo.iter().any(|&s| s)
    }

    /// Whether source `s` is currently silenced on the device — cut, or muted because another source
    /// is soloed. Its stored matrix gain is untouched; only the coefficient we *write* goes to off.
    pub(crate) fn source_muted(&self, s: usize) -> bool {
        self.cut[s] || (self.any_solo() && !self.solo[s])
    }

    /// Write the currently-selected mix cell's gain to the device as crosspoint coefficients. A
    /// cut/solo-silenced source writes its cells as off (coefficient 0) while keeping the stored gain.
    fn write_mix_cell(&mut self, source: usize, dest: usize) {
        let muted = self.source_muted(source);
        for (idx, raw) in self.matrix.cell_writes(source, dest) {
            let raw = if muted { 0 } else { raw };
            let msg = protocol::dsp_q625(
                DspCode::CoefficientUpdateQ625.num(),
                Coeff::MixerCrosspointTable.num(),
                idx,
                raw,
            );
            let _ = self.transport.send_dsp(&msg);
        }
    }

    /// Re-push every crosspoint cell (no hw-mute wrapper — idempotent re-writes are transparent, see
    /// ROADMAP §7). Used when a cut/solo toggle changes which sources are silenced.
    fn repush_all_cells(&mut self) {
        for s in 0..mixer::NUM_SOURCES {
            for d in 0..mixer::NUM_DESTINATIONS {
                self.write_mix_cell(s, d);
            }
        }
    }

    /// Toggle cut (mute) on a source row, mirroring to its stereo-linked partner so a linked pair
    /// cuts together, then re-push so the device reflects the new silenced set.
    fn toggle_cut(&mut self, s: usize) {
        let on = !self.cut[s];
        self.cut[s] = on;
        if let Some(p) = self.linked_partner_source(s) {
            self.cut[p] = on;
        }
        self.repush_all_cells();
    }

    /// Toggle solo on a source row (mirroring to its linked partner). Solo silences every other
    /// source, so the whole matrix is re-pushed.
    fn toggle_solo(&mut self, s: usize) {
        let on = !self.solo[s];
        self.solo[s] = on;
        if let Some(p) = self.linked_partner_source(s) {
            self.solo[p] = on;
        }
        self.repush_all_cells();
    }

    /// Set a mix cell's gain and push it; if its source is stereo-linked, mirror the resulting gain
    /// onto the partner cell so the pair stays locked. The single entry point for grid edits.
    fn set_mix_cell(&mut self, s: usize, d: usize, db: f64) {
        self.matrix.set_db(s, d, db);
        self.write_mix_cell(s, d);
        self.mirror_linked_cell(s, d);
    }

    /// Nudge a mix cell's gain and push it, mirroring onto a linked partner (see `set_mix_cell`).
    fn nudge_mix_cell(&mut self, s: usize, d: usize, delta: f64) {
        self.matrix.nudge(s, d, delta);
        self.write_mix_cell(s, d);
        self.mirror_linked_cell(s, d);
    }

    /// Nudge a mix cell's pan and push it (pan distributes the gain across the L/R legs, so this
    /// rewrites both crosspoint cells). On a stereo-linked pair each channel pans independently —
    /// linking only seeds the hard L/R spread as a default (see `match_pair`); it doesn't lock pan.
    fn nudge_mix_pan(&mut self, s: usize, d: usize, delta: f64) {
        self.matrix.nudge_pan(s, d, delta);
        self.write_mix_cell(s, d);
    }

    /// Recenter a mix cell's pan and push it.
    fn center_mix_pan(&mut self, s: usize, d: usize) {
        self.matrix.set_pan(s, d, 0.0);
        self.write_mix_cell(s, d);
    }

    /// Mirror cell `(s, d)`'s gain onto its linked partner (same destination) so a linked pair tracks
    /// one send level. Pan is left untouched — each channel pans independently (linking only seeds the
    /// hard L/R spread, see `match_pair`). No-op when `s` isn't part of a linked pair.
    fn mirror_linked_cell(&mut self, s: usize, d: usize) {
        if let Some(p) = self.linked_partner_source(s) {
            self.matrix.set_db(p, d, self.matrix.db(s, d));
            self.write_mix_cell(p, d);
        }
    }

    fn toggle_mute(&mut self) {
        self.muted = !self.muted;
        self.set_hw_mute(self.muted);
    }

    /// Open/close the talkback mic (`OUTPUT_BUS_TALKBACK_ENABLE`, param idx 0). Press-to-toggle
    /// because terminals don't reliably report key-up; the title bar shows `●TALK` while it's open.
    fn toggle_talk(&mut self) {
        self.talking = !self.talking;
        self.send_out_bool(Param::OutputBusTalkbackEnable, self.talking);
    }

    /// Send a monitor-bus bool param (idx 0) and remember it locally.
    fn send_out_bool(&mut self, number: Param, value: bool) {
        let msg = protocol::dsp_bool(DspCode::ParamUpdateBool.num(), number.num(), 0, value);
        let _ = self.transport.send_dsp(&msg);
    }

    /// Send an output selection coefficient and remember it locally.
    fn send_out_sel(&mut self, number: Coeff, index: u16, sel: u16) {
        let msg = protocol::dsp_selection(
            DspCode::CoefficientUpdateSelection.num(),
            number.num(),
            index,
            sel,
        );
        let _ = self.transport.send_dsp(&msg);
    }

    /// Send a host-owned **coefficient** bool (idx 0) — e.g. the alt-speaker enable. (Distinct from
    /// `send_out_bool`, which sends a device *param*.)
    fn send_out_coeff_bool(&mut self, number: Coeff, value: bool) {
        let msg = protocol::dsp_bool(DspCode::CoefficientUpdateBool.num(), number.num(), 0, value);
        let _ = self.transport.send_dsp(&msg);
    }

    /// Send an output Q6.25 coefficient carrying a dB level (the dim amount). Mirrors `write_mix_cell`
    /// but for a single host-owned output coefficient rather than a crosspoint cell.
    fn send_out_q625(&mut self, number: Coeff, index: u16, db: f64) {
        let msg = protocol::dsp_q625(
            DspCode::CoefficientUpdateQ625.num(),
            number.num(),
            index,
            protocol::db_to_q625(db),
        );
        let _ = self.transport.send_dsp(&msg);
    }

    /// Push the dim *amount* as a Q6.25 coefficient. `dim_level_db` is UI dB where 0 = unity (no
    /// cut), but the device's "0 dB" coefficient is the −3.01 dB `ZERO_DB_REF`, so add
    /// `DEVICE_REF_OFFSET_DB` to land true unity at 0 dB — the same fader→device-ref mapping the
    /// crosspoint matrix uses. Without it, engaging Dim at 0 dB quietly drops the bus ~3 dB.
    fn push_dim_level(&mut self) {
        self.send_out_q625(
            Coeff::OutputBusDimLevel,
            0,
            self.dim_level_db + mixer::DEVICE_REF_OFFSET_DB,
        );
    }

    /// Push the alt-speaker trim (Q6.25). Bipolar dB where 0 = unity, so it takes the same
    /// `DEVICE_REF_OFFSET_DB` correction as the dim level / crosspoints.
    fn push_alt_trim(&mut self) {
        self.send_out_q625(
            Coeff::OutputBusAltTrimLevel,
            0,
            self.alt_trim_db + mixer::DEVICE_REF_OFFSET_DB,
        );
    }

    /// Push one cue bus's talkback send (level + pan) to its two `TALKBACK_LEVEL` legs. The mono
    /// pan law distributes the level across L/R exactly like a crosspoint cell — and already folds in
    /// the `DEVICE_REF_OFFSET_DB`, so the leg dB goes straight to `send_out_q625` (no extra offset).
    fn push_talkback(&mut self, slot: usize) {
        let dest = mixer::DESTINATIONS[slot + 1]; // slot 0..=2 → Line 3-4 / HP A / HP B
        let (l_db, r_db) =
            mixer::fader_pan_to_leg_coeffs(TALK_SOURCE, self.tb_db[slot], self.tb_pan[slot]);
        self.send_out_q625(Coeff::TalkbackLevel, dest.left_block, l_db);
        self.send_out_q625(Coeff::TalkbackLevel, dest.right_block, r_db);
    }

    /// Talk-row cell edits, keyed by the Mixer's destination column. Each mirrors its crosspoint
    /// counterpart (`nudge`/`set_db`/pan) but on the host-owned talkback send; all no-op on the Main
    /// column, which carries no talkback (`talkback_slot` → `None`).
    fn nudge_talk(&mut self, dest: usize, delta: f64) {
        let Some(slot) = talkback_slot(dest) else {
            return;
        };
        let cur = self.tb_db[slot];
        // Match the crosspoint nudge: up from -inf (off) re-enters at MIN_DB; down from MIN_DB drops off.
        self.tb_db[slot] = if cur.is_infinite() {
            if delta > 0.0 {
                mixer::MIN_DB
            } else {
                return;
            }
        } else {
            (cur + delta).min(mixer::MAX_DB)
        };
        self.push_talkback(slot);
    }

    fn set_talk_db(&mut self, dest: usize, db: f64) {
        let Some(slot) = talkback_slot(dest) else {
            return;
        };
        self.tb_db[slot] = if db < mixer::MIN_DB {
            f64::NEG_INFINITY
        } else {
            db.min(mixer::MAX_DB)
        };
        self.push_talkback(slot);
    }

    fn nudge_talk_pan(&mut self, dest: usize, delta: f64) {
        let Some(slot) = talkback_slot(dest) else {
            return;
        };
        self.tb_pan[slot] = (self.tb_pan[slot] + delta).clamp(-1.0, 1.0);
        self.push_talkback(slot);
    }

    fn set_talk_pan(&mut self, dest: usize, pan: f64) {
        let Some(slot) = talkback_slot(dest) else {
            return;
        };
        self.tb_pan[slot] = pan.clamp(-1.0, 1.0);
        self.push_talkback(slot);
    }

    /// Space on the selected Outputs row: dispatch to that row's action.
    fn output_activate(&mut self, row: usize) {
        OutRow::ALL[row].activate(self);
    }

    /// ←/→ on the selected Outputs row: cycle it (a no-op on bool rows).
    fn output_cycle(&mut self, row: usize, delta: i32) {
        OutRow::ALL[row].cycle(self, delta);
    }

    pub(crate) fn on_key(&mut self, code: KeyCode) {
        // The help overlay is modal: while it's up, any key just dismisses it and nothing else
        // fires — so a curious keypress can't accidentally toggle a control behind the popup.
        if self.show_help {
            self.show_help = false;
            return;
        }
        // Global keys, available on every screen.
        match code {
            KeyCode::Char('q') | KeyCode::Esc => {
                self.should_quit = true;
                return;
            }
            KeyCode::Char('?') => {
                self.show_help = true;
                return;
            }
            KeyCode::Tab => {
                self.screen = match self.screen {
                    Screen::Meters => Screen::Inputs,
                    Screen::Inputs => Screen::Outputs,
                    Screen::Outputs => Screen::Mixer,
                    Screen::Mixer => Screen::Meters,
                };
                return;
            }
            KeyCode::BackTab => {
                self.screen = match self.screen {
                    Screen::Meters => Screen::Mixer,
                    Screen::Inputs => Screen::Meters,
                    Screen::Outputs => Screen::Inputs,
                    Screen::Mixer => Screen::Outputs,
                };
                return;
            }
            KeyCode::Char('m') => {
                self.toggle_mute();
                return;
            }
            KeyCode::Char('t') => {
                self.toggle_talk();
                return;
            }
            _ => {}
        }
        match self.screen {
            Screen::Meters => self.on_key_meters(code),
            Screen::Inputs => self.on_key_inputs(code),
            Screen::Outputs => self.on_key_outputs(code),
            Screen::Mixer => self.on_key_mixer(code),
        }
    }

    fn on_key_meters(&mut self, code: KeyCode) {
        if let KeyCode::Char('c') = code {
            for p in self.peaks.iter_mut() {
                p.clear();
            }
            self.status = "cleared peak-hold + clip latches".into();
        }
    }

    fn on_key_outputs(&mut self, code: KeyCode) {
        match code {
            KeyCode::Up | KeyCode::Char('k') => self.out_sel = self.out_sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.out_sel = (self.out_sel + 1).min(NUM_OUT_ROWS - 1)
            }
            KeyCode::Char(' ') | KeyCode::Enter => self.output_activate(self.out_sel),
            KeyCode::Left | KeyCode::Char('h') => self.output_cycle(self.out_sel, -1),
            KeyCode::Right | KeyCode::Char('l') => self.output_cycle(self.out_sel, 1),
            // The preset now also carries the host-owned output selections, so `s` persists them
            // from here too (same save as the Mixer screen).
            KeyCode::Char('s') => self.save_mix(),
            _ => {}
        }
    }

    fn on_key_inputs(&mut self, code: KeyCode) {
        let (mut r, mut c) = self.in_sel;
        match code {
            KeyCode::Up | KeyCode::Char('k') => r = r.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => r = (r + 1).min(NUM_INPUTS - 1),
            KeyCode::Left | KeyCode::Char('h') => c = c.saturating_sub(1),
            KeyCode::Right | KeyCode::Char('l') => c = (c + 1).min(NUM_INPUT_CONTROLS - 1),
            KeyCode::Char(' ') | KeyCode::Enter => self.toggle_input(r, c),
            KeyCode::Char('p') => self.toggle_link(r),
            _ => {}
        }
        self.in_sel = (r, c);
    }

    fn on_key_mixer(&mut self, code: KeyCode) {
        let (mut s, mut d) = self.mix_sel;
        // The talkback send row sits one past the matrix; its cells take the same keys as crosspoint
        // cells but edit the host-owned talk send (`nudge_talk` &co.) instead of the matrix.
        let on_talk = s == MIXER_TALK_ROW;
        match code {
            KeyCode::Up | KeyCode::Char('k') => s = s.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => s = (s + 1).min(MIXER_TALK_ROW),
            KeyCode::Left | KeyCode::Char('h') => d = d.saturating_sub(1),
            KeyCode::Right | KeyCode::Char('l') => d = (d + 1).min(mixer::NUM_DESTINATIONS - 1),
            KeyCode::Char('+') | KeyCode::Char('=') if on_talk => self.nudge_talk(d, 1.0),
            KeyCode::Char('-') | KeyCode::Char('_') if on_talk => self.nudge_talk(d, -1.0),
            KeyCode::Char('0') if on_talk => self.set_talk_db(d, 0.0),
            KeyCode::Char('[') if on_talk => self.nudge_talk_pan(d, -PAN_STEP),
            KeyCode::Char(']') if on_talk => self.nudge_talk_pan(d, PAN_STEP),
            KeyCode::Char('\\') if on_talk => self.set_talk_pan(d, 0.0),
            KeyCode::Backspace | KeyCode::Char('x') | KeyCode::Char('.') if on_talk => {
                self.set_talk_db(d, f64::NEG_INFINITY)
            }
            KeyCode::Char('+') | KeyCode::Char('=') => self.nudge_mix_cell(s, d, 1.0),
            KeyCode::Char('-') | KeyCode::Char('_') => self.nudge_mix_cell(s, d, -1.0),
            KeyCode::Char('0') => self.set_mix_cell(s, d, 0.0),
            // [ / ] pan the selected send left/right; \ recenters.
            KeyCode::Char('[') => self.nudge_mix_pan(s, d, -PAN_STEP),
            KeyCode::Char(']') => self.nudge_mix_pan(s, d, PAN_STEP),
            KeyCode::Char('\\') => self.center_mix_pan(s, d),
            // Backspace / x / '.' = mute this send (-inf).
            KeyCode::Backspace | KeyCode::Char('x') | KeyCode::Char('.') => {
                self.set_mix_cell(s, d, f64::NEG_INFINITY)
            }
            // Row-level cut/solo apply to the selected source (not the talkback row).
            KeyCode::Char('c') if !on_talk => self.toggle_cut(s),
            KeyCode::Char('o') if !on_talk => self.toggle_solo(s),
            KeyCode::Char('s') => self.save_mix(),
            _ => {}
        }
        self.mix_sel = (s, d);
    }

    /// Scroll wheel. On the Mixer it nudges the highlighted cell's gain (the fast way to make a big
    /// change) — same step as +/-. Elsewhere it moves the row selection up/down, the natural mapping.
    pub(crate) fn on_scroll(&mut self, up: bool) {
        match self.screen {
            Screen::Mixer => {
                let (s, d) = self.mix_sel;
                let delta = if up { 1.0 } else { -1.0 };
                if s == MIXER_TALK_ROW {
                    self.nudge_talk(d, delta);
                } else {
                    self.nudge_mix_cell(s, d, delta);
                }
            }
            Screen::Inputs => self.on_key_inputs(if up { KeyCode::Up } else { KeyCode::Down }),
            Screen::Outputs => self.on_key_outputs(if up { KeyCode::Up } else { KeyCode::Down }),
            Screen::Meters => {}
        }
    }
}

fn cycle(cur: u16, len: usize, delta: i32) -> u16 {
    let n = len as i32;
    (((cur as i32 + delta) % n + n) % n) as u16
}

pub(crate) fn onoff(b: bool) -> String {
    if b {
        "ON".to_string()
    } else {
        "off".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::{
        dashboard_fits, draw, meter_bar, meter_scale, DASHBOARD_MIN_HEIGHT, DASHBOARD_MIN_WIDTH,
    };
    use ssl12_ctl::transport::MockTransport;

    /// The Inputs grid hydrates from the device's connect-time parameter dump (via the mock).
    /// Exercises poll → pump → apply_value → in_state end-to-end with no terminal.
    #[test]
    fn inputs_hydrate_from_connect_dump() {
        let mut app = App::new(Box::new(MockTransport::new()));
        app.pump(); // first poll delivers the whole connect dump
                    // Columns: 0=48V 1=HPF 2=Line 3=Hi-Z 4=Ø. Values from MockTransport::build_connect_dump.
        assert!(app.in_state[0][0], "48V on input 1");
        assert!(app.in_state[0][1], "HPF on input 1");
        assert!(app.in_state[1][2], "Line on input 2");
        assert!(app.in_state[0][3], "Hi-Z on input 1");
        assert!(app.in_state[3][4], "polarity on input 4");
        assert!(!app.in_state[1][0], "48V off on input 2");
    }

    /// The Inputs grid hydrates purely from explicit `value request`s — the path the real
    /// device needs, since it doesn't volunteer the dump the way the mock's first poll does. We
    /// drain (and discard) the mock's unsolicited dump, blank local state, then prove a fresh
    /// `request_param_hydration` re-fills the grid from the device's request→reply answers alone.
    #[test]
    fn inputs_hydrate_via_explicit_requests() {
        let mut app = App::new(Box::new(MockTransport::new()));
        app.pump(); // consume the unsolicited connect dump...
        app.in_state = [[false; NUM_INPUT_CONTROLS]; NUM_INPUTS]; // ...then forget what it told us.
        app.request_param_hydration();
        app.pump(); // only the request replies can re-hydrate now
        assert!(
            app.in_state[0][0],
            "48V on input 1 (from request reply, not the dump)"
        );
        assert!(app.in_state[1][2], "Line on input 2");
        assert!(app.in_state[0][3], "Hi-Z on input 1");
        assert!(app.in_state[3][4], "polarity on input 4");
    }

    /// Toggling an input control flips local state and (via the mock echo) reconciles back the same.
    #[test]
    fn toggling_input_round_trips_through_mock() {
        let mut app = App::new(Box::new(MockTransport::new()));
        app.pump(); // drain connect dump first
        let before = app.in_state[2][1]; // input 3, HPF
        app.toggle_input(2, 1);
        assert_eq!(app.in_state[2][1], !before, "optimistic local flip");
        app.pump(); // the mock echoes bool value; reconciliation must agree
        assert_eq!(app.in_state[2][1], !before, "device echo agrees");
    }

    /// A mix cell's pan survives the full round-trip: edit → write both legs → mock echoes them →
    /// the reconcile cache recombines the two legs back into the same (fader, pan).
    #[test]
    fn mix_pan_round_trips_through_mock() {
        let mut app = App::new(Box::new(MockTransport::new()));
        let (s, d) = (4, 0); // Analogue 1 (mono, equal-power pan) → Main
        app.set_mix_cell(s, d, -6.0); // on at −6 dB fader, centered
        app.nudge_mix_pan(s, d, -0.5); // pan halfway left
        assert!(
            (app.matrix.pan(s, d) + 0.5).abs() < 1e-9,
            "optimistic pan set"
        );
        for _ in 0..4 {
            app.pump(); // drain the per-leg coefficient echoes
        }
        assert!(
            (app.matrix.db(s, d) + 6.0).abs() < 0.05,
            "fader recovered from legs: {}",
            app.matrix.db(s, d)
        );
        assert!(
            (app.matrix.pan(s, d) + 0.5).abs() < 0.02,
            "pan recovered from legs: {}",
            app.matrix.pan(s, d)
        );
    }

    /// Linking an analogue pair seeds a hard L/R spread (lower → L, upper → R) as the default; gain
    /// stays mirrored across the pair, but each channel's pan is then independently adjustable.
    #[test]
    fn stereo_link_seeds_hard_pan_then_pans_independently() {
        let mut app = App::new(Box::new(MockTransport::new()));
        let (s_lo, s_hi) = (analogue_source(0), analogue_source(1));
        app.set_mix_cell(s_lo, 0, -3.0); // bring the pair's source on into Main
        app.toggle_link(0); // link 1-2 → default hard-pan L/R
        assert_eq!(
            app.matrix.pan(s_lo, 0),
            -1.0,
            "lower channel defaults hard-left"
        );
        assert_eq!(
            app.matrix.pan(s_hi, 0),
            1.0,
            "upper channel defaults hard-right"
        );

        // A gain edit mirrors to the partner and leaves the seeded spread untouched.
        app.nudge_mix_cell(s_lo, 0, -2.0);
        assert_eq!(
            app.matrix.db(s_hi, 0),
            app.matrix.db(s_lo, 0),
            "gain mirrored across the pair"
        );
        assert_eq!(
            app.matrix.pan(s_lo, 0),
            -1.0,
            "gain edit doesn't disturb pan"
        );
        assert_eq!(
            app.matrix.pan(s_hi, 0),
            1.0,
            "gain edit doesn't disturb pan"
        );

        // Each channel pans independently while linked; the partner is unaffected.
        app.nudge_mix_pan(s_lo, 0, 0.5);
        assert!(
            (app.matrix.pan(s_lo, 0) - (-0.5)).abs() < 1e-9,
            "lower channel pans freely while linked"
        );
        assert_eq!(
            app.matrix.pan(s_hi, 0),
            1.0,
            "partner's pan is unaffected by the other channel's pan edit"
        );
    }

    /// Stereo-linking an analogue pair mirrors both input switches and mixer sends across the pair,
    /// and unlinking stops the mirroring. Exercises `toggle_link`/`toggle_input`/`set_mix_cell`.
    #[test]
    fn stereo_link_mirrors_switches_and_sends() {
        let mut app = App::new(Box::new(MockTransport::new()));
        app.pump(); // drain the connect dump so input switches hold known state

        assert!(!app.link[0], "pair 1-2 starts unlinked");
        app.toggle_link(0); // link inputs 1-2
        assert!(app.link[0], "pair 1-2 now linked");

        // A switch edit on channel 1 (48V, col 0) also applies to channel 2.
        let col = 0;
        let partner_before = app.in_state[1][col];
        app.toggle_input(0, col);
        assert_eq!(
            app.in_state[0][col], app.in_state[1][col],
            "48V mirrored across the linked pair"
        );
        assert_ne!(
            app.in_state[1][col], partner_before,
            "partner channel actually changed"
        );

        // A mixer send on Analogue 1 mirrors onto Analogue 2 (sources 4 and 5).
        let (s_lo, s_hi) = (analogue_source(0), analogue_source(1));
        app.set_mix_cell(s_lo, 0, -4.0);
        assert_eq!(
            app.matrix.db(s_hi, 0),
            -4.0,
            "send mirrored to the linked partner"
        );

        // After unlinking, the partner no longer follows.
        app.toggle_link(0);
        assert!(!app.link[0], "pair 1-2 unlinked");
        app.set_mix_cell(s_lo, 0, -10.0);
        assert_eq!(
            app.matrix.db(s_hi, 0),
            -4.0,
            "partner is left as-is once unlinked"
        );
    }

    /// The Outputs screen's monitor-bus params hydrate from the connect dump (Dim was seeded on).
    #[test]
    fn outputs_hydrate_monitor_dim() {
        let mut app = App::new(Box::new(MockTransport::new()));
        app.pump();
        assert!(app.out_dim, "Dim hydrated on from the device dump");
        assert!(!app.out_mono, "Mono hydrated off");
    }

    /// A monitor-bus toggle (param) round-trips through the mock echo.
    #[test]
    fn output_mono_toggle_round_trips() {
        let mut app = App::new(Box::new(MockTransport::new()));
        app.pump();
        let before = app.out_mono;
        app.output_activate(OutRow::Mono.index());
        assert_eq!(app.out_mono, !before, "optimistic flip");
        app.pump();
        assert_eq!(app.out_mono, !before, "device echo agrees");
    }

    #[test]
    fn output_phase_l_toggle_round_trips() {
        let mut app = App::new(Box::new(MockTransport::new()));
        app.pump();
        let before = app.out_phase_l;
        app.output_activate(OutRow::PhaseL.index());
        assert_eq!(app.out_phase_l, !before, "optimistic flip");
        app.pump(); // mock echoes bool value back
        assert_eq!(app.out_phase_l, !before, "device echo agrees");
    }

    /// The dashboard engages only when both thresholds are met; one short dimension falls back to tabs.
    #[test]
    fn dashboard_gates_on_both_dimensions() {
        assert!(
            dashboard_fits(DASHBOARD_MIN_WIDTH, DASHBOARD_MIN_HEIGHT),
            "exactly at the threshold fits"
        );
        assert!(dashboard_fits(200, 60), "comfortably large fits");
        assert!(
            !dashboard_fits(DASHBOARD_MIN_WIDTH - 1, DASHBOARD_MIN_HEIGHT),
            "too narrow falls back"
        );
        assert!(
            !dashboard_fits(DASHBOARD_MIN_WIDTH, DASHBOARD_MIN_HEIGHT - 1),
            "too short falls back"
        );
        assert!(!dashboard_fits(80, 24), "a standard terminal stays tabbed");
    }

    /// Render one frame to an off-screen buffer and flatten it to a searchable string.
    fn render_to_string(app: &App, w: u16, h: u16) -> String {
        use ratatui::{backend::TestBackend, Terminal};
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    /// On a large terminal every panel renders at once (body-only markers, since the tab bar always
    /// prints the screen names). The verbose footers don't fit the narrow columns, so they're
    /// omitted rather than clipped.
    #[test]
    fn dashboard_shows_all_panels_when_large() {
        let app = App::new(Box::new(MockTransport::new()));
        let screen = render_to_string(&app, 170, 54);
        for marker in [
            "table 1",
            "switches",
            "Headphone A",
            "Monitor mix",
            "Talkback",
        ] {
            assert!(
                screen.contains(marker),
                "dashboard should render the panel containing {marker:?}"
            );
        }
        // The verbose legends don't fit, but the short hints do — the link instruction survives.
        assert!(
            !screen.contains("host-owned coeffs"),
            "outputs verbose legend omitted in the narrow column"
        );
        assert!(
            !screen.contains("equal-power pan"),
            "mixer verbose legend omitted in the narrow column"
        );
        assert!(
            screen.contains("link pair"),
            "the short stereo-link hint survives in the Inputs column"
        );
    }

    /// Below the threshold it's the tabbed single-panel view: only the active screen's body renders.
    #[test]
    fn small_terminal_is_tabbed_single_panel() {
        let mut app = App::new(Box::new(MockTransport::new()));
        app.screen = Screen::Mixer;
        let screen = render_to_string(&app, 90, 30);
        assert!(
            screen.contains("Monitor mix"),
            "the active Mixer panel renders"
        );
        assert!(
            !screen.contains("Headphone A"),
            "the Outputs panel is hidden in tabbed mode"
        );
        assert!(
            !screen.contains("switches"),
            "the Inputs panel is hidden in tabbed mode"
        );
    }

    /// A footer that fits still shows — the omission is width-driven, not unconditional.
    #[test]
    fn wide_single_panel_keeps_its_footer() {
        let mut app = App::new(Box::new(MockTransport::new()));
        app.screen = Screen::Mixer;
        let screen = render_to_string(&app, 120, 40);
        assert!(
            screen.contains("equal-power pan"),
            "the mixer footer shows when the panel is wide"
        );
    }

    /// `c` on the Meters screen clears every channel's peak-hold and latched clip.
    #[test]
    fn meters_clear_key_resets_peaks() {
        let mut app = App::new(Box::new(MockTransport::new()));
        app.peaks[0].update(
            Sample {
                level: 0x4000,
                clip: true,
            },
            Duration::from_millis(1),
        );
        app.peaks[5].update(
            Sample {
                level: 0x10,
                clip: true,
            },
            Duration::from_millis(1),
        );
        assert_eq!(app.screen, Screen::Meters, "Meters is the default screen");
        app.on_key(KeyCode::Char('c'));
        assert_eq!(
            app.peaks[0],
            meters::PeakHold::default(),
            "peak + clip cleared"
        );
        assert!(!app.peaks[5].clipped, "latch cleared on every channel");
    }

    /// The dBFS ruler fills the bar width exactly and anchors −60 at the left, 0 at the right.
    #[test]
    fn meter_scale_spans_width_and_anchors_ends() {
        let s = meter_scale(60);
        assert_eq!(s.chars().count(), 60, "ruler exactly fills the bar width");
        assert!(s.starts_with("-60"), "−60 dB anchored at the left");
        assert!(s.ends_with('0'), "0 dB anchored at the right edge");
    }

    /// Gridlines drop from the ticks through the unlit part of a bar, and the lit fill covers them.
    #[test]
    fn meter_bar_gridlines_only_in_unlit_region() {
        let flat = |db, peak| -> String {
            meter_bar(60, db, false, peak)
                .iter()
                .map(|s| s.content.to_string())
                .collect()
        };
        // Silent bar: gridlines visible across the empty track.
        assert!(
            flat(f64::NEG_INFINITY, f64::NEG_INFINITY).contains('┊'),
            "unlit bar shows gridlines"
        );
        // Full-scale bar: the lit fill covers every gridline.
        assert!(
            !flat(0.0, f64::NEG_INFINITY).contains('┊'),
            "a full bar hides the gridlines"
        );
    }

    /// The level bar keeps its exact column width and overlays a single peak-hold tick.
    #[test]
    fn meter_bar_keeps_width_and_marks_peak() {
        let bar = meter_bar(20, -6.0, false, 0.0); // loud level, peak pinned at full scale
        let text: String = bar.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text.chars().count(), 20, "bar fills exactly its width");
        assert_eq!(text.matches('│').count(), 1, "exactly one peak marker");
    }

    /// An output selection (coefficient) cycles and reconciles via the mock echo.
    #[test]
    fn output_hp_mode_cycles_and_reconciles() {
        let mut app = App::new(Box::new(MockTransport::new()));
        app.pump();
        assert_eq!(app.hp_a_mode, 0);
        app.output_cycle(OutRow::HpAMode.index(), 1);
        assert_eq!(app.hp_a_mode, 1, "advanced one option");
        app.pump(); // mock echoes selection coeff update back
        assert_eq!(app.hp_a_mode, 1, "echo agrees");
        // Wrap-around downward from 0.
        app.hp_b_mode = 0;
        app.output_cycle(OutRow::HpBMode.index(), -1);
        assert_eq!(app.hp_b_mode as usize, HP_MODES.len() - 1, "wraps to last");
    }

    /// The line operating-level selection (index `LINE_OUT_IDX`) cycles and reconciles via the mock
    /// echo — the `apply_value` arm the HP-mode test doesn't reach.
    #[test]
    fn output_line_level_cycles_and_reconciles() {
        let mut app = App::new(Box::new(MockTransport::new()));
        app.pump();
        assert_eq!(app.line_level_sel, 0);
        app.output_cycle(OutRow::LineLevel.index(), 1);
        assert_eq!(app.line_level_sel, 1, "advanced one option");
        app.pump(); // mock echoes selection coeff update back
        assert_eq!(app.line_level_sel, 1, "echo agrees");
        // Wrap-around: two options, so advancing again returns to 0.
        app.output_cycle(OutRow::LineLevel.index(), 1);
        assert_eq!(app.line_level_sel, 0, "wraps within the two options");
    }

    /// The dim-level row (a dB level, not a selection) nudges + clamps with ←/→ and resets on Space —
    /// it must not wrap like the selection rows.
    #[test]
    fn output_dim_level_nudges_clamps_and_resets() {
        let mut app = App::new(Box::new(MockTransport::new()));
        app.pump();
        assert_eq!(app.dim_level_db, preset::default_dim_level_db());
        let dim = OutRow::DimLevel.index();
        app.output_cycle(dim, 1);
        assert_eq!(
            app.dim_level_db,
            preset::default_dim_level_db() + DIM_STEP_DB,
            "nudged up 1 dB"
        );
        // Clamp at the ceiling instead of wrapping.
        for _ in 0..100 {
            app.output_cycle(dim, 1);
        }
        assert_eq!(
            app.dim_level_db, DIM_MAX_DB,
            "clamps at the ceiling, no wrap"
        );
        // Clamp at the floor too.
        for _ in 0..100 {
            app.output_cycle(dim, -1);
        }
        assert_eq!(app.dim_level_db, DIM_MIN_DB, "clamps at the floor");
        // Space resets to the default cut.
        app.output_activate(dim);
        assert_eq!(
            app.dim_level_db,
            preset::default_dim_level_db(),
            "Space resets to default"
        );
    }

    /// The alt-speaker enable gates the live Alt switch + trim: both are inert until it's on, then
    /// the toggle flips and the trim nudges/clamps bipolar.
    #[test]
    fn alt_speaker_enable_gates_toggle_and_trim() {
        let mut app = App::new(Box::new(MockTransport::new()));
        app.pump();
        let (alt, trim) = (OutRow::Alt.index(), OutRow::AltTrim.index());
        assert!(!app.alt_enable, "alt feature off by default");
        assert!(
            OutRow::Alt.is_disabled(&app) && OutRow::AltTrim.is_disabled(&app),
            "alt rows disabled while the feature is off"
        );
        // While disabled, the toggle + trim do nothing.
        app.output_activate(alt);
        app.output_cycle(trim, 1);
        assert!(
            !app.out_alt && app.alt_trim_db == 0.0,
            "inert while disabled"
        );

        // Enable the feature, then both work.
        app.output_activate(OutRow::AltEnable.index());
        assert!(app.alt_enable && !OutRow::Alt.is_disabled(&app));
        app.output_activate(alt);
        assert!(app.out_alt, "Alt toggles once enabled");
        app.output_cycle(trim, -1);
        assert_eq!(app.alt_trim_db, -ALT_TRIM_STEP_DB, "trim nudges down");
        for _ in 0..100 {
            app.output_cycle(trim, -1);
        }
        assert_eq!(
            app.alt_trim_db, ALT_TRIM_MIN_DB,
            "trim clamps (bipolar floor)"
        );
    }

    /// Cut silences a source on the device but preserves its stored level (so un-cut restores it,
    /// even through the mock's coefficient echo); solo silences every other source.
    #[test]
    fn mixer_cut_and_solo_preserve_levels() {
        let mut app = App::new(Box::new(MockTransport::new()));
        app.pump();
        // Give Playback 1-2 (source 0) a known send, reconciled back through the mock.
        app.set_mix_cell(0, 0, -6.0);
        app.pump();
        let stored = app.matrix.db(0, 0);
        assert!((stored - -6.0).abs() < 0.2, "level set and reconciled");

        // Cut source 0: silenced, but its stored level must survive the echoed off-write.
        app.toggle_cut(0);
        assert!(app.cut[0] && app.source_muted(0), "source 0 is cut");
        app.pump();
        assert!(
            (app.matrix.db(0, 0) - stored).abs() < 0.2,
            "cut preserves the stored level"
        );
        // Un-cut restores it.
        app.toggle_cut(0);
        app.pump();
        assert!(!app.source_muted(0));
        assert!(
            (app.matrix.db(0, 0) - stored).abs() < 0.2,
            "un-cut keeps the level"
        );

        // Solo source 1: every other source is silenced, the soloed one is not.
        app.toggle_solo(1);
        assert!(app.source_muted(0), "non-soloed source is silenced");
        assert!(!app.source_muted(1), "soloed source stays audible");
        app.toggle_solo(1);
        assert!(
            !app.source_muted(0),
            "clearing the solo un-silences the rest"
        );
    }

    /// A stereo-linked pair cuts together — cutting one mirrors onto its partner.
    #[test]
    fn cut_mirrors_onto_linked_partner() {
        let mut app = App::new(Box::new(MockTransport::new()));
        app.pump();
        app.toggle_link(0); // link analogues 1-2
        let s = analogue_source(0);
        let p = app.linked_partner_source(s).expect("pair is linked");
        app.toggle_cut(s);
        assert!(
            app.cut[s] && app.cut[p],
            "cut mirrors onto the linked partner"
        );
    }

    /// The Mixer's talkback row edits each cue bus's level + pan independently (a mono mix cell),
    /// skips the Main column, and shares the crosspoint cell keys (gain / pan / off).
    #[test]
    fn talkback_row_edits_level_and_pan_per_bus() {
        let mut app = App::new(Box::new(MockTransport::new()));
        app.pump();
        app.screen = Screen::Mixer;
        // Walk the cursor to the talkback row, HP A column (DESTINATIONS index 2 → talk slot 1).
        for _ in 0..MIXER_TALK_ROW {
            app.on_key(KeyCode::Down);
        }
        app.on_key(KeyCode::Right);
        app.on_key(KeyCode::Right);
        assert_eq!(app.mix_sel, (MIXER_TALK_ROW, 2));
        assert_eq!(app.tb_db, [0.0; 3]);
        // Level nudge touches only HP A's slot.
        app.on_key(KeyCode::Char('-'));
        assert_eq!(app.tb_db[1], -1.0, "HP A talk nudged down 1 dB");
        assert_eq!(app.tb_db[0], 0.0, "Line 3-4 untouched");
        assert_eq!(app.tb_db[2], 0.0, "HP B untouched");
        // Pan is per-bus too.
        app.on_key(KeyCode::Char(']'));
        assert!(app.tb_pan[1] > 0.0, "HP A talk panned right");
        // x turns the send off; 0 restores unity.
        app.on_key(KeyCode::Char('x'));
        assert!(app.tb_db[1].is_infinite(), "x turns the send off");
        app.on_key(KeyCode::Char('0'));
        assert_eq!(app.tb_db[1], 0.0, "0 restores unity");
        // The Main column carries no talkback — editing it there is a no-op.
        app.on_key(KeyCode::Left);
        app.on_key(KeyCode::Left);
        assert_eq!(app.mix_sel, (MIXER_TALK_ROW, 0));
        let before = app.tb_db;
        app.on_key(KeyCode::Char('-'));
        assert_eq!(app.tb_db, before, "Main column has no talkback send");
    }

    /// `t` is a global press-to-toggle for the talkback mic, available on any screen.
    #[test]
    fn talk_key_toggles_talkback() {
        let mut app = App::new(Box::new(MockTransport::new()));
        app.pump();
        assert!(!app.talking, "talk starts closed");
        app.on_key(KeyCode::Char('t'));
        assert!(app.talking, "first press opens the mic");
        app.on_key(KeyCode::Char('t'));
        assert!(!app.talking, "second press closes it");
    }
}
