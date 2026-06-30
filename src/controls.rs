//! SSL 12 control map: the device's DSP parameter and coefficient number-spaces, plus the
//! selection-value enums, as exercised over the USB vendor protocol.
//!
//! `Number` identifies the control; `Index` selects the channel/instance.

/// DSP parameter item numbers (use `param` DSP message codes).
///
/// The discriminant is the wire `Number`; the message's `Index` selects the
/// channel/instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum Param {
    InputPhantomPower = 1,       // bool, index 0..=3 (mic inputs)
    InputHpf = 2,                // bool, 0..=3
    InputLineInput = 3,          // bool, 0..=3 (line vs mic)
    OutputBusPhaseL = 4,         // bool, 0
    OutputBusMono = 5,           // bool, 0
    OutputBusDim = 6,            // bool, 0
    OutputBusCut = 7,            // bool, 0 (monitor cut)
    OutputBusAlt = 8,            // bool, 0
    OutputBusTalkbackEnable = 9, // bool, 0
    OutputLevel = 10,            // Q6.25, index 0,4,6
    InputInstrumentInput = 11,   // bool, 0..=1 (Hi-Z on in 1/2)
    SampleRate = 12,             // int (normally driven by UAC2 clock; readback)
    ClockSelection = 13,         // selection (ClockSource)
    ClockValid = 14,             // bool (read)
    InputPolarity = 15,          // bool, 0..=11 (phase invert)
}

impl Param {
    /// The DSP parameter item number carried on the wire.
    pub const fn num(self) -> u16 {
        self as u16
    }

    /// Human-readable name for the parameter.
    pub const fn name(self) -> &'static str {
        match self {
            Param::InputPhantomPower => "INPUT_PHANTOM_POWER",
            Param::InputHpf => "INPUT_HPF",
            Param::InputLineInput => "INPUT_LINE_INPUT",
            Param::OutputBusPhaseL => "OUTPUT_BUS_PHASE_L",
            Param::OutputBusMono => "OUTPUT_BUS_MONO",
            Param::OutputBusDim => "OUTPUT_BUS_DIM",
            Param::OutputBusCut => "OUTPUT_BUS_CUT",
            Param::OutputBusAlt => "OUTPUT_BUS_ALT",
            Param::OutputBusTalkbackEnable => "OUTPUT_BUS_TALKBACK_ENABLE",
            Param::OutputLevel => "OUTPUT_LEVEL",
            Param::InputInstrumentInput => "INPUT_INSTRUMENT_INPUT",
            Param::SampleRate => "SAMPLE_RATE",
            Param::ClockSelection => "CLOCK_SELECTION",
            Param::ClockValid => "CLOCK_VALID",
            Param::InputPolarity => "INPUT_POLARITY",
        }
    }
}

impl TryFrom<u16> for Param {
    /// The unrecognized number, on failure.
    type Error = u16;

    fn try_from(number: u16) -> Result<Self, Self::Error> {
        Ok(match number {
            1 => Param::InputPhantomPower,
            2 => Param::InputHpf,
            3 => Param::InputLineInput,
            4 => Param::OutputBusPhaseL,
            5 => Param::OutputBusMono,
            6 => Param::OutputBusDim,
            7 => Param::OutputBusCut,
            8 => Param::OutputBusAlt,
            9 => Param::OutputBusTalkbackEnable,
            10 => Param::OutputLevel,
            11 => Param::InputInstrumentInput,
            12 => Param::SampleRate,
            13 => Param::ClockSelection,
            14 => Param::ClockValid,
            15 => Param::InputPolarity,
            other => return Err(other),
        })
    }
}

/// DSP coefficient item numbers (use `COEFFICIENT_*` DSP message codes).
///
/// The discriminant is the wire `Number`; the message's `Index` selects the
/// channel/instance. Discriminants are non-contiguous — match the device's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum Coeff {
    MixerCrosspointTable = 1,  // Q6.25, index 0..=239 (monitor-mix matrix)
    OutputBusMono = 2,         // bool, idx 2,4,6
    OutputBusDimLevel = 3,     // Q6.25, idx 0
    OutputBusesCut = 4,        // bool
    OutputBusAltSpkEnable = 5, // bool
    OutputBusAltTrimLevel = 6, // Q6.25
    OutputBusesFollow1_2 = 7,  // bool
    TalkbackLevel = 8,         // Q6.25, idx 2..=7
    OutputBusLevel = 9,        // Q6.25, idx 0,2,3,4,6
    BusOutputMode = 10,        // selection (BusOutputMode), idx 4,6
    LoopbackSource = 11,       // selection (LoopbackSource), idx 0
    UserButtonFunction = 12,   // selection, idx 0..=2
    StereoLinkChannels = 13,   // bool, idx 0,2
    DisableMixer = 14,         // bool  (!! kills monitoring; recoverable)
    // 15, 17, 18, 19 = device test / power-rail / protection modes — intentionally unmapped
    // (brick/damage risk; the client never sends them).
    HeadphonesGainMode = 16,       // selection (HeadphoneGainMode), idx 4,6
    LineOutputOperatingLevel = 26, // selection (OutputOperatingLevel), idx 0,2
    OutputMonoLevel = 28,          // Q6.25
    MuteHardwareOutputs = 31,      // bool
    MonitorLevelControl = 32,      // bool, idx 0..=3
}

impl Coeff {
    /// The DSP coefficient item number carried on the wire.
    pub const fn num(self) -> u16 {
        self as u16
    }

    /// Human-readable name for the coefficient.
    pub const fn name(self) -> &'static str {
        match self {
            Coeff::MixerCrosspointTable => "MIXER_CROSSPOINT_TABLE",
            Coeff::OutputBusMono => "OUTPUT_BUS_MONO",
            Coeff::OutputBusDimLevel => "OUTPUT_BUS_DIM_LEVEL",
            Coeff::OutputBusesCut => "OUTPUT_BUSES_CUT",
            Coeff::OutputBusAltSpkEnable => "OUTPUT_BUS_ALT_SPK_ENABLE",
            Coeff::OutputBusAltTrimLevel => "OUTPUT_BUS_ALT_TRIM_LEVEL",
            Coeff::OutputBusesFollow1_2 => "OUTPUT_BUSES_FOLLOW_1_2",
            Coeff::TalkbackLevel => "TALKBACK_LEVEL",
            Coeff::OutputBusLevel => "OUTPUT_BUS_LEVEL",
            Coeff::BusOutputMode => "BUS_OUTPUT_MODE",
            Coeff::LoopbackSource => "LOOPBACK_SOURCE",
            Coeff::UserButtonFunction => "USER_BUTTON_FUNCTION",
            Coeff::StereoLinkChannels => "STEREO_LINK_CHANNELS",
            Coeff::DisableMixer => "DISABLE_MIXER",
            Coeff::HeadphonesGainMode => "HEADPHONES_GAIN_MODE",
            Coeff::LineOutputOperatingLevel => "LINE_OUTPUT_OPERATING_LEVEL",
            Coeff::OutputMonoLevel => "OUTPUT_MONO_LEVEL",
            Coeff::MuteHardwareOutputs => "MUTE_HARDWARE_OUTPUTS",
            Coeff::MonitorLevelControl => "MONITOR_LEVEL_CONTROL",
        }
    }
}

impl TryFrom<u16> for Coeff {
    /// The unrecognized number, on failure.
    type Error = u16;

    fn try_from(number: u16) -> Result<Self, Self::Error> {
        Ok(match number {
            1 => Coeff::MixerCrosspointTable,
            2 => Coeff::OutputBusMono,
            3 => Coeff::OutputBusDimLevel,
            4 => Coeff::OutputBusesCut,
            5 => Coeff::OutputBusAltSpkEnable,
            6 => Coeff::OutputBusAltTrimLevel,
            7 => Coeff::OutputBusesFollow1_2,
            8 => Coeff::TalkbackLevel,
            9 => Coeff::OutputBusLevel,
            10 => Coeff::BusOutputMode,
            11 => Coeff::LoopbackSource,
            12 => Coeff::UserButtonFunction,
            13 => Coeff::StereoLinkChannels,
            14 => Coeff::DisableMixer,
            // 15, 17, 18, 19 intentionally unmapped (test / power / protection modes).
            16 => Coeff::HeadphonesGainMode,
            26 => Coeff::LineOutputOperatingLevel,
            28 => Coeff::OutputMonoLevel,
            31 => Coeff::MuteHardwareOutputs,
            32 => Coeff::MonitorLevelControl,
            other => return Err(other),
        })
    }
}

/// CLOCK_SELECTION
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum ClockSource {
    Internal = 0,
    Adat = 1,
}

/// LOOPBACK_SOURCE
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum LoopbackSource {
    Off = 0,
    Playback1_2 = 1,
    Playback3_4 = 2,
    Playback5_6 = 3,
    Playback7_8 = 4,
    OutputBus1_2 = 5,
    OutputBus3_4 = 6,
    OutputBus5_6 = 7,
    OutputBus7_8 = 8,
}

/// LINE_OUTPUT_OPERATING_LEVEL
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum OutputOperatingLevel {
    Plus9dBu = 0,
    Plus24dBu = 1,
}

/// HEADPHONES_GAIN_MODE — the "headphone impedance" setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum HeadphoneGainMode {
    Standard = 0,
    HighSensitivity = 1,
    HighImpedance = 2,
}

/// BUS_OUTPUT_MODE
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum BusOutputMode {
    Stereo = 0,
    BalancedMono = 1,
    UnbalancedMono = 2,
}

/// Human-readable name for a DSP parameter item number.
pub fn param_name(number: u16) -> &'static str {
    match number {
        0 => "UNKNOWN",
        n => Param::try_from(n).map(Param::name).unwrap_or("(param?)"),
    }
}

/// Human-readable name for a DSP coefficient item number.
pub fn coeff_name(number: u16) -> &'static str {
    match number {
        0 => "UNKNOWN",
        n => Coeff::try_from(n).map(Coeff::name).unwrap_or("(coeff?)"),
    }
}

/// Device constants
pub const DSP_MAJOR_VERSION: i32 = 1;
pub const PROTOCOL_VERSION: i32 = 1;
pub const METER_TABLE_NUMBER: i32 = 1;
pub const NUM_METERS: usize = 29;
