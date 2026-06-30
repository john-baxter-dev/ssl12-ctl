//! `ssl12tui` — a terminal meter + control surface for the SSL 12.
//!
//! Backends (see `ssl12_ctl::transport`):
//!   * live USB device  — default when built with the `usb` feature and one is plugged in
//!   * mock             — synthetic meters + echoed control state, no hardware
//!
//! Run on the road (macOS/Linux, no device, no libusb):
//!   cargo run --no-default-features --features tui --bin ssl12tui
//! Force the mock even when built with `usb`:
//!   cargo run --bin ssl12tui -- --mock
//! Show the transport's bring-up diagnostics (FTDI-open report, keepalive warnings):
//!   cargo run --bin ssl12tui -- --verbose
//!
//! Tab cycles screens. Meters: display only (c clears peak/clip holds). Inputs: ↑/↓/←/→ select,
//! Space toggle, p stereo-link the selected analogue pair (1-2 / 3-4). Mixer: ↑/↓/←/→ select cell,
//! +/- gain (or scroll wheel), 0 unity, [ ] pan, \ center, x/Backspace off, s save. Global: m mute
//! outputs, q quit.

use std::time::Duration;

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, MouseEventKind,
};
use crossterm::execute;
use ratatui::DefaultTerminal;

use ssl12_ctl::transport::{MockTransport, Transport};

mod app;
mod view;

use app::App;
use view::draw;

fn main() -> std::io::Result<()> {
    if std::env::args().any(|a| a == "-h" || a == "--help") {
        print!(
            r#"ssl12tui — terminal meter + control surface for the SSL 12 (unofficial)

USAGE:
    ssl12tui [--mock] [--verbose]

FLAGS:
    --mock       force the mock backend (synthetic meters), even with a device plugged in
    --verbose    show transport bring-up diagnostics (FTDI-open, keepalive)
    -h, --help   show this help

KEYS:
    Tab          cycle screens (Meters / Inputs / Outputs / Mixer)
    ?            in-app help overlay (full key list)    m  mute outputs    q  quit
    Inputs       arrows select · Space toggle · p stereo-link the analogue pair
    Mixer        arrows select · +/- gain (or scroll) · 0 unity · [ ] pan · \ center · x off · s save
    Meters       display only · c clears peak/clip holds

With no device (or --mock) it runs against a built-in mock — no hardware needed.
Unofficial; not affiliated with or endorsed by Solid State Logic. No warranty (see README).
"#
        );
        return Ok(());
    }
    let force_mock = std::env::args().any(|a| a == "--mock");
    // Opt into the transport's bring-up diagnostics. Off by default so stray stderr never corrupts
    // the TUI canvas; must be set before the device opens (FTDI-open reports during `Ssl12::open`).
    #[cfg(feature = "usb")]
    if std::env::args().any(|a| a == "--verbose") {
        ssl12_ctl::device::set_verbose(true);
    }
    let transport = open_transport(force_mock);

    let mut terminal = ratatui::init();
    // Capture the mouse so the scroll wheel reaches us as `Event::Mouse` (otherwise the terminal
    // turns it into its own scrollback / arrow keys). Best-effort: a terminal without mouse support
    // just won't send the events.
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    let mut app = App::new(transport);
    // Now that the transport is open, run the connect-time sequence (load preset, push host-owned
    // state, hydrate device params). Kept out of `App::new` so construction has no side effects.
    app.connect();
    let result = run(&mut terminal, app);
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

/// Pick a backend: live device if available and not forced to mock, else the mock.
fn open_transport(force_mock: bool) -> Box<dyn Transport> {
    if !force_mock {
        #[cfg(feature = "usb")]
        {
            match ssl12_ctl::device::Ssl12::open() {
                Ok(dev) => return Box::new(dev),
                // The status bar already shows the MOCK backend; only explain the fallback reason
                // when asked, so a default run leaves the pre-TUI terminal clean.
                Err(e) if std::env::args().any(|a| a == "--verbose") => {
                    eprintln!("live device unavailable ({e}); falling back to mock")
                }
                Err(_) => {}
            }
        }
    }
    Box::new(MockTransport::new())
}

fn run(terminal: &mut DefaultTerminal, mut app: App) -> std::io::Result<()> {
    while !app.should_quit {
        app.pump();
        terminal.draw(|frame| draw(frame, &app))?;
        // ~60 Hz UI tick; non-blocking key poll so meters keep flowing.
        if event::poll(Duration::from_millis(16))? {
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => app.on_key(k.code),
                Event::Mouse(m) => match m.kind {
                    MouseEventKind::ScrollUp => app.on_scroll(true),
                    MouseEventKind::ScrollDown => app.on_scroll(false),
                    _ => {}
                },
                _ => {}
            }
        }
    }
    Ok(())
}
