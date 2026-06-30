# SSL 12 — Vendor Control Protocol Specification

Derived by observing the SSL 360 application's USB traffic (USBPcap captures) and verified against
real SSL 12 hardware. Status: **decoded & verified**. Audio streaming is separate (standard UAC2 —
see below).

> **Interoperability note.** This document describes the SSL 12's *control* wire protocol as it can
> be observed on the USB bus, for the sole purpose of interoperating with hardware the user owns on
> a platform the vendor does not support (Linux). It documents byte layouts, message numbers, and
> observed device behaviour — all visible to any USB monitor — not any vendor source code.

---

## 0. Big picture

The SSL 12 exposes **two logical functions** over USB:

1. **Audio streaming + standard volume/mute** — USB Audio Class 2.0. On Windows this is served by a
   rebadged Thesycon-class driver; on Linux, `snd-usb-audio` handles it natively. **No reverse
   engineering required.**
2. **Onboard mixer / DSP control** (monitor mix, routing, input gain, 48V, Line/Hi-Z, etc.) —
   a **vendor-specific bulk protocol** spoken by the SSL 360 app. This document specifies (2).

Transport: **vendor interface (bInterfaceClass 0xFF), bulk endpoint 0x02 OUT** (and a bulk IN
for replies). Confirm exact endpoints with `lsusb -v` on Linux.

The protocol is a 3-layer stack:

```
 ┌─ vendor serial frame ── FF | Code | Len | Payload | CRC
 │     Code = USB message code (DSP data = 0x6B for mixer/DSP)
 └──── Payload = DSP message ── MsgCode(u16) | Number(u16) | Index(u16) | Value
```

All multi-byte fields are **little-endian** on the USB path.

> **⚠ The bulk pipe is an FTDI UART bridge, not a raw vendor pipe.** The `31e9:0024` control
> interface is an FT232R-class serial bridge, so before *any* of the 3 layers above will work you
> must (a) "open the port" with FTDI vendor control transfers, (b) strip 2 FTDI status bytes from
> every IN packet, and (c) send a periodic keepalive or the device re-enumerates. This was the hard
> part of bring-up — see **§0.5** below. Skip it and you get silence (or a device that streams meters
> for ~5 s then drops off the bus). **HW-CONFIRMED on Linux: with §0.5 in place, control,
> meters, and a stable connection all work.**

---

## 0.5. Transport layer — FTDI UART bridge (READ FIRST)

The control device is a **Future Technology Devices FT232R-style** USB→UART bridge. The SSL MCU is on
the UART side; the 3-layer serial/DSP protocol is a byte stream *through* that UART. Three things
the FTDI layer imposes — all four bring-up layers must be present together or metering fails:

**(1) Port-open via FTDI vendor control transfers.** Before bulk I/O, replicate the SSL 360 app's open
sequence on EP0 (`bmRequestType = 0x40` vendor-out, **all `wIndex = 0`**, no data stage). Captured
byte-for-byte:

| Order | bRequest | Name | wValue | Meaning |
|---|---|---|---|---|
| 1 | 0x00 | RESET | 0x0000 | reset bridge |
| 2 | 0x04 | SET_DATA | 0x0008 | 8 data bits, no parity, 1 stop (8N1) |
| 3 | 0x02 | SET_FLOW_CTRL | 0x0000 | no flow control |
| 4 | 0x03 | SET_BAUD_RATE | 0x001A | divisor 26 → **115200 baud** |
| 5 | 0x09 | SET_LATENCY | 0x0002 | 2 ms latency timer |
| 6 | 0x00 | RESET | 0x0001 | purge RX |
| 7 | 0x00 | RESET | 0x0002 | purge TX |

Notably the app sends **no MODEM_CTRL (DTR/RTS)**. Without this open, the MCU never sees the port as
live and streams nothing.

**(2) Per-packet status-byte stripping.** Every bulk IN transfer is FTDI-framed: each `wMaxPacketSize`
(64-byte) packet begins with **2 status bytes** (modem status + line status; idle value `31 60`),
followed by up to 62 payload bytes. You must strip those 2 bytes from *each* 64-byte block *before*
reassembling the serial byte stream — otherwise multi-packet frames (meters) get `31 60` injected
mid-frame and fail CRC. (This is why an earlier decoder showed meter frames as `[BAD CRC]`.)

**(3) Keepalive — USB code `0x1B` with a rolling counter.** During normal operation the host sends
`OUT 0x1b [NN]` where `NN` cycles `00,01,02,03,00…`, roughly every 3 meter frames. **This is a
watchdog keepalive: without it the device re-enumerates (~5 s), seen as `No such device` mid-stream.**
The frame is a normal serial frame (`FF 1B 01 NN crc`). It rides the **same** bulk endpoints and the
**same USBPcap device address** as all other SSL 12 traffic — confirming it is the SSL 12's own
command, not a second device. (Code 27 is otherwise a flash command; the benign 1-byte counter
payload is the keepalive — real flashing uses block codes 10–18.) A client should send it on a timer
well under 5 s (this client uses 500 ms).

**Putting it together — the four layers that must coexist:** FTDI port-open → tile-init handshake
(§9a) → FTDI status-strip on every read → `0x1b` keepalive on a timer. Each is individually useless;
metering only works with all four. The reference Rust client does all four in `device.rs`
(`ftdi_open`, `read_frames` de-status, `handshake` tile-init, `heartbeat`/`keepalive_if_due`).

---

## 1. Layer 1 — serial framing

```
offset  field          notes
  0      0xFF           StartCode / sync (= 255)
  1      Code           message code (u8) — see §2
  2      PayloadLength  u8 = number of payload bytes (excludes CRC)
  3..    Payload        PayloadLength bytes
  last   CRC            u8 additive checksum
```

**CRC** (additive; identical on TX and RX):

```
crc = (Code + PayloadLength + Σ(Payload bytes)) & 0xFF
```

> The CRC deliberately **excludes** the 0xFF start byte.
> Verified against captures, e.g. fader 0 dB frame
> `FF 6B 0A  06 00 01 00 0a 00 e6 09 6a 01  E0` → 0x6B+0x0A+Σ = 0x1E0 → CRC 0xE0 ✓.

Receiver resync: read bytes until one equals 0xFF, then read Code, Len, Payload, CRC.

---

## 2. Layer 2 — USB message codes

The `Code` byte selects a command. Only a subset of the observed code set is relevant to the
SSL 12; these are the project's own identifiers for the codes it uses. Key ones:

| Code | Name | Use |
|---|---|---|
| 0x6B (107) | `USB_SEND_DSP` | **carries a DSP/mixer message (§3)** |
| 0x6C (108) | `USB_RECV_DSP` | inbound DSP data |
| 0x2B (43)  | `USB_REQUEST_CONTROL_STATES` | bulk state dump — **NOT supported on SSL 12** (replies with an invalid-command error 404; the device sends its param VALUE dump unsolicited instead, see §8) |
| 0x05 (5)   | `USB_INIT_TILE` | init/handshake |
| 0x06 (6)   | `USB_RECONNECT_REQUIRED` | |
| 0x09 (9)   | `USB_DISCONNECT` | |
| 0x4B (75)  | `USB_GET_SOFTWARE_VERSION_INT` | |
| 0x4E (78)  | `USB_GET_HW_VERSION` | |
| 0x01/0x02  | `USB_GET_IS_TILE` / `USB_GET_TILE_ID` | enumeration |

(Only the SSL 12-relevant subset of the code set is listed.)

---

## 3. Layer 3 — DSP message

Serialized as the **payload** of a `USB_SEND_DSP` (0x6B) frame. Base layout:

```
offset  field            type    notes
  0      MessageCode      u16     DSP message code (§3.1)
  2      ParameterNumber  u16     "number" selector
  4      ParameterIndex   u16     "index" selector (channel/control)
  6      Value            i32     payload (format depends on MessageCode)
```

(Bool/selection/int variants change the trailing value size.)

### 3.1 DSP message codes

The message codes observed in SSL 12 traffic, with descriptive labels for each:

```
 0  none                       10  selection update
 1  request protocol version   11  selection value
 2  protocol version reply     12  Q6.25 update
 3  value request              13  Q6.25 value
 4  bool update                14  request DSP version
 5  bool value                 15  DSP version reply
 6  Q6.25 coeff update         16  int update
 7  bool coeff update          17  int value
 8  selection coeff update     18  int coeff update
 9  meter table (15-bit)
```

A "param" update/value carries a device-authoritative parameter; a "coeff" update/value carries
a host-authoritative coefficient (the ownership split is §8). Faders/gains observed in captures use
the **Q6.25 coeff update (6)**. Codes above 18 weren't seen in any SSL 12 capture.

### 3.2 Q6.25 fixed-point values (faders / gains)

`Value` is a signed **Q6.25** fixed-point linear amplitude coefficient — i.e. the field is
`round(coeff · 2^25)`.

- Device **0 dB reference** = `0x016A09E6` = 23,725,030 → coeff ≈ **0.7071** (= 1/√2).
- Therefore:  **dB = 20·log₁₀(raw / 0x016A09E6)**  and  **raw = round(0x016A09E6 · 10^(dB/20))**.
- Verified: −10 dB = `0x00727C97` (÷3.162), −20 dB = `0x00243431` (÷10), −∞ = 0.

---

## 4. Worked example — set a fader

Captured "analogue-1 fader → 0 dB", two messages (DSP path + UI/meter path):

```
FF 6B 0A  06 00  01 00  0A 00  E6 09 6A 01  E0      ; index 0x0A (DSP coeff)
FF 6B 0A  06 00  01 00  28 00  E6 09 6A 01  FE      ; index 0x28 (UI/meter coeff)
```
Per-channel: `ParameterIndex` increments by 1 per analogue channel (ch1 DSP=0x0A, ch2=0x0B…);
the UI/meter twin sits at DSP index + 0x1E (0x0A→0x28). Build the full index map empirically by
toggling each control while capturing.

---

## 5. DSP message variants (value encodings)

All share the `MsgCode(u16) | Number(u16) | Index(u16)` header, then:

| Update msg (host→device) | Code | Trailing value |
|---|---|---|
| `bool update` / `bool coeff update` | 4 / 7 | 1 byte (0/1) |
| `selection update` / `selection coeff update` | 10 / 8 | u16 LE |
| `int update` / `int coeff update` | 16 / 18 | i32 LE |
| `Q6.25 update` / `Q6.25 coeff update` | 12 / 6 | i32 LE Q6.25 |

Device→host VALUE replies are richer — for Q-format, the reply body carries:
`Value(i32) InitialValue(i32) MinAllowedValue(i32) MaxAllowedValue(i32) InputValid(u8) OutputValid(u8)`.
This is how the device advertises current value **and** the legal range/defaults.

> **Coefficient vs Parameter:** "Parameter" items use `param` codes; "Coefficient" items use
> `coefficient` codes. Both serialize identically on the wire (number+index+value). The distinction
> is the ownership boundary described in §8.

## 6. SSL 12 control map

**`Number` = the control's integer.** `Index` selects channel/instance (valid sets listed).
The names below are descriptive labels for each observed control number.

### Parameters (use param codes)
| # | Name | Type | Valid indices | Meaning |
|---|---|---|---|---|
| 1 | INPUT_PHANTOM_POWER | bool | 0–3 | 48V per mic input |
| 2 | INPUT_HPF | bool | 0–3 | high-pass filter |
| 3 | INPUT_LINE_INPUT | bool | 0–3 | line/mic select |
| 4 | OUTPUT_BUS_PHASE_L | bool | 0 | |
| 5 | OUTPUT_BUS_MONO | bool | 0 | |
| 6 | OUTPUT_BUS_DIM | bool | 0 | |
| 7 | OUTPUT_BUS_CUT | bool | 0 | monitor cut/mute |
| 8 | OUTPUT_BUS_ALT | bool | 0 | alt speaker |
| 9 | OUTPUT_BUS_TALKBACK_ENABLE | bool | 0 | |
| 10 | OUTPUT_LEVEL | Q6.25 | 0,4,6 | monitor/phones level |
| 11 | INPUT_INSTRUMENT_INPUT | bool | 0,1 | Hi-Z on in 1/2 |
| 12 | SAMPLE_RATE | int | 0 | |
| 13 | CLOCK_SELECTION | selection | 0 | |
| 14 | CLOCK_VALID | bool | 0 | (read) |
| 15 | INPUT_POLARITY | bool | 0–11 | phase invert |

### Coefficients (use coefficient codes)
| # | Name | Type | Notes |
|---|---|---|---|
| 1 | MIXER_CROSSPOINT_TABLE | Q6.25 | **the monitor mixer matrix** (indices 0–239); the "faders" |
| 2 | OUTPUT_BUS_MONO | bool | indices 2,4,6 |
| 3 | OUTPUT_BUS_DIM_LEVEL | Q6.25 | |
| 4 | OUTPUT_BUSSES_CUT | bool | |
| 6 | OUTPUT_BUS_ALT_TRIM_LEVEL | Q6.25 | |
| 8 | TALKBACK_LEVEL | Q6.25 | idx 2–7 |
| 9 | OUTPUT_BUS_LEVEL | Q6.25 | idx 0,2,3,4,6 |
| 11 | LOOPBACK_SOURCE | selection | |
| 13 | STEREO_LINK_CHANNELS | bool | idx 0,2 |
| 14 | DISABLE_MIXER | bool | |
| 16 | HEADPHONES_GAIN_MODE | selection | |
| 26 | LINE_OUTPUT_OPERATING_LEVEL | selection | idx 0,2 (+4/-10) |
| 28 | OUTPUT_MONO_LEVEL | Q6.25 | |
| 31 | MUTE_HARDWARE_OUTPUTS | bool | **internal reconfigure-mute guard, not a user control** — see §9a |
| 32 | MONITOR_LEVEL_CONTROL | bool | idx 0–3 |

(Index sets are filled in empirically by toggling each control while capturing.)

### Selection value enums
- **CLOCK_SELECTION**: 0 INTERNAL, 1 ADAT.
- **LOOPBACK_SOURCE**: 0 OFF, 1 PLAYBACK_1_2, 2 PLAYBACK_3_4, 3 PLAYBACK_5_6,
  4 PLAYBACK_7_8, 5 OUTPUT_BUS_1_2, 6 OUTPUT_BUS_3_4, 7 OUTPUT_BUS_5_6, 8 OUTPUT_BUS_7_8.
- **LINE_OUTPUT_OPERATING_LEVEL**: 0 +9 dBu, 1 +24 dBu.
- **HEADPHONES_GAIN_MODE** ("impedance"): 0 STANDARD, 1 HIGH_SENSITIVITY, 2 HIGH_IMPEDANCE.
- **BUS_OUTPUT_MODE**: 0 STEREO, 1 BALANCED_MONO, 2 UNBALANCED_MONO.
- **SAMPLE_RATE** (param 12, INT): normally driven by the UAC2 clock (host audio stack); the DSP
  param is effectively a status/readback. `CLOCK_VALID` (param 14, bool) reports lock.

A reference Rust implementation of all of the above lives in this repository (`src/`).

**Worked example — 48V on mic input 1:** DSP msg `04 00 01 00 00 00 01`
(bool update, num 1, idx 0, val 1) wrapped:
`FF 6B 07 04 00 01 00 00 00 01 78` (CRC 0x6B+0x07+0x06 = 0x78).

## 7. Connection handshake & readiness

USB transport uses **send code 0x6B (107)**, **receive code 0x6C (108)** for DSP data.

**A tile-init handshake must run first (see §9a)** — without it the device parks the host in a
"reconnect required" state: it accepts control writes but withholds the meter stream. CONFIRMED on
Linux hardware (first bring-up): control worked but meters didn't until the tile init was added.

On connect, the version exchange:
1. host sends a protocol-version request (DSP code 1) and a DSP-version request (code 14);
2. device replies `protocol version reply` (2, = version 1) **and `DSP version reply`
   (15, = version 1)**. NOTE: the code-15 reply can arrive *slightly later* than code 2 (observed live:
   it was still queued on IN right after the client had already proceeded). So a robust client gates on
   **protocol version (code 2) only** and does not block waiting for code 15 — but the SSL 12 *does*
   send 15. (An earlier note here claimed it never does; that was a capture-window artifact.)
3. client validates protocol version == 1 (the SSL 12 reports `1`);
4. on success → the connection is marked **ready**.

**Until ready, all parameter/coefficient writes are dropped.** So a Linux client must do the tile-init
+ version exchange first — gate on **protocol version 1**; treat code 15 as optional/best-effort.
Device versions observed live: protocol 1, HW 4, SW int 0x00007e08 (§9a).

## 8. State model / reconciliation  ← (the "how is state kept in sync" answer)

**Ownership is split by control type.** (An earlier draft said flatly "the device is the source of
truth" — that's only half right, and the connect capture proves it.)

- **Parameters are device-authoritative.** They correspond to real hardware
  state — phantom supplies, line/Hi-Z relays, HPF, polarity, clock select, output levels, the
  per-bus switches. The device **retains** them and **reports its current values host-ward on
  connect** as a burst of `value replies` frames. Confirmed in the connect capture: ~71
  device→host VALUE frames covering phantom / HPF / line / Hi-Z / polarity / clock / sample-rate /
  output-level / bus switches. A client should **hydrate its UI from these VALUEs** (and keep
  listening for unsolicited VALUEs when a control changes). NB: in the app's capture that burst is
  provoked by its own per-param `value request`s — a passive Linux client must **pull** them
  the same way (see the "Pull" bullet below), not wait for an unsolicited dump.
- **Coefficients are host-authoritative.** The monitor mix
  (`MIXER_CROSSPOINT_TABLE`), plus `MUTE_HARDWARE_OUTPUTS` / `DISABLE_MIXER` / `TALKBACK_LEVEL` /
  `STEREO_LINK_CHANNELS` / `OUTPUT_MONO_LEVEL`, live in the device's **volatile** DSP coefficient
  RAM. **The device never reports them host-ward** — there are *zero* device→host crosspoint frames
  in the capture. The host **owns** them and **pushes its saved session on connect** (~2500
  `*_UPDATE_*` frames, including the full 240-cell crosspoint matrix). There is nothing to read
  back: a Linux client must **persist the mix itself** and push it on connect, or accept the
  device's power-on default.

So the protocol's Parameter-vs-Coefficient distinction *is* the ownership boundary.

The host keeps a *mirror* either way, keyed by `(Number, Index)`:

- **Host→device** on user action: `*_UPDATE_*` message (optimistic write).
- **Device→host**: `*_VALUE_*` message carrying current value + range + valid flags, routed by
  `(Number,Index)` to the bound control. These arrive as write confirmations **and** unsolicited on a
  physical change — **but only for parameters** (per above).
- **Pull**: a `value request` (code 3) for one item. NOTE: the USB-layer bulk dump
  `USB_REQUEST_CONTROL_STATES (0x2B)` is **not supported on the SSL 12** (it replies
  `invalid-command error`, 404). In practice a Linux client **must pull** the parameters it cares about
  with code-3 requests on connect: any unsolicited VALUE burst the device emits arrives *during* the
  version-exchange read loop (`device.rs::handshake`), which is looking for version replies and
  discards everything else — so a passive listener never sees it. `ssl12tui` pulls each input switch +
  monitor-bus param in `App::request_param_hydration`; the replies fold in via the normal pump loop.
- **On (re)connect**, per-control policy decides whether the host re-pushes its stored value or
  adopts the device's — which in practice means *adopt* for parameters, *re-push* for the coefficient
  mix.
- **Metering** is a separate device→host push: `METER_VALUE_TABLE_15BIT` (code 9) — see §8a.

**Standalone / no-host operation (why the box works without any of this).** Audio is class-compliant
UAC2, so the SSL 12 streams with no control host at all. Its DSP simply runs whatever coefficients
are in RAM: on cold power-up it loads a **firmware default mix** (so playback reaches the
monitors/headphones out of the box); while powered it holds the last values written (the default, or
the last host push, until overwritten or power-cycled). None of these controls touch non-volatile
memory, so a power cycle restores the firmware default — the SSL 360 app (or this client) just
**overrides** that default by pushing a saved mix on connect. That's also why "the device can't
remember a mix" and "the device works fine standalone" are both true: it has a built-in default, not
your saved one.

Practical consequence for the Linux client: **hydrate parameters from the device's connect dump**,
own + persist the **mix** yourself and push it, send UPDATEs optimistically, and keep **listening on
the IN endpoint** to reconcile parameter VALUEs (don't assume a write "stuck" — wait for the echo).

## 8a. Metering (device → host)  — SOLVED

The device continuously pushes meter levels via `METER_VALUE_TABLE_15BIT` (DSP code **9**), routed by
table number. **Device → host only** (the host never sends code 9).

**Message body** (DSP layer, after the `MsgCode=9` u16; all u16 LE):

```
offset  field         type        notes
0       MsgCode        u16         = 9 (METER_VALUE_TABLE_15BIT)
2       TableNumber    u16         SSL 12 = 1
4       TableOffset    u16         start index into the table (observed: 0)
6       TableSize      u16         count of samples that follow (observed: 29)
8..     samples[Size]  u16[] LE    one per meter index, starting at TableOffset
```

On the SSL 12, table 1 carries **29 meters**. In practice every frame carries the whole table
(offset 0, size 29). Confirmed across captures (one had 540 frames; plus the clip/input captures
below).

**Sample encoding** (each u16): **bits 0–14 = level** (linear, 0…32767 = 0x7FFF full scale);
**bit 15 (MSB) = over/clip flag.** Decode: `level = word & 0x7FFF`, `clip = word & 0x8000 != 0`.

There is **no dedicated clip/peak/overload message** in the DSP enums — clip is carried entirely by
this MSB. Verified with a real clipping capture: the MSB does get set on clipping channels, while a
near-full-scale-but-not-clipping channel (level 32322) leaves it clear. Per-sample check across that
capture: MSB set ⟺ level == 0x7FFF *exactly* (0 counter-examples), i.e. the device sets the bit on the
frame a sample saturates; it is **not observed as a sticky/hold latch** (does not persist after the
level drops). For a Linux client, `clip = MSB` is correct and equivalent to "level pinned at 0x7FFF
this frame." Levels are linear 15-bit; for a dB readout use `20·log₁₀(level / 32767)` (≈ dBFS) —
distinct from the Q6.25 *control* scale in §3.2.

**Meter index → channel map (table 1, 29 entries).** Derived empirically with
`capturedecode --meters` (peak/clip aggregate) and `--metertrace` (per-segment temporal
view), cross-referenced across four captures by set-intersection of which buses/inputs were fed:

| Index | Channel | How confirmed |
|------:|---------|---------------|
| 0  | Analogue input 1 | time-trace (lit 1st) |
| 1  | Analogue input 2 | time-trace (lit 2nd) |
| 2  | Analogue input 3 | time-trace (lit 3rd) |
| 3  | Analogue input 4 | position (silent — nothing plugged) |
| 4–11 | ADAT inputs 1–8 | by elimination (8 remaining input slots) |
| 12 / 13 | **Monitor / Main bus** L/R | lit only when Monitor fed |
| 14 / 15 | **Line 3–4 bus** L/R | lit only when Line 3–4 fed |
| 16 / 17 | **Headphone A** L/R | only pair lit in *both* HP-A captures |
| 18 / 19 | **Headphone B** L/R | lit only when HP B fed |
| 20 / 21 | **Playback 1–2** | hot whenever PB 1–2 was the source |
| 22–27 | Playback 3–8 returns | by elimination |
| 28 | **Talkback mic** | HW-confirmed: holding Talk moves meter 28 |

The bus block order (12–19) = **Monitor, Line 3–4, HP A, HP B** — identical to the crosspoint
*destination* order in §9b, a useful cross-check. The bus meters are **post-mix** (they follow the
live monitor mix, not raw input taps). The analogue/ADAT split, plus the bus and Playback-1–2 pairs,
are fully confirmed; the ADAT sub-order (4–11) and Playback sub-order (22–27) are by-elimination only
(low value — would each need a one-channel-at-a-time capture to pin).

## 9. Device identity & endpoints

**VID = 0x31E9 (12777)** (Solid State Logic).

**IMPORTANT (confirmed by `lsusb` on Linux): the SSL 12 enumerates as TWO SEPARATE USB DEVICES**, not
one composite:
- **`31e9:0024` "SSL Control I/F"** — **Full Speed (12 Mbps)**, `bcdDevice 10.00`, serial `S12-…`.
  One vendor-specific interface (class/subclass/protocol all 0xFF), interface number **0**, with bulk
  **IN 0x81** + **OUT 0x02** (64-byte max packet). **This is the device the control client drives.**
- **`31e9:0005` "SSL 12"** — **High Speed (480 Mbps)**, `bcdDevice 1.41`. The audio+MIDI function: a
  UAC2 IAD (AudioControl + 2 AudioStreaming + MIDIStreaming). All its interfaces are class 1 (Audio),
  so a 0xFF-interface scan correctly skips it. Audio is **fully class-compliant** (see §9d) → handled
  by the kernel's `snd-usb-audio`.

So the `0x0024` "control" PID is a *standalone* USB device, and the `0x0005` PID is the audio device.
The client's "scan all 0x31E9 devices for a 0xFF interface with bulk IN+OUT" approach lands on 0x0024
interface 0 automatically.

**Vendor-control endpoints (from capture pseudoheaders, via `capturedecode --endpoints`):**

| Endpoint | Dir | Type | Carries |
|---|---|---|---|
| `0x02` | OUT | BULK | host→device: all `0x6b` DSP data writes + USB-layer init codes |
| `0x81` | IN  | BULK | device→host: `0x6c` DSP data (VALUE replies **and** code-9 meter frames) |
| `0x80` | IN  | CTRL | EP0 control replies (occasional `0xff` frames) |

So the Linux client opens the vendor interface and uses **bulk OUT 0x02 / bulk IN 0x81**. (Note: IN is
`0x81`, not the `0x82` USB convention would suggest.) Still confirm the interface *number* with
`lsusb -v -d 31e9:` (and that the kernel UAC2 driver claims only the audio interface, leaving the
vendor interface free).

### 9a. Connect / init sequence (host → device, observed in capture)

On connect the host emits, all on bulk OUT 0x02 — the **tile-init prefix is mandatory for metering**
(and remember the FTDI **port-open of §0.5 happens first**):
`USB_GET_IS_TILE (0x01)` → `USB_GET_TILE_ID (0x02)` → **`USB_INIT_TILE (0x05)`** (all empty payload) →
`USB_GET_SOFTWARE_VERSION_INT (0x4b)` + `USB_GET_HW_VERSION (0x4e)` → meters begin streaming on IN →
one-time front-panel setup (`0x13` LED table ×36, `0x14` intensity, `0x1f` fader threshold) →
two no-target DSP data frames = DSP `request protocol version (1)` + `request DSP version (14)` →
the full **state push** (`0x6b` DSP data: crosspoints, switches, levels, selections — the host's stored
config dumped optimistically). Thereafter the host sends the **`0x1b` keepalive (§0.5 (3))** on a
timer for the life of the connection.

> **`MUTE_HARDWARE_OUTPUTS` (coeff 31) is a reconfigure-mute guard, not a user control.** In the
> connect capture the host wraps the bulk state push in it: `MUTE_HARDWARE_OUTPUTS = true`
> *before* the crosspoint/switch dump, then `= false` *after* the last coefficient — so rewriting all
> 240 crosspoints one at a time doesn't pop/zipper the outputs. (Observed: `true` at the start of each
> config block, a single `false` at the very end.) This is why the SSL 360 app exposes no "mute
> everything" button. A client doing its own bulk push should wrap it the same way; the reference TUI's
> `push_full_mix` does. It's independent of any per-bus cut/dim.

> **Correction:** an earlier draft listed `0x1b ×34` as part of "button-LED setup … cosmetic and not
> required." That was wrong. `0x1b` is the **keepalive** (§0.5 (3)) and is **required** — it is *not*
> LED setup, and it recurs for the whole session rather than being a one-time connect burst. The
> genuinely one-time/cosmetic frames are `0x13` (LED table) / `0x14` (intensity) / `0x1f` (fader
> threshold).

**Tile init is REQUIRED for the meter stream (confirmed on Linux hardware).** If the host skips
`USB_GET_IS_TILE`/`USB_GET_TILE_ID`/`USB_INIT_TILE`, the device:
- still **accepts control writes** (phantom/mix/mute all work), but
- **does NOT push meters**, and emits **`USB_RECONNECT_REQUIRED` (code 6)** on IN to signal the
  connection is incomplete.
Sending the three empty-payload tile commands (the client now does this in `handshake()`) clears the
reconnect state and starts the meter push. The LED/fader-threshold setup (`0x13`/`0x14`/`0x1f`) is
cosmetic and not required — but the **`0x1b` keepalive (§0.5) IS required** to keep the connection
alive past ~5 s.

**Device answers on bulk IN 0x81 (captured via `capturedecode --rawframes`):**

| Reply | Bytes | Meaning |
|---|---|---|
| DSP `protocol version reply` (code 2) | `01 00 00 00` | **protocol version = 1** |
| `USB_GET_HW_VERSION` (0x4e) | `04 00` | **HW version = 4** |
| `USB_GET_SOFTWARE_VERSION_INT` (0x4b) | `08 7e 00 00` | SW/firmware int = **0x00007e08** |
| DSP `DSP version reply` (code 15) | `01 00 00 00` | **DSP version = 1** (arrives slightly later — see below) |
| INIT_TILE acks (codes 0x01/0x02/0x04/0x05) | e.g. `05 → 00` | tile/serial handshake (`05 00` = OK) |

**Handshake timing note:** the SSL 12 *does* send `DSP version reply` (code 15), but it can
arrive **slightly after** `protocol version reply` (code 2) — on Linux the live `info` read showed the
code-15 frame (`0f 00 01 00 00 00`) still queued right after the client had already proceeded. (An
earlier draft of this spec wrongly said "never sends 15" based on a capture where it just fell
outside the captured window.) Practical guidance is unchanged: **gate readiness on protocol version
(code 2) == 1 only**; treat code 15 as best-effort, since blocking on it risks a timing-dependent hang.
After the version exchange the device streams its full state on 0x81 (bool/Q6.25/int VALUE + meters) —
reconcile against that. (Int-VALUE quad confirmed: SAMPLE_RATE value/initial/min/max =
48000/48000/44100/192000.) CRC caveat: device→host **meter** frames (code 9) fail our additive CRC
check (the meter values decode fine regardless) — non-meter VALUE frames pass; treat meter-frame CRC
as don't-care for now.

### 9c. Selection enum value maps (CONFIRMED)

`*_SELECTION` controls carry a `u16` index into these lists (all exercised in the connect state push):

| Control (DSP #) | Values |
|---|---|
| `CLOCK_SELECTION` (13) | 0 = INTERNAL · 1 = ADAT |
| `BUS_OUTPUT_MODE` (10) | 0 = STEREO · 1 = BALANCED_MONO · 2 = UNBALANCED_MONO |
| `HEADPHONES_GAIN_MODE` (16) | 0 = STANDARD · 1 = HIGH_SENSITIVITY · 2 = HIGH_IMPEDANCE |
| `LINE_OUTPUT_OPERATING_LEVEL` (26) | 0 = +9 dBu · 1 = +24 dBu |
| `LOOPBACK_SOURCE` (11) | 0 = OFF · 1–4 = PLAYBACK_1_2/3_4/5_6/7_8 · 5–8 = OUTPUT_BUS_1_2/3_4/5_6/7_8 (= Monitor / Line 3-4 / HP A / HP B) |

The `LOOPBACK_SOURCE` output-bus order (Monitor, Line 3-4, HP A, HP B) independently matches the
meter and crosspoint bus order — another cross-check.

### 9d. Audio streaming = standard UAC2 (project scope goal — CONFIRMED)

`lsusb -v` on the `31e9:0005` device shows a textbook **USB Audio Class 2.0** function (IAD, bcdADC
2.00), so audio I/O is handled by the kernel's `snd-usb-audio` — no custom driver needed. Highlights:
- **Playback**: USB-streaming IN terminal (ID 2) → 8 channels, 32-bit/4-byte PCM, async iso EP `0x01`.
- **Capture**: Microphone IN terminal (ID 1) → up to 16 channels, 32-bit PCM, iso EP `0x81`; capture
  AS interface offers alt settings for 16 / 12 / 10 channels (channel-count vs sample-rate trade-off).
- **Clock**: internal programmable clock (ID 41) + external clock (ID 43) via a clock selector (ID 40)
  — this is the UAC2 view of `CLOCK_SELECTION` (internal vs ADAT).
- Also exposes a standard **USB-MIDI** interface (bulk EP `0x02`/`0x83`).

So the project's "verify streaming is class-compliant" question is answered **yes** — only the vendor
*control* protocol (this document) needs a userspace implementation; streaming is kernel-handled.

---

## 9b. Mixer model & derived controls (pan / solo / cut / fader / stereo link)

The SSL 12's onboard DSP is a **flat gain matrix + hardware toggles**, nothing more. The device
exposes (a) per-input hardware switches, (b) per-output-bus controls, and (c) a 240-cell
**crosspoint gain table** (`MIXER_CROSSPOINT_TABLE`, Q6.25 per cell). Confirmed by capture: each
crosspoint **index** is one gain cell; the host writes a coefficient to it. The device has **no
concept of "pan", "solo", "channel fader", or "balance".**

Those are **computed host-side** (pan / balance / solo / cut / fader logic) and collapsed into
crosspoint gains:
- **Channel fader / level** → scales that source's crosspoint cells.
- **Pan / balance** → distributes a source's gain across its L vs R destination cells per a pan law.
- **Solo** → attenuates/zeroes all *other* sources feeding the monitored bus.
- **Cut (channel)** → zeroes that source's crosspoint cells.
So to reproduce them on Linux you implement the same math and write the resulting cell gains;
there is no shortcut message.

**"Follow mix 1-2"** (HP A / HP B / Line 3-4 "follow" buttons) = `OUTPUT_BUSES_FOLLOW_1_2`
(coeff 7, bool, per output bus, idx 2/3/4/6). When set, the device mirrors the main monitor mix
(bus 1-2) onto that bus; the host then writes no independent crosspoints for it.

**"Direct to bus"** (playback 1-2→Mon, 3-4→Line 3-4, 5-6→HP A, 7-8→HP B) = **not a separate
control** — implemented via the crosspoint table (no separate enum item). It routes a playback pair
1:1 to its bus by writing cells in `MIXER_CROSSPOINT_TABLE`. Reproduce on Linux by writing those
cells; nothing extra.

**Stereo link / bonding** (`STEREO_LINK_CHANNELS`, coeff 13, bool, idx 0 & 2): a device-side flag
marking an input pair as linked. Practically the *host* keeps the pair's fader/processing in lock-step
and still writes **individual** crosspoint gains for both channels; hardware toggles that are
inherently per-pair (48V, line/Hi-Z) are applied to both. The coeff lets the DSP track the pair where
it matters (e.g. gain). (Matches the capture notes: linked pairs send a single toggle to the
master but individual fader packets.)

**Per-input switches** (all bool unless noted), index = channel:
phase invert = `INPUT_POLARITY` (15, idx 0–11), HPF = `INPUT_HPF` (2, idx 0–3),
line/mic = `INPUT_LINE_INPUT` (3), Hi-Z = `INPUT_INSTRUMENT_INPUT` (11, idx 0–1), 48V =
`INPUT_PHANTOM_POWER` (1).

**Per-output-bus controls:** main cut = `OUTPUT_BUS_CUT` (param 7) / per-bus `OUTPUT_BUSSES_CUT`
(coeff 4); dim on/off = `OUTPUT_BUS_DIM` (param 6) + amount `OUTPUT_BUS_DIM_LEVEL` (coeff 3, Q6.25);
mono = `OUTPUT_BUS_MONO`; talkback = `OUTPUT_BUS_TALKBACK_ENABLE` (param 9) + `TALKBACK_LEVEL`
(coeff 8); bus/headphone level = `OUTPUT_BUS_LEVEL` (coeff 9, idx 0,2,3,4,6) and `OUTPUT_LEVEL`
(param 10, idx 0,4,6). **Headphone mixes and the Line 3–4 output are simply additional destination
buses** in the crosspoint matrix, each with its own level/mode controls.

**Matrix geometry (derived from captures via `capturedecode`):** the 240 cells form
**8 destination blocks × 30 source-slots**, i.e. `index = destination*30 + source_slot`
(destinations stride by 30: 0, 30, 60 … 210). Evidence: fading one source writes that source's
slot in every destination block — indices 8, 38, 68, 98, 128, 158, 188, 218 (slot 8, all 8
destinations), with the unused destinations driven to `-inf`. Stereo sources occupy adjacent slots
(`slot`, `slot+1`); a stereo-linked move also sets `STEREO_LINK_CHANNELS`. The earlier "DSP vs UI
twin" idea (idx 10 & 40) was actually **destination 0 vs destination 1**, same source slot 10.

**SOLVED (from three routing captures):**
Destination blocks (L/R per stereo output): `Main = 0/1 · Line 3-4 = 2/3 · HP A = 4/5 · HP B = 6/7`.
Source slots: `Playback 1-2 = 0/1 · Playback 3-4 = 2/3 · Playback 5-6 = 4/5 · Playback 7-8 = 6/7 ·
Analogue 1/2/3/4 = 8/9/10/11` (analogues mono — one slot fed to both L/R blocks at the pan position;
stereo playbacks L→L block, R→R block). **Talkback is not a crosspoint source** — its fader writes
`TALKBACK_LEVEL` (coeff 8), a separate monitor-bus injection. Encoded with tests in `src/mixer.rs`.

**Fader-law caveat:** the SSL 360 fader dB ≠ the crosspoint coefficient dB. For stereo playbacks the
offset is ≈ **+3.01 dB** (consistent across captures); a centered **mono** source has **no** offset —
its fader 0 dB writes the device 0 dB reference coefficient (the §4 "analogue-1 fader → 0 dB" example
confirms this). Identifying strips by exact dB is unreliable — use indices. Raw control doesn't need
the law. _Client status:_ `src/mixer.rs` implements this linear region as a single
`DEVICE_REF_OFFSET_DB` (+3.01) plus a per-leg pan law (`fader_pan_to_leg_coeffs` /
`leg_coeffs_to_fader_pan`), so the TUI mixer displays SSL-360-accurate fader dB and per-cell pan. The
*piecewise* fader detail near the bottom of the throw — and the exact pan curve — are still uncaptured.

"Direct to bus" confirmed = crosspoint bypass: unity (+3.01 dB) to the target output's blocks, `-inf`
to that source in all other blocks.

## 10. Open items / TODO

- [x] Bulk IN/OUT endpoint addresses + interface — **SOLVED & HW-CONFIRMED** (§9): control is a
      *separate* Full-Speed device `31e9:0024`, vendor iface **#0**, bulk OUT `0x02` / IN `0x81`.
- [x] `MIXER_CROSSPOINT_TABLE` index→(source,destination) layout — **SOLVED**, see §9b
      (`index = destination*30 + source_slot`).
- [x] Metering format + meter index→channel map — **SOLVED**, see §8a.
- [x] Selection enum value maps — **SOLVED** (all confirmed), see §9c.
- [x] Device version answers — **SOLVED & HW-CONFIRMED** (§9a): protocol 1, HW 4, SW 0x00007e08;
      code 15 (DSP version 1) *is* sent but can lag code 2 — gate on protocol 1.
- [x] Audio class-compliance — **CONFIRMED** via `lsusb` (§9d): standard UAC2, kernel-handled.
- [x] **FTDI transport layer** — **SOLVED & HW-CONFIRMED** (§0.5): the control pipe is an FTDI UART
      bridge needing port-open control transfers + per-packet 2-byte status stripping + the `0x1b`
      keepalive. This was the missing piece behind "control works but meters don't / drop after 5 s."
- [x] **Tile-init required for metering** — **SOLVED & HW-CONFIRMED** (§9a): GET_IS_TILE/GET_TILE_ID/
      INIT_TILE (empty payloads) before the version exchange, else device emits USB_RECONNECT_REQUIRED
      and withholds meters.
- [x] Meter idx 28 = **talkback mic** — HW-confirmed (holding Talk moves meter 28).
- [ ] Meter sub-orders (ADAT 4–11, Playback 22–27) — by-elimination only; low value.
- [~] Fader taper: linear region implemented in the client (mono offset 0 / stereo +3.01 dB, both
      capture-verified). Still open: the taper is piecewise near the bottom of the throw — would need a
      capture sweep to pin its breakpoints exactly (raw protocol works without it).
