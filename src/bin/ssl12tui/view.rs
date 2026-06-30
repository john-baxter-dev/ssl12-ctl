//! Rendering for ssl12tui: the tabbed/dashboard layout and per-screen panels. Reads `App` state
//! (see `app`) and draws with ratatui; holds no state of its own.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use ssl12_ctl::meters::{self, NUM_METERS};
use ssl12_ctl::mixer;

use crate::app::{
    onoff, talkback_slot, App, OutRow, Screen, INPUT_CONTROLS, MIXER_TALK_ROW, NUM_INPUTS,
    NUM_OUT_ROWS,
};

/// Minimum body size to switch from the tabbed single-screen view to the all-panels "dashboard":
/// meters across the top, Inputs/Outputs/Mixer in a row below. Width fits the three bottom panels
/// side by side; height fits the dBFS ruler + all 29 meter rows (32 incl. border) above the 18-row
/// bottom strip (the Outputs pane's 13 rows + blank + 2 footer lines, inside its border, set that
/// floor; the Mixer's header + 8 sources + talkback strip + footer also fits). Below either threshold
/// we fall back to tabs. Re-evaluated every frame, so resizing is automatic.
pub(crate) const DASHBOARD_MIN_WIDTH: u16 = 140;
pub(crate) const DASHBOARD_MIN_HEIGHT: u16 = 50;
/// Height of the dashboard's bottom control strip (the Inputs/Outputs/Mixer row).
const BOTTOM_STRIP_HEIGHT: u16 = 18;

/// Whether the body area clears both dashboard thresholds (else fall back to tabs).
pub(crate) fn dashboard_fits(width: u16, height: u16) -> bool {
    width >= DASHBOARD_MIN_WIDTH && height >= DASHBOARD_MIN_HEIGHT
}

pub(crate) fn draw(frame: &mut Frame, app: &App) {
    let [title, body, help] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    let ready = if app.transport.is_ready() {
        "ready"
    } else {
        "not ready"
    };
    // `●MUTED` / `●TALK` are safety state, so they sit right after the tabs as bright badges — not
    // appended to the status tail, where a narrow terminal would clip them off the right edge.
    let badge = |label: &str, color: Color| {
        Span::styled(
            format!(" {label} "),
            Style::default()
                .fg(Color::Black)
                .bg(color)
                .add_modifier(Modifier::BOLD),
        )
    };
    let tab = |name: &'static str, on: bool| {
        let style = if on {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        Span::styled(format!(" {name} "), style)
    };
    let mut spans = vec![
        Span::styled(
            " SSL 12 ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        tab("Meters", app.screen == Screen::Meters),
        tab("Inputs", app.screen == Screen::Inputs),
        tab("Outputs", app.screen == Screen::Outputs),
        tab("Mixer", app.screen == Screen::Mixer),
        Span::raw(" "),
    ];
    if app.muted {
        spans.push(badge("●MUTED", Color::Red));
    }
    if app.talking {
        spans.push(badge("●TALK", Color::LightRed));
    }
    // Backend tag: MOCK is a loud warning badge (you should never mistake it for hardware); the
    // live device is a quiet "USB · ready" tag. `app.backend_label` is the transport's `label()`.
    if app.backend_label == "MOCK" {
        spans.push(badge("MOCK", Color::Yellow));
        spans.push(Span::raw(format!("  {:.0} fps", app.fps)));
    } else {
        spans.push(Span::raw(format!(
            "  {} · {ready} · {:.0} fps",
            app.backend_label, app.fps
        )));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), title);

    let help_text = match app.screen {
        Screen::Meters => " Tab screen · c clear peaks/clips · m mute · t talk · ? help · q quit ",
        Screen::Inputs => " Tab screen · ↑/↓/←/→ select · Space toggle · p link pair · ? help · q quit ",
        Screen::Outputs => " Tab screen · ↑/↓ row · Space toggle · ←/→ change · s save · ? help · q quit ",
        Screen::Mixer => " Tab screen · ↑/↓/←/→ cell · +/- gain · [ ] pan · x off · c cut · o solo · s save · ? help · q quit ",
    };

    // Dashboard: on a large enough terminal, show every panel at once — meters across the top, the
    // three control panels in a row below — with `app.screen` acting as the focused panel (the
    // others render dimmed). Otherwise fall back to the tabbed single-screen view. The focused panel
    // still receives all key input exactly as in tabbed mode, so nothing else has to change.
    if dashboard_fits(body.width, body.height) {
        let [meters_area, bottom] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(BOTTOM_STRIP_HEIGHT)])
                .areas(body);
        // Inputs and Outputs take their natural widths; the wider Mixer gets the rest. Both side
        // panels are kept tight (Inputs: label 12 + 5×7; Outputs: label 17 + value) so that at
        // DASHBOARD_MIN_WIDTH the Mixer still clears its 57-column content (label 13 + 4 bus
        // columns × 11) and HP B isn't clipped: 49 + 32 + 59 = 140.
        let [ins, outs, mix] = Layout::horizontal([
            Constraint::Length(49),
            Constraint::Length(32),
            Constraint::Min(40),
        ])
        .areas(bottom);
        draw_meters(frame, meters_area, app, app.screen == Screen::Meters);
        draw_inputs(frame, ins, app, app.screen == Screen::Inputs);
        draw_outputs(frame, outs, app, app.screen == Screen::Outputs);
        draw_mixer(frame, mix, app, app.screen == Screen::Mixer);
    } else {
        match app.screen {
            Screen::Meters => draw_meters(frame, body, app, true),
            Screen::Inputs => draw_inputs(frame, body, app, true),
            Screen::Outputs => draw_outputs(frame, body, app, true),
            Screen::Mixer => draw_mixer(frame, body, app, true),
        }
    }

    let [help_left, help_right] =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(help);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            help_text,
            Style::default().fg(Color::DarkGray),
        ))),
        help_left,
    );
    if !app.status.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("{} ", app.status),
                Style::default().fg(Color::Yellow),
            )))
            .alignment(ratatui::layout::Alignment::Right),
            help_right,
        );
    }

    // The keybinding cheat-sheet sits on top of everything when toggled (`?`).
    if app.show_help {
        draw_help_overlay(frame, body);
    }
}

/// A `width`×`height` rectangle centered in `area`, clamped so it never exceeds it. For modal popups.
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

/// The full keybinding reference, drawn as a centered modal over the body. `("Section", "")` rows are
/// headers, `("", "")` rows are blank spacers, and `(key, desc)` rows are bindings — so the layout is
/// just this table. Any key dismisses it (see `App::on_key`), so it needs no state of its own.
fn draw_help_overlay(frame: &mut Frame, area: Rect) {
    const ROWS: [(&str, &str); 30] = [
        ("Global", ""),
        ("Tab / Shift-Tab", "next / previous screen"),
        ("m", "mute all outputs"),
        ("t", "talkback mic open/close"),
        ("?", "toggle this help"),
        ("q / Esc", "quit"),
        ("", ""),
        ("Meters", ""),
        ("c", "clear peak-hold + clip latches"),
        ("", ""),
        ("Inputs", ""),
        ("↑ ↓ ← →", "move selection"),
        ("Space / Enter", "toggle switch"),
        ("p", "stereo-link the analogue pair"),
        ("", ""),
        ("Outputs", ""),
        ("↑ ↓", "select row"),
        ("Space / Enter", "toggle / advance selection"),
        ("← →", "change selection value"),
        ("s", "save preset"),
        ("", ""),
        ("Mixer", ""),
        ("↑ ↓ ← →", "move cell"),
        ("+ / -", "gain ±1 dB · 0 unity · x off"),
        ("[ / ]", "pan left / right · \\ center"),
        ("c / o", "cut / solo the source row"),
        ("(bottom row)", "Talkback → cue buses (level + pan)"),
        ("s", "save preset"),
        ("", ""),
        ("— press any key to close —", ""),
    ];

    let key_w = 16usize;
    let mut lines: Vec<Line> = Vec::with_capacity(ROWS.len());
    for (key, desc) in ROWS {
        if desc.is_empty() {
            if key.is_empty() {
                lines.push(Line::from(""));
            } else {
                lines.push(Line::from(Span::styled(
                    format!(" {key}"),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )));
            }
        } else {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("   {key:<key_w$}"),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(desc.to_string(), Style::default().fg(Color::Gray)),
            ]));
        }
    }

    let popup = centered_rect(52, ROWS.len() as u16 + 2, area);
    frame.render_widget(Clear, popup); // clear whatever's behind so the overlay reads cleanly
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Keybindings ");
    frame.render_widget(Paragraph::new(lines).block(block), popup);
}

/// A bordered panel block. When `focused` is false (only happens for the *unfocused* panels in the
/// dashboard), the border is dimmed; a focused panel — and every panel in tabbed single-screen mode
/// — keeps the default border, so the single-screen look is unchanged.
fn panel_block(title: &str, focused: bool) -> Block<'_> {
    let block = Block::default().borders(Borders::ALL).title(title);
    if focused {
        block
    } else {
        block.border_style(Style::default().fg(Color::DarkGray))
    }
}

/// The widest of `candidates` (ordered widest→narrowest) that fits `width`, as a dimmed footer line,
/// or `None` if even the shortest overflows. Lets a panel keep its full legend full-screen and fall
/// back to a short hint that still fits the narrow dashboard columns.
fn footer_line(width: u16, candidates: &[String]) -> Option<Line<'static>> {
    candidates
        .iter()
        .find(|c| c.chars().count() <= width as usize)
        .map(|c| {
            Line::from(Span::styled(
                c.clone(),
                Style::default().fg(Color::DarkGray),
            ))
        })
}

/// Append assembled footer lines below a blank separator (nothing if there are none).
fn push_footer(lines: &mut Vec<Line<'static>>, footer: Vec<Line<'static>>) {
    if footer.is_empty() {
        return;
    }
    lines.push(Line::from(""));
    lines.extend(footer);
}

/// The per-input control grid: inputs (rows) × bool controls (columns).
fn draw_inputs(frame: &mut Frame, area: Rect, app: &App, focused: bool) {
    let block = panel_block(" Inputs — switches ", focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let label_w = 12usize;
    let col_w = 7usize; // columns hold 1–4 char labels / ON·— cells; kept tight so the Mixer fits
    let (sel_r, sel_c) = app.in_sel;

    let mut lines = Vec::with_capacity(NUM_INPUTS + 1);

    // Header row: control names.
    let mut header = vec![Span::raw(format!("{:<label_w$}", "", label_w = label_w))];
    for (name, _, _) in &INPUT_CONTROLS {
        header.push(Span::styled(
            format!("{name:^col_w$}"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    }
    lines.push(Line::from(header));

    for row in 0..NUM_INPUTS {
        let row_selected = row == sel_r;
        let name_style = if row_selected {
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        // A ⇄ marker flags rows whose analogue pair is stereo-linked.
        let marker = if app.link[row / 2] { "⇄" } else { " " };
        let label = format!(
            "{marker} {:<width$}",
            meters::label(row as u16),
            width = label_w - 2
        );
        let mut spans = vec![Span::styled(label, name_style)];
        for (col, &(_, _, valid_rows)) in INPUT_CONTROLS.iter().enumerate() {
            let valid = row < valid_rows;
            let on = app.in_state[row][col];
            let (sym, mut style) = if !valid {
                ("·", Style::default().fg(Color::Black))
            } else if on {
                (
                    "ON",
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                ("—", Style::default().fg(Color::DarkGray))
            };
            if row == sel_r && col == sel_c {
                style = style
                    .bg(Color::Blue)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD);
            }
            spans.push(Span::styled(format!("{sym:^col_w$}"), style));
        }
        lines.push(Line::from(spans));
    }

    let (a, b) = (onoff(app.link[0]), onoff(app.link[1]));
    let footer = [
        footer_line(
            inner.width,
            &[
                format!("  Stereo link (p):  1-2 {a}   3-4 {b}   — linked pairs share switches + mixer sends (⇄)."),
                format!("  p: link pair (⇄)   1-2 {a}  3-4 {b}"),
            ],
        ),
        footer_line(
            inner.width,
            &[
                "  Per-input switches hydrate from the device on connect. Ø = polarity, Hi-Z = inputs 1–2."
                    .to_string(),
                "  Ø = polarity · Hi-Z = inputs 1–2".to_string(),
            ],
        ),
    ];
    push_footer(&mut lines, footer.into_iter().flatten().collect());
    frame.render_widget(Paragraph::new(lines), inner);
}

/// The Outputs screen: monitor-bus toggles (device-reported params) above headphone/line
/// selections (host-owned coefficients). Row order matches `output_activate`/`output_cycle`.
fn draw_outputs(frame: &mut Frame, area: Rect, app: &App, focused: bool) {
    let block = panel_block(" Outputs ", focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let label_w = 17usize; // fits the longest row label ("Line out op level"); trimmed so the Mixer fits
    let mut lines = Vec::with_capacity(NUM_OUT_ROWS + 2);
    for (i, row) in OutRow::ALL.iter().enumerate() {
        let label = row.label();
        let (value, is_bool) = row.value(app);
        let selected = i == app.out_sel;
        let disabled = row.is_disabled(app); // alt rows while the alt feature is off
        let marker = if selected { "▶ " } else { "  " };
        let name_style = if disabled {
            Style::default().fg(Color::DarkGray)
        } else if selected {
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        let value_span = if disabled {
            // Inert until the alt feature is enabled: dim the value, no ON/selection styling.
            Span::styled(format!(" {value} "), Style::default().fg(Color::DarkGray))
        } else {
            match is_bool {
                Some(true) => Span::styled(
                    format!(" {value} "),
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Some(false) => {
                    Span::styled(format!(" {value} "), Style::default().fg(Color::DarkGray))
                }
                None => {
                    let mut st = Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD);
                    if selected {
                        st = st.bg(Color::Blue).fg(Color::White);
                    }
                    Span::styled(format!(" {value} "), st)
                }
            }
        };
        lines.push(Line::from(vec![
            Span::styled(marker, Style::default().fg(Color::Cyan)),
            Span::styled(format!("{label:<label_w$}"), name_style),
            value_span,
        ]));
    }
    let footer = [
        footer_line(
            inner.width,
            &[
                "  Monitor Mono/Dim/Cut are device-reported params; HP/line selections are host-owned coeffs."
                    .to_string(),
                "  Space toggle · ←/→ change".to_string(),
            ],
        ),
        footer_line(
            inner.width,
            &["  \"Mute all\" = MUTE_HARDWARE_OUTPUTS — the guard SSL 360 uses internally around DSP reconfig."
                .to_string()],
        ),
    ];
    push_footer(&mut lines, footer.into_iter().flatten().collect());
    frame.render_widget(Paragraph::new(lines), inner);
}

/// A compact pan/balance tag for a mix cell: `C` (center), `L50`, `R50`, … (percent off-center).
fn pan_tag(pan: f64) -> String {
    let pct = (pan.abs() * 100.0).round() as i32;
    if pct == 0 {
        "C".to_string()
    } else if pan < 0.0 {
        format!("L{pct}")
    } else {
        format!("R{pct}")
    }
}

/// The monitor-mix grid: sources (rows) × destination buses (columns); each cell shows dB + pan.
fn draw_mixer(frame: &mut Frame, area: Rect, app: &App, focused: bool) {
    let block = panel_block(" Monitor mix — source → bus · dB + pan ", focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let label_w = 13usize;
    let col_w = 11usize;
    let (sel_s, sel_d) = app.mix_sel;

    let mut lines = Vec::with_capacity(mixer::NUM_SOURCES + 1);

    // Header row: destination bus names.
    let mut header = vec![Span::raw(format!("{:<label_w$}", "", label_w = label_w))];
    for d in &mixer::DESTINATIONS {
        header.push(Span::styled(
            format!("{:>col_w$}", d.name, col_w = col_w),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    }
    lines.push(Line::from(header));

    // One row per source.
    for (si, src) in mixer::SOURCES.iter().enumerate() {
        let row_selected = si == sel_s;
        let silenced = app.source_muted(si); // cut, or muted because another source is soloed
                                             // Cut = red, soloed = yellow (the audible one), silenced-by-others' solo = greyed.
        let name_style = if app.cut[si] {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        } else if app.solo[si] {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else if silenced {
            Style::default().fg(Color::DarkGray)
        } else if row_selected {
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        // Flag a stereo-linked analogue source so it's clear edits will mirror onto its partner.
        let name = if app.linked_partner_source(si).is_some() {
            format!("{} ⇄", src.name())
        } else {
            src.name().to_string()
        };
        let mut spans = vec![Span::styled(format!("{name:<label_w$}"), name_style)];
        for di in 0..mixer::NUM_DESTINATIONS {
            let db = app.matrix.db(si, di);
            let text = if db.is_finite() {
                format!("{db:+.1} {}", pan_tag(app.matrix.pan(si, di)))
            } else {
                "—".to_string()
            };
            let cell = format!("{text:>col_w$}", col_w = col_w);
            let selected = si == sel_s && di == sel_d;
            // Silenced sources dim their cells (the stored level still shows, just greyed).
            let mut style = if silenced {
                Style::default().fg(Color::DarkGray)
            } else if db.is_finite() {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            if selected {
                style = style
                    .bg(Color::Blue)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD);
            }
            spans.push(Span::styled(cell, style));
        }
        lines.push(Line::from(spans));
    }

    // Talkback send row — a separate strip below the matrix (talk isn't a crosspoint source, §9b).
    // Each cue-bus cell is a level + pan like a mix cell; Main carries no talk send. Drawn in magenta
    // so it reads as distinct from the green crosspoint grid.
    lines.push(Line::from(""));
    let talk_row_selected = sel_s == MIXER_TALK_ROW;
    let label_style = if talk_row_selected {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    let mut spans = vec![Span::styled(
        format!("{:<label_w$}", "Talkback"),
        label_style,
    )];
    for di in 0..mixer::NUM_DESTINATIONS {
        let (text, is_talk_cell) = match talkback_slot(di) {
            Some(slot) if app.tb_db[slot].is_finite() => (
                format!("{:+.1} {}", app.tb_db[slot], pan_tag(app.tb_pan[slot])),
                true,
            ),
            Some(_) => ("—".to_string(), true), // talk send off
            None => ("—".to_string(), false),   // Main: no talkback
        };
        let cell = format!("{text:>col_w$}", col_w = col_w);
        let mut style = if is_talk_cell {
            Style::default().fg(Color::Magenta)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        if talk_row_selected && di == sel_d {
            style = style
                .bg(Color::Blue)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD);
        }
        spans.push(Span::styled(cell, style));
    }
    lines.push(Line::from(spans));

    let footer = [footer_line(
        inner.width,
        &[
            "  SSL 360 fader dB (0 = unity) + pan (C/L/R). Talkback row feeds the cue buses. Mono = equal-power pan. — = off."
                .to_string(),
            "  +/- gain · [ ] pan · \\ center · 0 unity · x off".to_string(),
        ],
    )];
    push_footer(&mut lines, footer.into_iter().flatten().collect());
    frame.render_widget(Paragraph::new(lines), inner);
}

/// dBFS tick marks shown on the meter ruler and dropped as gridlines down each bar.
const SCALE_TICKS_DB: [i32; 8] = [-60, -48, -36, -24, -18, -12, -6, 0];

/// Position of a dBFS value within a meter bar (0.0 at the left = −60 dB, 1.0 at the right = 0 dB).
fn meter_frac(db: f64) -> f64 {
    ((db + 60.0) / 60.0).clamp(0.0, 1.0)
}

/// Column index of a tick within a `width`-wide bar (clamped to the last column).
fn tick_col(db: i32, width: usize) -> usize {
    ((meter_frac(db as f64) * width as f64).round() as usize).min(width.saturating_sub(1))
}

/// A dBFS ruler spanning a `width`-column meter bar (−60 dB at the left … 0 dB at the right), using
/// the same position mapping as `meter_bar` so the ticks line up with the fill and the gridlines.
/// Labels are placed at their column (0 dB flush to the right edge) and thinned if they'd collide.
pub(crate) fn meter_scale(width: usize) -> String {
    let mut buf = vec![b' '; width];
    let mut last_end = 0usize; // exclusive end of the last label placed; ensures a >=1 col gap
    for (n, db) in SCALE_TICKS_DB.into_iter().enumerate() {
        let label = db.to_string();
        let col = (meter_frac(db as f64) * width as f64).round() as usize;
        let start = if db == 0 {
            width.saturating_sub(label.len()) // 0 dB pinned to the right edge
        } else {
            col.min(width.saturating_sub(label.len()))
        };
        if n == 0 || start > last_end {
            for (k, ch) in label.bytes().enumerate() {
                if start + k < width {
                    buf[start + k] = ch;
                }
            }
            last_end = start + label.len();
        }
    }
    String::from_utf8(buf).unwrap_or_default()
}

fn draw_meters(frame: &mut Frame, area: Rect, app: &App, focused: bool) {
    let block = panel_block(" Meters (table 1) — █ level · │ peak-hold ", focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Layout: "label       " + bar + "  -8.3  pk  -3.1  CLIP".
    let label_w = 11usize;
    let right_w = 23usize; // "  -xx.x" + "  pk -xx.x" + "  CLIP" (clip field reserved even when off)
    let bar_w = (inner.width as usize)
        .saturating_sub(label_w + 1 + right_w)
        .max(4);

    let mut lines = Vec::with_capacity(NUM_METERS + 1);
    // dBFS scale ruler across the top, aligned to the bar columns.
    lines.push(Line::from(vec![
        Span::styled(
            format!("{:<label_w$} ", "dBFS"),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(meter_scale(bar_w), Style::default().fg(Color::DarkGray)),
    ]));
    for i in 0..NUM_METERS {
        let s = app.levels[i];
        let peak = app.peaks[i];
        let db = s.dbfs();
        let peak_db = peak.as_sample().dbfs();

        let mut spans = vec![Span::styled(
            format!("{:<width$} ", meters::label(i as u16), width = label_w),
            Style::default().fg(Color::Gray),
        )];
        spans.extend(meter_bar(bar_w, db, peak.clipped, peak_db));

        // Numeric current + held-peak dB.
        let fmt = |d: f64| {
            if d.is_finite() {
                format!("{d:>5.1}")
            } else {
                " -inf".to_string()
            }
        };
        spans.push(Span::styled(
            format!("  {}", fmt(db)),
            Style::default().fg(Color::Gray),
        ));
        spans.push(Span::styled(
            format!("  pk {}", fmt(peak_db)),
            Style::default().fg(Color::DarkGray),
        ));

        // Latched-clip badge (sticky until `c`); reserve its width when clear so columns align.
        if peak.clipped {
            spans.push(Span::styled(
                "  CLIP",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::LightRed)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::raw("      "));
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

/// A tri-color horizontal level bar with a peak-hold tick. `db` is the live dBFS (0 = full scale),
/// `peak_db` the held peak (drawn as a bright `│` marker), `clip` the latched over flag.
pub(crate) fn meter_bar(width: usize, db: f64, clip: bool, peak_db: f64) -> Vec<Span<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let col_at = |d: f64| (meter_frac(d) * width as f64).round() as usize;
    let filled = col_at(db);
    let green_end = col_at(-18.0);
    let yellow_end = col_at(-6.0);

    // Color for a filled column by its zone (red brightens to LightRed once clip has latched).
    let zone = |col: usize| -> Color {
        if col < green_end {
            Color::Green
        } else if col < yellow_end {
            Color::Yellow
        } else if clip {
            Color::LightRed
        } else {
            Color::Red
        }
    };

    // Build the bar one column at a time, then coalesce equal-styled runs into spans. A per-column
    // model keeps the peak marker trivial to overlay wherever it lands (inside or past the fill).
    let mut cells: Vec<(char, Style)> = (0..width)
        .map(|col| {
            if col < filled {
                let mut st = Style::default().fg(zone(col));
                if col >= yellow_end {
                    st = st.add_modifier(Modifier::BOLD); // red zone stands out
                }
                ('█', st)
            } else {
                ('·', Style::default().fg(Color::Black))
            }
        })
        .collect();

    // Drop a faint gridline from each ruler tick — but only through the *unlit* part of the bar, so
    // the lit level still reads cleanly on top (like a meter bridge's scale lines behind the bars).
    for db in SCALE_TICKS_DB {
        let c = tick_col(db, width);
        if c >= filled {
            cells[c] = ('┊', Style::default().fg(Color::DarkGray));
        }
    }

    if peak_db.is_finite() {
        let p = col_at(peak_db).min(width - 1);
        cells[p] = (
            '│',
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        );
    }

    let mut spans = Vec::new();
    let mut run = String::new();
    let mut run_style = cells[0].1;
    for (ch, st) in cells {
        if st != run_style {
            spans.push(Span::styled(std::mem::take(&mut run), run_style));
            run_style = st;
        }
        run.push(ch);
    }
    spans.push(Span::styled(run, run_style));
    spans
}
