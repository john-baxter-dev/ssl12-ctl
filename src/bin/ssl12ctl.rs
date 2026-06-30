//! Minimal CLI demo for the SSL 12 control protocol.
//!
//! Examples:
//!   ssl12ctl info
//!   ssl12ctl phantom 0 on
//!   ssl12ctl hiz 0 on
//!   ssl12ctl xpoint 10 -10.0      # monitor-mix crosspoint index -> dB
//!   ssl12ctl mix playback_1_2 main -6.0   # route a source into a bus at a gain (dB)
//!   ssl12ctl level 0 -6.0         # OUTPUT_LEVEL index -> dB
//!   ssl12ctl loopback playback_1_2
//!   ssl12ctl hpmode 4 high_impedance
//!   ssl12ctl oplevel 0 plus24
//!   ssl12ctl mute on
//!   ssl12ctl meters              # live labelled meter display
//!   ssl12ctl listen              # dump device->host frames (state/meters)
//!   ssl12ctl --no-handshake listen   # diagnostic: open without the version handshake
//!
//! Nothing here touches firmware/flash — the library refuses those codes outright.

use std::process::exit;

use anyhow::{bail, Context};
use ssl12_ctl::controls::*;
use ssl12_ctl::Ssl12;

/// Shown on usage/`info`. Unofficial-project + no-liability notice; full text in README.
const DISCLAIMER: &str =
    "\nUnofficial project — not affiliated with or endorsed by Solid State Logic. \
Tested on real hardware and drives only volatile DSP state, but provided \"as is\" with no \
warranty; use at your own risk (see README Disclaimer).";

/// Full command reference. Shown on `help`/`-h`/`--help` and when run with no command.
fn print_help() {
    println!(
        "\
ssl12ctl — control client for the Solid State Logic SSL 12 (unofficial)

USAGE:
    ssl12ctl [--no-handshake] <command> [args]

CONNECTION
    info                       open the device, run the handshake, print versions + state
    meters                     live text meter bars (Ctrl-C to stop)
    listen                     print raw device->host frames (state echoes + meters)

INPUTS                         (idx = channel, 0-based)
    phantom <idx> <on|off>     48V phantom power on a mic input
    hpf <idx> <on|off>         high-pass filter
    line <idx> <on|off>        line / mic input select
    hiz <idx> <on|off>         Hi-Z / instrument input (inputs 1-2)
    polarity <idx> <on|off>    phase (polarity) invert

MONITOR MIX
    mix <source> <bus> <dB>    route a source into a bus  (e.g. mix playback_1_2 main -6)
    xpoint <index> <dB>        set one crosspoint cell directly (index 0-239)
    link <idx> <on|off>        stereo-link an input pair (idx 0 or 2)

OUTPUTS
    level <idx> <dB>           output bus level
    mute <on|off>              mute all hardware outputs
    loopback <src>             off | playback_1_2 | playback_3_4 | playback_5_6 |
                               playback_7_8 | bus_1_2
    oplevel <idx> <plus9|plus24>                              line output operating level (dBu)
    hpmode <idx> <standard|high_sensitivity|high_impedance>   headphone gain mode
    busmode <idx> <stereo|balanced_mono|unbalanced_mono>      bus output mode
    clock <internal|adat>      clock source

SETUP
    install-udev               install the udev rule for no-sudo access (self-elevates), then replug
    uninstall-udev             remove the udev rule

FLAGS
    --no-handshake             skip the version handshake (diagnostic; writes may be ignored)
    -h, --help                 show this help

The terminal UI is a separate binary, `ssl12tui` (run it, then press `?` in-app for keys).{DISCLAIMER}"
    );
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // Global flag: skip the version handshake (diagnostic — keeps read-only commands usable
    // even if the device never returns protocol version reply).
    let no_handshake = args.iter().any(|a| a == "--no-handshake");
    args.retain(|a| a != "--no-handshake");
    if args.is_empty() {
        print_help();
        exit(2);
    }
    if matches!(args[0].as_str(), "help" | "-h" | "--help") {
        print_help();
        exit(0);
    }

    // udev-rule management runs *before* any device open: it needs root, not hardware, and is
    // the very thing you run to make the no-sudo device open work in the first place.
    match args[0].as_str() {
        "install-udev" => finish(install_udev(&args)),
        "uninstall-udev" => finish(uninstall_udev(&args)),
        _ => {}
    }

    // This is the diagnostic CLI — always show the transport's bring-up output (it's suppressed by
    // default so it can't corrupt the TUI, but here it's the whole point).
    ssl12_ctl::device::set_verbose(true);

    let opened = if no_handshake {
        Ssl12::open_no_handshake()
    } else {
        Ssl12::open()
    };
    let mut dev = match opened {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error opening SSL 12: {e}");
            eprintln!("(check it's connected. For no-sudo access, run `ssl12ctl install-udev` once, then replug.)");
            if !no_handshake {
                eprintln!("(if this is a handshake error, retry with `--no-handshake` then `listen` to see if code 2 ever arrives)");
            }
            exit(1);
        }
    };
    if no_handshake {
        eprintln!("--no-handshake: skipped version exchange (ready={}). Writes may be ignored by the device until it's ready.", dev.ready);
    }

    if let Err(e) = run(&mut dev, &args) {
        eprintln!("error: {e:#}");
        exit(1);
    }
}

fn run(dev: &mut Ssl12, args: &[String]) -> anyhow::Result<()> {
    match args[0].as_str() {
        "info" => cmd_info(dev)?,
        "meters" => cmd_meters(dev)?,
        "golive" => cmd_golive(dev)?,
        "rawin" => cmd_rawin(dev)?,
        "probe" => cmd_probe(dev)?,
        "phantom" => dev.set_phantom(idx(args, 1)? as u8, onoff(args, 2)?)?,
        "hpf" => dev.set_hpf(idx(args, 1)? as u8, onoff(args, 2)?)?,
        "line" => dev.set_line_input(idx(args, 1)? as u8, onoff(args, 2)?)?,
        "hiz" => dev.set_instrument_hiz(idx(args, 1)? as u8, onoff(args, 2)?)?,
        "polarity" => dev.set_input_polarity(idx(args, 1)? as u8, onoff(args, 2)?)?,
        "xpoint" => dev.set_crosspoint_db(idx(args, 1)?, db(args, 2)?)?,
        "mix" => dev.set_mix(
            mix_source(arg(args, 1)?)?,
            mix_dest(arg(args, 2)?)?,
            db(args, 3)?,
        )?,
        "level" => dev.set_output_level_db(idx(args, 1)?, db(args, 2)?)?,
        "mute" => dev.mute_hardware_outputs(onoff(args, 1)?)?,
        "link" => dev.set_stereo_link(idx(args, 1)?, onoff(args, 2)?)?,
        "clock" => dev.set_clock_source(match arg(args, 1)? {
            "internal" => ClockSource::Internal,
            "adat" => ClockSource::Adat,
            x => bail!("invalid value: '{x}'"),
        })?,
        "loopback" => dev.set_loopback_source(match arg(args, 1)? {
            "off" => LoopbackSource::Off,
            "playback_1_2" => LoopbackSource::Playback1_2,
            "playback_3_4" => LoopbackSource::Playback3_4,
            "playback_5_6" => LoopbackSource::Playback5_6,
            "playback_7_8" => LoopbackSource::Playback7_8,
            "bus_1_2" => LoopbackSource::OutputBus1_2,
            x => bail!("invalid value: '{x}'"),
        })?,
        "oplevel" => dev.set_line_output_operating_level(
            idx(args, 1)?,
            match arg(args, 2)? {
                "plus9" => OutputOperatingLevel::Plus9dBu,
                "plus24" => OutputOperatingLevel::Plus24dBu,
                x => bail!("invalid value: '{x}'"),
            },
        )?,
        "hpmode" => dev.set_headphone_gain_mode(
            idx(args, 1)?,
            match arg(args, 2)? {
                "standard" => HeadphoneGainMode::Standard,
                "high_sensitivity" => HeadphoneGainMode::HighSensitivity,
                "high_impedance" => HeadphoneGainMode::HighImpedance,
                x => bail!("invalid value: '{x}'"),
            },
        )?,
        "busmode" => dev.set_bus_output_mode(
            idx(args, 1)?,
            match arg(args, 2)? {
                "stereo" => BusOutputMode::Stereo,
                "balanced_mono" => BusOutputMode::BalancedMono,
                "unbalanced_mono" => BusOutputMode::UnbalancedMono,
                x => bail!("invalid value: '{x}'"),
            },
        )?,
        "listen" => loop {
            for f in dev.read_frames()? {
                println!(
                    "rx code=0x{:02x} payload={:02x?} crc_ok={}{}",
                    f.code,
                    f.payload,
                    f.crc_ok,
                    note(f.code)
                );
            }
        },
        other => {
            eprintln!("unknown command: {other}");
            eprintln!("run `ssl12ctl --help` for the command list.");
            exit(2);
        }
    }
    Ok(())
}

// ---- multi-line command bodies (one-shot controls stay inline in `run`'s dispatch table) ----

/// `info` — print build/connection/version info and drain whatever's currently on the IN endpoint.
fn cmd_info(dev: &mut Ssl12) -> anyhow::Result<()> {
    println!("ssl12ctl build: {}", ssl12_ctl::device::TRANSPORT_BUILD);
    let (ep_in, ep_out) = dev.endpoints();
    println!(
        "connected. bulk IN=0x{ep_in:02x} OUT=0x{ep_out:02x}, ready={}",
        dev.ready
    );
    if let Some(v) = &dev.versions {
        print!("versions: protocol={}", v.protocol_version);
        match v.dsp_version {
            Some(d) => print!(", dsp={d}"),
            None => print!(", dsp=?"),
        }
        match v.hw_version {
            Some(hw) => print!(", hw={hw}"),
            None => print!(", hw=?"),
        }
        match v.sw_version {
            Some(sw) => println!(", sw=0x{sw:08x}"),
            None => println!(", sw=?"),
        }
    }
    for f in dev.read_frames()? {
        println!(
            "  rx code=0x{:02x} payload={:02x?} crc_ok={}{}",
            f.code,
            f.payload,
            f.crc_ok,
            note(f.code)
        );
    }
    eprintln!("{DISCLAIMER}");
    Ok(())
}

/// `meters` — live labelled meter bars, refreshed sparingly from the IN stream (Ctrl-C to stop).
fn cmd_meters(dev: &mut Ssl12) -> anyhow::Result<()> {
    eprintln!("ssl12ctl build: {}", ssl12_ctl::device::TRANSPORT_BUILD);
    eprintln!("listening for meters on the IN endpoint (Ctrl-C to stop)…");
    let mut tick = 0u32;
    loop {
        for f in dev.read_frames()? {
            if f.code != ssl12_ctl::protocol::USB_RECV_DSP {
                continue;
            }
            if let Some(update) = ssl12_ctl::meters::parse(&f.payload) {
                // The device pushes ~hundreds of frames/sec; refresh sparingly.
                tick += 1;
                if tick.is_multiple_of(12) {
                    print_meters(&update);
                }
            }
        }
    }
}

/// `golive` — replicate SSL360's post-handshake bring-up, then raw-dump IN for ~8 s to see whether
/// the device starts streaming. `USB_REQUEST_CONTROL_STATES` (0x2B) isn't implemented on the SSL 12
/// (it replies `PRINT_ERROR 0x194` = `ERROR_CODE_INVALID_USB_COMMAND`), so we provoke device→host
/// VALUEs the supported way instead — a few `value request`s.
fn cmd_golive(dev: &mut Ssl12) -> anyhow::Result<()> {
    use ssl12_ctl::protocol::{DspCode, USB_RECV_DSP};
    eprintln!("ssl12ctl build: {}", ssl12_ctl::device::TRANSPORT_BUILD);
    eprintln!("golive: DISABLE_MIXER=false + a few value requests, then raw IN for ~8 s…");
    dev.send_dsp(&ssl12_ctl::protocol::dsp_bool(
        DspCode::CoefficientUpdateBool.num(),
        ssl12_ctl::controls::Coeff::DisableMixer.num(),
        0,
        false,
    ))?;
    for i in 0..4u16 {
        let _ = dev.request_param_value(ssl12_ctl::controls::Param::InputPhantomPower.num(), i);
    }
    let start = std::time::Instant::now();
    let mut last_hb = std::time::Instant::now();
    let (mut total, mut xfers, mut empties, mut meters) = (0usize, 0u64, 0u64, 0u64);
    while start.elapsed() < std::time::Duration::from_secs(8) {
        if last_hb.elapsed() >= std::time::Duration::from_millis(500) {
            let _ = dev.heartbeat();
            last_hb = std::time::Instant::now();
        }
        let bytes = dev.read_raw(250)?;
        if bytes.is_empty() {
            empties += 1;
            continue;
        }
        xfers += 1;
        total += bytes.len();
        // count parseable meter frames in this transfer
        let mut data = &bytes[..];
        while let Some((f, used)) = ssl12_ctl::protocol::parse_frame(data) {
            if f.code == USB_RECV_DSP && ssl12_ctl::meters::parse(&f.payload).is_some() {
                meters += 1;
            }
            data = &data[used..];
        }
        if xfers <= 50 {
            print_transfer_hex(&bytes);
        }
    }
    println!("golive summary: {total} bytes, {xfers} non-empty transfers ({meters} meter frames), {empties} empty reads over 8 s");
    Ok(())
}

/// `rawin` — dump exactly what the IN endpoint delivers, unparsed, for ~8 s. With `--no-handshake`
/// nothing is sent first, so the device's unsolicited behaviour is visible. The keepalive still runs
/// (a benign 0x1b counter, not a control write) so the device doesn't re-enumerate mid-dump.
fn cmd_rawin(dev: &mut Ssl12) -> anyhow::Result<()> {
    eprintln!("ssl12ctl build: {}", ssl12_ctl::device::TRANSPORT_BUILD);
    eprintln!("raw IN dump for ~8 s (each non-empty transfer shown as hex; Ctrl-C to stop)…");
    let start = std::time::Instant::now();
    let mut last_hb = std::time::Instant::now();
    let (mut total, mut xfers, mut empties) = (0usize, 0u64, 0u64);
    while start.elapsed() < std::time::Duration::from_secs(8) {
        if last_hb.elapsed() >= std::time::Duration::from_millis(500) {
            let _ = dev.heartbeat();
            last_hb = std::time::Instant::now();
        }
        let bytes = dev.read_raw(250)?;
        if bytes.is_empty() {
            empties += 1;
            continue;
        }
        xfers += 1;
        total += bytes.len();
        if xfers <= 50 {
            print_transfer_hex(&bytes);
        }
    }
    println!("raw IN summary: {total} bytes in {xfers} non-empty transfers, {empties} empty reads (250ms each) over 8 s");
    Ok(())
}

/// `probe` — after the handshake, pull control states + a few param values and print everything the
/// device returns for ~6 s. Tests whether a state request kicks the meter stream / clears
/// `USB_RECONNECT_REQUIRED`.
fn cmd_probe(dev: &mut Ssl12) -> anyhow::Result<()> {
    eprintln!("ssl12ctl build: {}", ssl12_ctl::device::TRANSPORT_BUILD);
    eprintln!("probe: requesting control states + a few param values, watching IN…");
    dev.request_control_states()?;
    // Request a handful of known parameter values (phantom 0..3) to provoke VALUE replies.
    for i in 0..4u16 {
        let _ = dev.request_param_value(ssl12_ctl::controls::Param::InputPhantomPower.num(), i);
    }
    let mut meter_frames = 0u32;
    let mut other_frames = 0u32;
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(6) {
        for f in dev.read_frames()? {
            let is_meter = f.code == ssl12_ctl::protocol::USB_RECV_DSP
                && ssl12_ctl::meters::parse(&f.payload).is_some();
            if is_meter {
                meter_frames += 1;
                if meter_frames <= 3 {
                    println!(
                        "  METER frame #{meter_frames}: code=0x{:02x} len={}",
                        f.code,
                        f.payload.len()
                    );
                }
            } else {
                other_frames += 1;
                println!(
                    "  rx code=0x{:02x} payload={:02x?} crc_ok={}{}",
                    f.code,
                    f.payload,
                    f.crc_ok,
                    note(f.code)
                );
            }
        }
    }
    println!("probe done: {meter_frames} meter frames, {other_frames} other frames in 6 s");
    Ok(())
}

/// Print one raw bulk transfer as truncated hex (first 80 bytes) — the shared dump line used by the
/// `golive` and `rawin` diagnostics.
fn print_transfer_hex(bytes: &[u8]) {
    let hex: String = bytes.iter().take(80).map(|b| format!("{b:02x} ")).collect();
    let more = if bytes.len() > 80 { " …" } else { "" };
    println!("[{:>4}B] {}{more}", bytes.len(), hex.trim_end());
}

/// Render a meter update as a refreshing labelled bar display (40 cols = -60..0 dBFS).
fn print_meters(update: &ssl12_ctl::meters::MeterUpdate) {
    const WIDTH: usize = 40;
    const FLOOR_DB: f64 = -60.0;
    let mut out = String::from("\x1b[H\x1b[2J"); // cursor home + clear
    out.push_str("SSL 12 meters (table 1)   level dBFS\n\n");
    for (idx, label, s) in update.labelled() {
        let db = s.dbfs();
        let filled = if db.is_finite() {
            (((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0) * WIDTH as f64).round() as usize
        } else {
            0
        };
        let bar: String = "#".repeat(filled) + &"·".repeat(WIDTH - filled);
        let db_str = if db.is_finite() {
            format!("{db:6.1}")
        } else {
            "  -inf".to_string()
        };
        let clip = if s.clip { " CLIP" } else { "" };
        out.push_str(&format!("{idx:>2} {label:<11} {bar} {db_str}{clip}\n"));
    }
    print!("{out}");
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

// ---- udev rule install/uninstall ----

/// Where the rule is installed, and its text — the committed packaging file is the single source
/// of truth (embedded at compile time so the binary is self-sufficient for `cargo install` users).
const UDEV_RULE_PATH: &str = "/etc/udev/rules.d/70-ssl12.rules";
const UDEV_RULE: &str = include_str!("../../packaging/70-ssl12.rules");

/// Map a subcommand's outcome to a process exit: print the full context chain and exit 0/1. The
/// only place an integer code appears — it's the OS's process-exit ABI, not a value convention.
fn finish(result: anyhow::Result<()>) -> ! {
    match result {
        Ok(()) => exit(0),
        Err(e) => {
            eprintln!("error: {e:#}");
            exit(1)
        }
    }
}

/// `ssl12ctl install-udev` — drop the udev rule so the device opens without sudo, then reload.
/// Writing /etc needs root; if we aren't root, re-exec the same subcommand under sudo.
fn install_udev(args: &[String]) -> anyhow::Result<()> {
    match std::fs::write(UDEV_RULE_PATH, UDEV_RULE) {
        Ok(()) => {
            eprintln!("wrote {UDEV_RULE_PATH}");
            reload_udev();
            eprintln!("done. Unplug and replug the SSL 12 for the new permissions to take effect.");
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            elevate(args, "write the udev rule")
        }
        Err(e) => Err(e).with_context(|| format!("writing {UDEV_RULE_PATH}")),
    }
}

/// `ssl12ctl uninstall-udev` — remove the rule and reload. Same root handling as install.
fn uninstall_udev(args: &[String]) -> anyhow::Result<()> {
    match std::fs::remove_file(UDEV_RULE_PATH) {
        Ok(()) => {
            eprintln!("removed {UDEV_RULE_PATH}");
            reload_udev();
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("nothing to do: {UDEV_RULE_PATH} is not installed");
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            elevate(args, "remove the udev rule")
        }
        Err(e) => Err(e).with_context(|| format!("removing {UDEV_RULE_PATH}")),
    }
}

/// Reload udev so the rule applies without a reboot (best-effort; warns rather than fails).
fn reload_udev() {
    for argv in [
        ["control", "--reload-rules"].as_slice(),
        ["trigger"].as_slice(),
    ] {
        match std::process::Command::new("udevadm").args(argv).status() {
            Ok(s) if s.success() => {}
            Ok(s) => eprintln!("warning: `udevadm {}` exited with {s}", argv.join(" ")),
            Err(e) => eprintln!(
                "warning: couldn't run `udevadm {}`: {e} (reload manually)",
                argv.join(" ")
            ),
        }
    }
}

/// We hit a permission error touching /etc while trying to `action` (e.g. "write the udev rule").
/// Unless this is already the sudo retry (guarded by `--from-sudo` so we can never loop), re-exec
/// the same subcommand under sudo; the child does the real work and its success becomes ours.
fn elevate(args: &[String], action: &str) -> anyhow::Result<()> {
    const GUARD: &str = "--from-sudo";
    if args.iter().any(|a| a == GUARD) {
        bail!("permission denied trying to {action}, even under sudo — run as root manually");
    }
    let exe = std::env::current_exe().context("locating my own executable to re-run under sudo")?;
    eprintln!("need root to {action}; re-running under sudo…");
    let status = std::process::Command::new("sudo")
        .arg(&exe)
        .arg(&args[0])
        .arg(GUARD)
        .status()
        .with_context(|| format!("invoking sudo (try `sudo ssl12ctl {}` manually)", args[0]))?;
    if !status.success() {
        bail!("`sudo ssl12ctl {}` exited with {status}", args[0]);
    }
    Ok(())
}

/// Friendly note for notable device→host USB message codes.
fn note(code: u8) -> &'static str {
    match code {
        0x06 => "  <- USB_RECONNECT_REQUIRED (tile init incomplete?)",
        0x6c => "  (DSP: VALUE / meter)",
        _ => "",
    }
}

// ---- mixer source/destination name parsing ----
fn mix_source(s: &str) -> anyhow::Result<ssl12_ctl::mixer::Source> {
    use ssl12_ctl::mixer::*;
    Ok(match s {
        "playback_1_2" => PLAYBACK_1_2,
        "playback_3_4" => PLAYBACK_3_4,
        "playback_5_6" => PLAYBACK_5_6,
        "playback_7_8" => PLAYBACK_7_8,
        "analogue_1" | "analog_1" => ANALOGUE_1,
        "analogue_2" | "analog_2" => ANALOGUE_2,
        "analogue_3" | "analog_3" => ANALOGUE_3,
        "analogue_4" | "analog_4" => ANALOGUE_4,
        x => bail!("unknown mix source '{x}' (try playback_1_2 / analogue_1 …)"),
    })
}
fn mix_dest(s: &str) -> anyhow::Result<ssl12_ctl::mixer::Destination> {
    use ssl12_ctl::mixer::*;
    Ok(match s {
        "main" | "monitor" => MAIN,
        "line_3_4" => LINE_3_4,
        "hp_a" => HP_A,
        "hp_b" => HP_B,
        x => bail!("unknown mix dest '{x}' (try main / line_3_4 / hp_a / hp_b)"),
    })
}

// ---- tiny arg helpers ----
fn arg(a: &[String], i: usize) -> anyhow::Result<&str> {
    a.get(i).map(|s| s.as_str()).context("missing argument")
}
fn idx(a: &[String], i: usize) -> anyhow::Result<u16> {
    let s = arg(a, i)?;
    s.parse()
        .with_context(|| format!("expected an integer index, got '{s}'"))
}
fn db(a: &[String], i: usize) -> anyhow::Result<f64> {
    let s = arg(a, i)?;
    s.parse()
        .with_context(|| format!("expected a dB float, got '{s}'"))
}
fn onoff(a: &[String], i: usize) -> anyhow::Result<bool> {
    match arg(a, i)? {
        "on" | "true" | "1" => Ok(true),
        "off" | "false" | "0" => Ok(false),
        x => bail!("expected on/off, got '{x}'"),
    }
}
