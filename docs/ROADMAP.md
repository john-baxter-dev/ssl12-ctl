# Roadmap / next steps

A running checklist so nothing gets lost. Each item is tagged:
- 🟢 **mock** — fully doable with no hardware (`cargo run --no-default-features --features tui --bin ssl12tui`)
- 🔧 **bench** — needs the real SSL 12 plugged in (Linux)
- 📦 **housekeeping** — repo/publish chores

## Done (for context)

- ✅ Vendor control protocol decoded (framing/CRC/Q6.25, control map, meters, crosspoint matrix).
- ✅ FTDI transport solved & HW-confirmed: port-open + per-packet status strip + tile-init + `0x1b` keepalive.
- ✅ Live control confirmed on Linux: phantom, mix, mute, routing; **meter stream stable**.
- ✅ `Transport` trait + `MockTransport` (synthetic meters, echoed writes, connect-time param dump).
- ✅ TUI: Meters screen + Mixer matrix screen (host-side `MixMatrix`, crosspoint writes/reconcile).
- ✅ Docs: `PROTOCOL.md` (incl. §0.5 transport, §8 ownership split), `ARCHITECTURE.md`.

---

## 1. Reconciliation — finish the "real client" loop

- [x] 🟢 **Mix + output persistence.** `preset.rs` (behind the `config` feature): TOML preset
      (`source → { bus = dB }`, omit = off) at `<config dir>/ssl12/mix.toml` via `serde`+`toml`+`dirs`.
      Also persists the other host-owned coefficients that the device never reports back — HP gain
      modes + line operating level (`[outputs]`) and analogue stereo-link flags (`[links]`), all
      `#[serde(default)]` so older files still load. TUI loads + pushes them on connect; `s` saves
      (from the Mixer **or** Outputs screen). The host-owns-it half of reconciliation — done.
      _Future:_ multiple named presets / a preset picker.
- [x] 🟢 **Parameter hydration (input switches).** `ssl12tui` Inputs screen folds the device's
      connect dump into UI state for 48V / HPF / Line / Hi-Z / Ø polarity (bidirectional: optimistic
      write + device echo reconcile; tested). _Still TODO:_ clock select, output levels, bus switches
      (no UI for those yet — fold in when those surfaces are added).
- [x] 🟢 **Robust dump capture on hardware.** `device.rs::handshake()` reads frames looking for
      version replies and discards the rest — so it swallows any param VALUE dump that arrives right
      after. Fixed by the "re-request key values" route: `App::request_param_hydration` pulls each
      input switch + monitor-bus param with `PARAM_VALUE_REQUEST` on connect, so replies land during
      the normal `pump` loop instead of the handshake. Bench-confirmed: the SSL 12 answers these
      requests with `PARAM_VALUE_BOOL` and the Inputs screen hydrates from real device state.

## 2. TUI feature breadth (all 🟢 mock)

- [x] 🟢 **Input strip controls** beyond phantom: HPF, line/mic, Hi-Z, polarity — done as the
      Inputs screen grid (`ssl12tui`). _Future:_ per-input gain if/when a gain control is mapped.
- [x] 🟢 **Output bus controls:** Outputs screen — monitor Mono/Dim/Cut + hardware mute (params,
      hydrate), HP gain mode + line operating level (coeff selections, host-owned → persisted +
      pushed on connect, see Mix persistence). _Future:_ `OUTPUT_LEVEL`/`OUTPUT_BUS_LEVEL` faders,
      dim level, talkback, loopback source, alt speaker.
- [x] 🟢 **Mixer pan/balance.** Each cell now carries a pan position (`-1..+1`) alongside its fader
      dB; `mixer::fader_pan_to_leg_coeffs` distributes the gain across the L/R legs and `cell_writes`
      writes the two legs independently. Mono sources use an **equal-power** law (−3 dB center),
      stereo sources a **balance** law (no center dip). Edited with `[`/`]` (pan) and `\` (center) on
      the Mixer screen, shown as `C/L50/R50`, persisted per-cell in the preset, independently
      adjustable on each leg of a stereo-linked pair (linking only seeds the hard L/R spread as a
      default), and reconciled back from both legs. _Open (🔧):_ exact match to the SSL 360 app's pan
      curve is unverified — equal-power is the standard default, swappable in one function.
- [x] 🟢 **Mixer source cut / solo.** `c` cuts (mutes) the selected source row, `o` solos it; both
      mirror onto a stereo-linked partner. Host-side + **ephemeral** (not persisted): the `MixMatrix`
      keeps the real levels and a silenced source (`App::source_muted` = cut, or another source
      soloed) just writes its crosspoint cells to off. Cut rows render red, soloed yellow, and
      solo-silenced rows grey out. The reconcile path skips silenced sources so the mock's off-echo
      can't clobber the stored level (real hardware never echoes coefficients anyway).
      **Bench-confirmed** (cut drops a source from the mix, solo isolates it, linked pairs track).
- [x] 🟢 **Meter polish:** peak-hold + clip latch + numeric peak readout. `meters::PeakHold`
      (rise-instant / hold 1.5 s / decay ~12 dB/s, sticky clip latch; unit-tested) is advanced from
      the meter stream in `pump`. The Meters screen draws a `│` peak tick on each bar, a `pk` dB
      column, and a latched `CLIP` badge; `c` clears the holds/latches. _Future:_ configurable
      ballistics if anyone wants VU vs PPM.
- [x] 🟢 **UI polish:** layout/tabs, keybinding help overlay, resize handling. Tabbed/dashboard
      reflow re-evaluates every frame (`view::dashboard_fits`), per-screen footer hints, and `?`
      toggles a modal keybinding cheat-sheet (`view::draw_help_overlay`; any key dismisses).
- [x] 🟢 **Loopback source select.** Outputs "Loopback source" row → `coeff::LOOPBACK_SOURCE`
      (selection, idx 0), host-owned → persisted + pushed on connect. _Open (🔧):_ idx 0 unconfirmed.
- [x] 🟢 **Audio dim level.** Outputs "Monitor — Dim level" row → `coeff::OUTPUT_BUS_DIM_LEVEL`
      (Q6.25 dB). ←/→ nudge ±1 dB (clamped −40…0), Space resets to default; host-owned, so
      persisted + pushed on connect like the other coeffs. **Bench-confirmed:** the dim coeff takes
      the same `DEVICE_REF_OFFSET_DB` (+3.01) correction as the crosspoints (`App::push_dim_level`),
      so 0 dB = true unity (no change vs Dim-off) and the dB scale tracks. _Open (🔧):_ exact
      factory default value still a guess (we use −20).
- [x] 🟢 **Left channel polarity invert.** Outputs screen "Monitor — Ø L" row → `param::OUTPUT_BUS_PHASE_L`.
- [ ] Scribble strip per channel?
- [ ] Add "follow monitor" mode for each of the other outputs?
- [x] 🟢 **Talkback routing to the output mixes.** A **Talkback row in the Mixer pane** (below the
      crosspoint matrix, like SSL 360) — each cue bus (Line 3-4 / HP A / HP B) is a per-bus level +
      pan, a mono mix cell whose L/R legs drive `coeff::TALKBACK_LEVEL` (Q6.25, idx 2..=7) via
      `mixer::fader_pan_to_leg_coeffs` (`App::push_talkback`). Main (idx 0,1) is excluded — talk feeds
      the cue/phones, not the control-room mains. Shares the matrix's cell keys (+/- gain, [ ] pan,
      \ center, 0 unity, x off); host-owned → persisted + pushed on connect. _Open (🔧):_ the 0 dB /
      centered default we push may differ from the firmware default (watch for a change on connect).
- [ ] 🔧 **ADAT routing to the mixes.** Add ADAT inputs 1–8 (meter idx 4–11) as mixer sources feeding
      the crosspoint matrix, alongside the playback returns + analogues. Needs a capture to pin their
      `MIXER_CROSSPOINT_TABLE` source slots (the 4 analogues are slots 8–11; ADAT slots unconfirmed),
      then extend `mixer::SOURCES`. Big win for routing ADAT-connected gear into the monitor mix.
- [x] 🟢 **Talkback push-to-talk.** Global `t` key → `param::OUTPUT_BUS_TALKBACK_ENABLE`
      (`App::toggle_talk`). Press-to-toggle (terminals don't deliver reliable key-up), with a bright
      `●TALK` title badge so the open mic is never forgotten. **Bench-confirmed:** the enable bool
      opens the mic on its own (unit light + meter 28 + audio), and the device firmware auto-dims the
      monitors while talking. _Future:_ optional auto-release timeout.
- [ ] 🔧 **Physical button remapping.** The 3 programmable hardware buttons map via
      `coeff::USER_BUTTON_FUNCTION` (selection, idx 0..=2), default **Cut / Alt / Talk**. Expose a UI
      to reassign each. Needs a bench capture to enumerate the selection values → functions (the
      button-function selection values aren't decoded yet).
- [x] 🟢 **ALT speaker support.** Three Outputs rows: **Alt spk enable** (`coeff::OUTPUT_BUS_ALT_SPK_ENABLE`,
      host-owned bool, off by default), the live **Monitor — Alt** switch (`param::OUTPUT_BUS_ALT`,
      device-reported → hydrate, like Mono/Dim/Cut), and **Alt spk trim** (`coeff::OUTPUT_BUS_ALT_TRIM_LEVEL`,
      Q6.25, bipolar ±12 dB, `DEVICE_REF_OFFSET_DB` so 0 = unity). The enable **gates** the switch +
      trim — they grey out and no-op until it's on (`OutRow::is_disabled`, mirrors SSL 360). Enable +
      trim persisted + pushed on connect. _Open (🔧):_ trim range and the coeff indices are guesses.

## 3. Accuracy / fidelity

- [x] 🟢 **Fader taper**: the SSL 360 fader-dB ≠ crosspoint-coeff-dB.
      a single `DEVICE_REF_OFFSET_DB` (+3.01, the full-scale-over-ref) plus the pan-law leg term map
      between them — the `MixMatrix` holds **fader** dB (0 = unity), and `fader_pan_to_leg_coeffs` /
      the TUI reconcile path convert at the device boundary. Anchored to both capture-verified points:
      mono fader 0 centered → device 0 dB ref (§4 worked example), stereo fader 0 → +3.01 dB. _Open
      (🔧):_ the law is *piecewise* near the bottom of the throw; we use the linear region, so the
      displayed dB just below the lowest breakpoint may be
      slightly off until the breakpoint arrays are pulled from a device capture.
- [x] 🔧 **Meter idx 28 = talkback mic** — HW-confirmed (holding Talk moves meter 28).
- [ ] 🔧 **Meter sub-orders:** ADAT 4–11, Playback 22–27 are by-elimination only. Pin them
      with one-channel-at-a-time captures (low value — skip unless wanted).

## 4. Hardware validation (🔧 bench, when back)

- [ ] 🔧 Confirm the new docs/reconciliation behavior against the device (param hydration, mix push).
- [ ] 🔧 **Hi-Z auto-detect?** The manual suggests Hi-Z engages automatically when a 1/4" jack is in
      the front instrument input. Test: plug a TS cable into input 1 and watch whether the Inputs-grid
      Hi-Z cell flips on by itself (device-reported); then with a mic plugged in, toggle Hi-Z in the
      TUI and see whether the write sticks or snaps back. If it's read-only/auto, make the Hi-Z column
      a non-editable status indicator (still hydrates + shows, Space is a no-op) rather than a toggle.
- [ ] 🔧 Re-verify writes still move hardware after the transport refactor (`heartbeat`, status strip).
- [ ] 🔧 Sanity-check the seeded/default mix push doesn't fight the device's power-on default oddly.
- [ ] 🔧 Linux integration: udev rule (README), that the vendor 0xFF interface claims alongside
      `snd-usb-audio`, the interface number.

## 5. Code cleanup

- [x] 🟢 **Pure `App::new` + explicit `connect`.** Construction no longer does disk I/O or DSP
      writes; the connect-time sequence (load preset → push mix/outputs/links → hydrate params) lives
      in `App::connect`, which `main` calls once the transport is open. Keeps unit tests deterministic
      (they build an `App` without reading the real `~/.config/ssl12/mix.toml`).
- [x] 🟢 **Quiet the diagnostics.** `device.rs`' bring-up stderr (`ftdi_open` per-step report,
      keepalive warnings) now sits behind a runtime `VERBOSE` flag (`device::set_verbose`), off by
      default so stray stderr can't corrupt the TUI canvas. `ssl12tui --verbose` opts back in;
      `ssl12ctl` enables it unconditionally (it's the diagnostic CLI). The TUI's "falling back to
      mock" notice is likewise `--verbose`-only — the status bar already shows the MOCK backend.
      _Left as-is:_ the `rawin`/`golive`/`probe` subcommands print intentionally (diagnostic-only).
- [x] 🟢 `FORBIDDEN_USB_CODES` guard is unit-tested (`device::is_forbidden_usb_code`): the
      firmware/flash family stays denied, normal control traffic stays allowed, and 0x1B is guarded
      against stray `send_usb` while `heartbeat`'s direct `write_bulk` remains the only keepalive path.

## 6. Pre-publish housekeeping (📦)

- [ ] 📦 **Publish metadata.** Fill in the remaining `Cargo.toml` fields for a crates.io release
      (`repository`, `homepage`, `keywords`, `categories`, `readme`, `rust-version`) so `cargo publish`
      and the crate page are complete.
- [ ] 📦 **CI** (GitHub Actions): gate `cargo fmt --check`, `cargo clippy --all-targets -D warnings`, and
      `cargo test` across the feature combos that build independently — `--no-default-features`,
      `+tui`, `+tui,dev-tools`, and default (`usb`). The feature-flag matrix is the main regression
      risk (each flag can break alone), so the matrix is the point of the CI, not just one build.
- [ ] 📦 **Easy install.** Foundations in place: `packaging/70-ssl12.rules` (the udev rule, single
      source of truth) + `ssl12ctl install-udev`/`uninstall-udev` (embeds the rule via `include_str!`,
      self-elevates with sudo, reloads udev) for the `cargo install` path. _Still TODO:_ a portable
      `just install`/`Makefile` target (build → `cargo install --path .` → drop the rule → reload) for
      from-source on any distro, and a **PKGBUILD → AUR** package (reuses `packaging/70-ssl12.rules`,
      runs `udevadm trigger` via the install hook) — the one-command path for Arch-family users.

## 7. The connect-time mute — and whether a daemon is the fix

**Status:** mostly resolved. **Test A-warm passed** (2026-06-19): with the mute removed, a warm
reconnect re-pushes all 240 cells with **no audible pop/zipper** — idempotent coefficient re-writes
are transparent on hardware. One thing left: **test A-cold** (the rewrite over *factory defaults*,
not over the existing state) — pending a cold-boot test (2026-06-20). **Leaning fix: Tier 0.5** —
*keep* the mute but speed up the push so its window is imperceptible (preserves the manufacturer's
atomic-reconfigure safety; see "Why the original software mutes"), rather than deleting it. The daemon
is **not** the likely outcome; it only revives in the worst case, deferred to the GUI milestone.

### The symptom

Launching the TUI (and later a GUI) mutes the device for ~0.5 s. This is **deliberate**, not a sync
artifact: `ssl12tui`'s `push_full_mix` wraps `set_hw_mute(true) … set_hw_mute(false)` around a blind
re-push of all ~240 crosspoint cells (plus `clear_bus_follow`). The mute hides the matrix being
rebuilt cell-by-cell through audible intermediate states. The re-push is **forced** by the design:
the mixer is host-authoritative and the device never reports coefficients back (PROTOCOL.md §8/§9b),
so a fresh process can't know the device's current state and must re-assert the whole matrix.

### Why the original software mutes (and why it gets away with it)

The mute mirrors SSL 360, and there's a real reason for it — two, actually, and both are
value-independent (they don't depend on whether any individual write changes a level):

1. **Atomic reconfigure.** Rewriting a crosspoint matrix one coefficient at a time has no guaranteed-safe
   ordering — a half-applied matrix can briefly route something loud or wrong. Muting makes the whole
   push look atomic to the listener: no output until the config is fully applied. It also covers an
   *interrupted* push (crash, cable yank mid-write) leaving the device stuck sounding wrong.
2. **Future-proofing a broader reconfigure.** SSL 360's connect sequence may mute around more than
   crosspoints (clock source, sample rate, routing topology) — operations that genuinely glitch
   regardless of warm/cold. We only push crosspoints + `clear_bus_follow` today, but clock-select is on
   the TODO list; if/when it lands on connect, *that* needs a mute irrespective of this finding.

**The crucial difference is architecture, not correctness.** SSL 360 is a **resident service**, so it
mutes *once* — when the service connects to the device, i.e. once per power-cycle. Opening the SSL 360
*window* attaches to the already-running service; it does not re-push or re-mute. So the mute cost is
real but invisible to them. We pay it on **every UI launch** because we're per-process. The original
isn't doing something we're failing to replicate — residency makes the same mute *free* for them and a
recurring tax for us. This is the same conclusion from the other direction: **the mute was never the
problem; the per-launch repetition is.** SSL 360 solves the repetition with the daemon, not by skipping
the mute — so its design argues for *keeping a cheap mute*, not for removing it.

### The decision tree

**Key clarification:** the proposed Tier 0 fix only *removes the mute* — it does **not** skip the
push. `push_full_mix` still re-asserts all 240 cells on every connect, so the mix is always correct
and **no warm/cold detection is needed**. Warm/cold detection is only required to *skip* the push
(Tier 1), which we are not doing. So the only thing removing the mute changes is whether the *rewrite
itself* is audible — split by device state:

- **Warm rewrite** (device already holds the state — the common reopen case): **confirmed clean**
  (test A-warm, 2026-06-19). Every write is a value no-op; transparent. The mute was pure downside
  here — a ~0.5 s output dropout on every UI launch for nothing.
- **Cold rewrite** (factory defaults, first connect after a power-cycle): writes actually change
  values, so the rebuild *could* zipper. **Untested** — A-warm ran over existing state, so it says
  nothing about this. The earlier "you're not listening at power-on" was an assumption, not a fact:
  if audio is already passing through the default monitor routing when you open the UI, you'd hear it.

So the remaining branch point is **test A-cold** — but note the rationale above shifts the *preferred*
landing spot from "delete the mute" to "keep a cheap mute":

- **Tier 0.5 — keep the mute, speed up the push (recommended default, regardless of A-cold).**
  Batch/pipeline the 240 writes so the mute window shrinks from ~0.5 s to imperceptible. This keeps the
  manufacturer's atomic-reconfigure safety *and* the future-proofing for clock/sample-rate reconfig,
  while killing the recurring per-launch dropout. No detection, no daemon. **This is the target
  outcome** — it approximates what residency buys SSL 360 (a mute too fast to notice) without going
  resident.
- **Tier 0 — delete the mute outright (only if A-cold is clean *and* you accept the trade).** If the
  cold rewrite is also transparent (likely, if the DSP ramps coefficient changes), you *can* just
  delete the two `set_hw_mute` calls — ~2 lines. But this throws away the atomic-push insurance and the
  clock/SR future-proofing, so prefer Tier 0.5 unless you want the absolute minimal change.
- **Tier 1 — suppress the push on warm reconnect (only if the cold zipper matters *and* the push can't
  be sped up enough).** Needs real cold/warm knowledge → the daemon branch.
- **Tier 2 — the daemon (only inside Tier 1).** See below. Don't build it unless you land in Tier 1.

### Why cold/warm detection wants a resident process (the Tier 1 problem)

"Just identify cold vs warm from each client" sounds cheap but fails in the *dangerous* direction. A
power-cycle is an **event** between two process lifetimes; a one-shot client can only read *current*
sysfs state, not witness the disconnect/connect. The current-state tokens (`devnum`) get recycled, so
two power-cycles can collide on the same value → the client wrongly concludes "warm" → skips the push
→ **device sits at factory defaults while the UI shows the preset.** A spurious *extra* mute
(false-cold) is harmless; a *missed* push (false-warm) is the bad failure, and the cheap marker fails
exactly that way. Reliable detection wants something alive across the power-cycle gap to witness the
event — i.e. residency.

### The bus-power keystone (makes the Tier 2 daemon clean, if reached)

The SSL 12 is **bus-powered** (no separate PSU), so a daemon's lifetime can be bound to device
presence via systemd/udev: **daemon-alive ⟺ device-powered ⟺ DSP-state-valid**. The warm/cold flag
then *dissolves* — the daemon's existence is the flag, and systemd tracks it for free. This is only
reachable *through* the daemon (it's the resident witness Tier 1 needs), so bus-power argues for the
daemon over the marker — but only once you're already in Tier 1.

**Robustness rule:** key warm-vs-cold on **observed USB connect events**, not on daemon uptime. Treat
every fresh enumeration as cold (re-push), continuity as warm. Correct for unplug/replug *and* a hard
power switch automatically (see test C).

### Why a full daemon over a minimal presence-watcher (if you reach Tier 1)

Cold/warm detection alone needs only a tiny resident *presence-watcher* (watch udev, hold a flag;
clients still talk to the device directly). But once you ship *any* resident user service + systemd
unit you've paid nearly all the install/ops cost, so take the full daemon and get the rest for the
marginal code: one owner of the exclusive device handle, concurrent multi-client access (CLI poke
while the GUI is open), and a single source of truth a GUI+TUI reconcile against. This is the standard
control-panel architecture (SSL 360, UA Console, RME TotalMix server, MOTU) — almost certainly why
SSL 360 itself runs a resident service.

### Tier 2 architecture (slots into the existing seam)

- Daemon owns the real `Ssl12` transport + `heartbeat` + meter stream + the in-memory matrix and the
  `initialized` flag.
- Clients get a `SocketTransport` — a **third `Transport` impl** alongside `Ssl12`/`MockTransport`,
  so the TUI/CLI/GUI code is unchanged (same trick the mock plays).
- Unix socket (XDG runtime dir), length-prefixed frames wrapping the existing DSP messages + a meter
  subscription. Push the full mix **once** at daemon start; diff-on-edit thereafter.
- **Client fallback (code, not packaging — the bigger design task):** try the socket, and if no
  daemon is up, either talk to the device directly or print a clear "start ssl12d" hint. Don't let a
  down service brick the UI.

### Tier 2 installation delta (modest — the existing setup pre-pays most of it)

- The udev rule already does `TAG+="uaccess"` (logind ACL for the logged-in user), so a **user-level**
  systemd service (`systemctl --user`) inherits device access **with no new privileges** — no root for
  the daemon itself. The only root step (dropping the rule) already exists via `install-udev`.
- Incremental artifact is essentially **one file**: a `ssl12d.service` user unit (embed via
  `include_str!` like `UDEV_RULE`); an `install-daemon` subcommand mirrors `install-udev` but is
  *simpler* (user units need no sudo).
- **Autostart at login (ship first):** `systemctl --user enable --now ssl12d`; daemon idle-waits when
  no device, claims it on appearance. AUR: install unit to `/usr/lib/systemd/user/`.
- **Device-activated (later refinement):** via the udev rule so daemon lifetime == device presence
  (the binding above). The one fiddly bit: udev's native activation targets *system* units, not user
  services, so it needs a small bridge (templated user unit, or login-autostart + self-detect plug).
- **The real new cost is conceptual, not mechanical:** it becomes a *service* (status/restart/`journalctl
  --user -u ssl12d`, restart-after-upgrade), plus the client-fallback path above.

### The marker-file alternative (the Tier 1 fallback if you skip the daemon)

A **host-side marker file keyed to USB enumeration** (no daemon): record the device's USB session
identity; a fresh client compares to detect cold vs warm. Made *viable in principle* because bus-power
makes re-enumeration a sound proxy for power-cycle — but it's the fragile path: a one-shot reader can't
witness the enumeration *event*, `devnum` tokens recycle, and the failure mode is the dangerous
false-warm (see "Why cold/warm detection wants a resident process"). It also solves **only** the mute.
If you land in Tier 1, prefer the daemon — same residency requirement, strictly cleaner, and it brings
concurrent access + shared-truth for free.

### Bench tests (🔧)

- [x] 🔧 **A-warm — Is the mute necessary on a *warm* reconnect?** Dropped the `set_hw_mute` wrapper in
      `push_full_mix`, reconnected against a device that already held the state, listened. **Result
      (2026-06-19): totally clean** — idempotent re-writes are transparent, no pop/zipper. The mute was
      pure downside on the common reopen path.
- [ ] 🔧 **A-cold — Does the cold rewrite zipper?** Power-cycle the SSL 12 so it's at factory defaults,
      get audio passing through the default monitor routing, launch the TUI **with the mute removed**,
      and listen as it pushes. This tells you *whether* the cold case glitches — but note the preferred
      fix is **Tier 0.5 either way** (keep the mute, speed up the push), since the mute also buys
      atomic-reconfigure safety + clock/SR future-proofing. **Clean** only unlocks the option to delete
      it outright (Tier 0) if you want the minimal change; **zippers** confirms Tier 0.5 is required.
      While here, also check whether `clear_bus_follow` (a routing *mode* toggle) is the click source
      rather than the coefficient writes — if so, condition *that* instead.
- [ ] 🔧 **B — Does the device retain DSP state across a host disconnect while powered?** (Believed
      yes.) Only matters in Tier 1: confirms retention is a hardware given, so the daemon is purely
      about the warm/cold *knowledge*, not keeping state alive.
- [ ] 🔧 **C — Power switch: hard or soft?** (Only matters in Tier 1/2.) With `udevadm monitor`/`lsusb`
      open and the cable plugged, flip the power switch off→on. USB disconnect/reconnect fires → hard
      switch, lifetime-binding is sound. No USB event but DSP resets → soft switch (stale-warm risk);
      mitigate with a manual "re-apply preset" key or a device-authoritative sentinel param polled as a
      tripwire.

## Notes / open design questions

- The mock's coefficient echo and connect dump are **mock conveniences** to exercise UI paths; real
  hardware reports parameters (not coefficients) — keep the mixer host-authoritative.
- Audio I/O stays out of scope — it's class-compliant UAC2, handled by `snd-usb-audio`.
