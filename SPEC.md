# EzanBox: spec

A small e-ink prayer timer. It shows the next prayer and the time left until it, uses Diyanet times for the selected city, keeps its clock in that city's timezone, and plays the adhan from a Turkish imam at each prayer time.

Hardware: **Waveshare ESP32-S3-ePaper-1.54** (V2: ESP32-S3-PICO-1-N8R8, 8 MB flash, 8 MB PSRAM). It was detected on this Mac as `/dev/cu.usbmodem101` (Espressif USB-Serial-JTAG, VID 303a / PID 1001).

---

## 1. Goals

| # | Requirement | How |
|---|---|---|
| 1 | Pick from a list of cities, one well-known city per timezone | A curated `cities.toml` (~33 cities, one per UTC offset), picked on the device or in the setup web page |
| 2 | Clock follows the city's time automatically | SNTP over Wi-Fi gives UTC. Each city has a POSIX TZ string (DST included). The PCF85063 RTC keeps time when offline |
| 3 | Show the next prayer and the hours and minutes left | Home screen with a large countdown, refreshed every minute with a partial e-ink refresh |
| 4 | Minimal, modern UI | 1-bit typographic layout, one large number, plenty of white space, no icons that don't carry meaning |
| 5 | Diyanet prayer times | Monthly Diyanet data, fetched once and cached on the device for about 30 days |
| 6 | Adhan by a Turkish imam | Your MP3s are converted at build time and stored in flash. They play through the onboard ES8311 codec and the speaker header |
| 7 | Rust everywhere | Firmware, host tools and UI simulator are all Rust. The only non-Rust code is ESP-IDF itself, which provides the Wi-Fi/BLE blobs and is linked in as a library |

Out of scope for v1: voice interaction, BLE, Qibla direction, multiple simultaneous cities.

---

## 2. Hardware map (V2 board)

From Waveshare's own V2 ESP-IDF examples (`epaper_config.h` and the `board_cfg.txt` audio entry `S3_ePaper_1_54`):

| Function | Part | Pins / address |
|---|---|---|
| E-paper 1.54" 200×200 | SSD1681 controller (1.54" V2 panel), SPI2 | DC 10, CS 11, SCK 12, MOSI 13, RST 9, BUSY 8 |
| E-paper power | load switch | GPIO 6, **active low** |
| Audio codec | ES8311 (I²C 0x18) | I²S: MCLK 14, BCLK 15, WS 38, DOUT 45, DIN 16 |
| Speaker amp | PA enable | GPIO 46 (active high) |
| Audio power | load switch | GPIO 42, **active low** |
| I²C bus | | SDA 47, SCL 48 |
| RTC | PCF85063A | 0x51, INT → GPIO 5 (wake source) |
| Temp/humidity | SHTC3 | 0x70 |
| TF card | SDMMC 1-bit | CLK 39, CMD 41, D0 40 |
| Buttons | BOOT, PWR | GPIO 0, GPIO 18 (both can wake from sleep) |
| Battery power latch | | GPIO 17, drive **high** to stay on when on battery |
| Battery voltage | ADC1 ch 3 | |
| LED | | GPIO 3 |
| Speaker | MX1.25 2-pin header | speaker not included; any 4–8 Ω, 1–3 W speaker |

Milestone 0 will confirm the board revision with `espflash board-info`. V1 has 4 MB flash and different pins.

---

## 3. Architecture

### 3.1 Rust stack

**Primary choice: `esp-idf-svc` (Rust std on top of ESP-IDF 5.x).**

- Wi-Fi, HTTPS (mbedTLS plus the certificate bundle), SNTP, NVS, the HTTP server for the setup portal, the I²S std driver, SDMMC/FAT and sleep are all mature and exposed through safe Rust wrappers.
- All application code is Rust. ESP-IDF is the C runtime underneath, much like libc on a desktop OS.

Alternative considered: `esp-hal` + `embassy` (bare-metal `no_std`, pure Rust). It's lighter and more "all Rust", but HTTPS with TLS, the captive portal and FAT are noticeably less mature. To keep this option open, the logic lives in a `no_std` `core` crate, so only the board layer would need a rewrite.

### 3.2 Workspace layout

```
ezanbox/
├── Cargo.toml              workspace
├── SPEC.md
├── cities.toml             curated city list (source of truth)
├── assets/
│   ├── ezan/               your MP3s go here (see §7)
│   └── fonts/              TTF sources for build-time font rasterizing
├── core/                   #![no_std] pure logic, unit-tested on the Mac
│   ├── schedule.rs         next prayer, countdown, day rollover
│   ├── prayer.rs           PrayerTimes { imsak, gunes, ogle, ikindi, aksam, yatsi }
│   ├── cities.rs           generated table: name, tz, diyanet_id
│   ├── ui/                 screens drawn with embedded-graphics (DrawTarget-generic)
│   └── strings.rs          UI labels (English only for now)
├── firmware/               ESP32-S3 binary (target xtensa-esp32s3-espidf)
│   ├── board.rs            pins, power rails, latch
│   ├── epd.rs              SSD1681 driver (full and partial refresh)
│   ├── audio/es8311.rs     codec register init (ported from esp_codec_dev)
│   ├── audio/player.rs     MP3 (nanomp3) → PCM → I²S, volume, stop on button
│   ├── net/wifi.rs         STA connect, retry, backoff
│   ├── net/portal.rs       SoftAP + captive portal (Wi-Fi + city form)
│   ├── net/diyanet.rs      fetch + parse monthly times
│   ├── time.rs             SNTP, TZ, PCF85063 sync
│   ├── store.rs            NVS settings + prayer-time cache
│   ├── buttons.rs          debounce, short/long press events
│   └── main.rs             event loop / state machine
├── simulator/              embedded-graphics-simulator: run the real UI on the Mac
└── xtask/                  host tools (cargo xtask …)
    ├── cities              resolve Diyanet IDs for cities.toml → core/cities.rs
    ├── audio               MP3s → trimmed, normalized 32 kbps mono MP3 pack (audio.bin)
    ├── flash               build + flash app + audio partition
    ├── setup               send Wi-Fi / city over USB serial
    └── reset               clear settings (or --all) over USB serial
```

`core/ui` draws into any `embedded_graphics::DrawTarget`. The same screen code renders on the panel and in a desktop window, so UI work on the Mac doesn't require reflashing.

### 3.3 Runtime model

A single main loop, with tasks only where a peripheral needs one:

```
 boot ─► power latch on ─► load settings (NVS)
        │
        ├─ no Wi-Fi creds ─► Setup mode (SoftAP + portal + QR on screen)
        │
        └─► RTC → system time ─► render Home (full refresh)
                 │
                 loop: wake every minute boundary (or on button / prayer time)
                   • recompute next prayer + countdown → partial refresh
                   • at prayer time → Adhan screen + play audio
                   • once a day (03:00 local, or when cache < 7 days) → Wi-Fi on:
                        SNTP → set RTC, refresh Diyanet cache → Wi-Fi off
```

Wi-Fi stays off except during the daily sync. On USB power the device uses light sleep between minute ticks. On battery it can deep sleep, waking from the RTC alarm or a button (milestone 6).

---

## 4. Data

### 4.1 Prayer times: Diyanet

**Source A (default): `https://ezanvakti.emushaf.net`.** This is an unofficial, auth-free mirror of Diyanet's data.

- `GET /vakitler/{ilceId}` returns about 32 days of data (checked: it doesn't necessarily start today, so the device filters by date).
- Fields used: `MiladiTarihKisa` ("16.11.2026"), `Imsak`, `Gunes`, `Ogle`, `Ikindi`, `Aksam`, `Yatsi` ("HH:mm", local wall time for the city), and `HicriTarihUzun` ("4 Cemaziyelahir 1448") for the Hijri line.
- Caveat: the `…Iso8601` fields always carry +03:00, even for cities outside Turkey. We ignore them and treat `date + HH:mm` as wall time in the city's TZ.

**Source B (optional, official): `https://awqatsalah.diyanet.gov.tr`.**

- Requires applying with a form to get credentials. It uses a JWT valid for 45 minutes and a refresh token valid for 7 days, with a quota of about 10 calls per endpoint per parameter.
- Endpoints: `/api/PrayerTime/Monthly/{cityId}`, `/api/Place/...`
- Built behind the same `PrayerSource` trait, so switching sources is a setting rather than a rewrite. Credentials are entered in the portal.

**Cache:** each day is `date + 6 × u16 minutes-since-midnight`, about 14 bytes per day, so 40 days fit in under 1 KB of NVS. If both sources and Wi-Fi are unavailable, the device keeps working from the cache. With fewer than 3 days left it shows a small "⟳ times expiring" marker.

### 4.2 City list: one per timezone

One well-known city per UTC offset, skipping uninhabited or tiny zones. `cargo xtask cities` resolves each city to its Diyanet ID through the `/ulkeler → /sehirler → /ilceler` chain and fails loudly if a city isn't covered. Then we pick the nearest covered city.

List order is fixed: **İstanbul first, Berlin second** (the default city), then every other city by UTC offset, west to east. UTC−3:30 (St. John's) is left out because Diyanet has no entry for Newfoundland.

| # | UTC | City | IANA zone |
|---|---|---|---|
| 1 | +3 | İstanbul | Europe/Istanbul |
| 2 | +1 | **Berlin** (default) | Europe/Berlin |
| 3 | −10 | Honolulu | Pacific/Honolulu |
| 4 | −9 | Anchorage | America/Anchorage |
| 5 | −8 | Los Angeles | America/Los_Angeles |
| 6 | −7 | Denver | America/Denver |
| 7 | −6 | Chicago | America/Chicago |
| 8 | −5 | New York | America/New_York |
| 9 | −4 | Santiago | America/Santiago |
| 10 | −3 | São Paulo | America/Sao_Paulo |
| 11 | −2 | Nuuk | America/Nuuk |
| 12 | −1 | Praia | Atlantic/Cape_Verde |
| 13 | 0 | London | Europe/London |
| 14 | +2 | Cairo | Africa/Cairo |
| 15 | +3:30 | Tehran | Asia/Tehran |
| 16 | +4 | Dubai | Asia/Dubai |
| 17 | +4:30 | Kabul | Asia/Kabul |
| 18 | +5 | Karachi | Asia/Karachi |
| 19 | +5:30 | New Delhi | Asia/Kolkata |
| 20 | +5:45 | Kathmandu | Asia/Kathmandu |
| 21 | +6 | Dhaka | Asia/Dhaka |
| 22 | +6:30 | Yangon | Asia/Yangon |
| 23 | +7 | Jakarta | Asia/Jakarta |
| 24 | +8 | Kuala Lumpur | Asia/Kuala_Lumpur |
| 25 | +9 | Tokyo | Asia/Tokyo |
| 26 | +9:30 | Adelaide | Australia/Adelaide |
| 27 | +10 | Sydney | Australia/Sydney |
| 28 | +12 | Auckland | Pacific/Auckland |

Offsets are standard time; DST comes from the TZ rules. Cities 3 onward are sorted by standard offset, even where DST shifts them (London, New York…). `cities.toml` is the single source of truth, and its row order is the list order: adding a row and re-running `xtask cities` is all it takes to add a city (for example Mecca, Ankara or Amsterdam).

Each entry stores `name_tr`, `name_en`, `tz_posix` (for example `"<+03>-3"` or `"CET-1CEST,M3.5.0,M10.5.0/3"`, generated from IANA at build time) and `diyanet_id`.

### 4.3 Time sync

1. Boot: read the PCF85063, set the system clock and apply `TZ=<city tz_posix>`.
2. Daily, or after a city change: Wi-Fi → SNTP (`pool.ntp.org`, `time.google.com`) → write UTC to the RTC.
3. Changing city only changes the TZ string and the prayer-time cache. UTC never changes, so the clock is right immediately and the times are refetched.

---

## 5. UX

### 5.1 Principles

- **One glance, one answer:** the largest thing on screen is always the time left until the next prayer.
- English UI for now. Prayer names: Fajr, Sunrise, Dhuhr, Asr, Maghrib, Isha, mapped from Diyanet's İmsak, Güneş, Öğle, İkindi, Akşam, Yatsı. All strings live in one `strings.rs`, so adding Turkish later is a small change.
- Black on white. Inversion (white on black) is reserved for "this is the one that matters" (the next prayer row) and for the adhan screen.
- Fonts: bitmap fonts from u8g2 (Logisoso for the countdown, Free Universal for labels, Helvetica for the times grid). Glyphs cover Latin Extended (İ ı Ş ş Ğ ğ Ç ç Ö ö Ü ü), so city names like İstanbul render correctly and a Turkish UI needs no font work later. Sizes: 64 px numerals, 20 px labels, 14 px small text.

### 5.2 Home screen (200 × 200)

```
┌──────────────────────────────┐
│ BERLIN            Mon 16 Nov │  city · date
│                              │
│        1 3 : 2 8             │  current time, large, centered
│                              │
│     Asr · 2h 14m left        │  next prayer · time left
│ ──────────────────────────── │
│ Fajr    05:29  Asr     13:54 │
│ Sunrise 07:22 ▌Maghrib 16:20▐│  next prayer row inverted
│ Dhuhr   11:56  Isha    18:00 │
│ 7 Jumada al-Akhirah 1448  ▮▮ │  Hijri date · status icons
└──────────────────────────────┘
```

Time left: `2h 14m left`, `39m left`, and `now` in the last minute. After Isha it counts to tomorrow's Fajr, and the grid shows tomorrow's times. Sunrise is shown and counted down to, but has no adhan.

Refresh policy: a partial refresh at each minute boundary (about 0.3 s, no flash), and a full refresh every hour and on every screen change to clear ghosting.

### 5.3 Adhan screen

At the prayer minute the whole screen inverts:

```
┌──────────────────────────────┐
│██████████████████████████████│
│██                          ██│
│██        ASR               ██│
│██        15:42             ██│
│██                          ██│
│██   adhan playing          ██│
│██   any button: stop       ██│
│██████████████████████████████│
└──────────────────────────────┘
```

The adhan plays from start to finish. Pressing either button stops it, and the screen returns to Home with a full refresh.

### 5.4 Buttons

There are two side buttons, named by position. The **top** button is GPIO0 (silkscreen BOOT). The **bottom** button is GPIO18, the board's power key, and is never used for holds because holding it can trigger the power circuit.

| Screen | Top press | Bottom press | Hold top (0.8 s) |
|---|---|---|---|
| Home | open menu | open menu | — |
| Lists (menu, city, adhan, Wi-Fi, reset) | up | down | select |
| Volume | louder | quieter | done |
| Confirm screens | cancel | cancel | confirm |
| Adhan playing | stop | stop | stop |

- Lists wrap around. Each list has a **Back** row; in the city list, holding top on the current city goes back.
- The hint bar shows "▲ top ▼ bottom" on the left and what holding top does on the right.
- Hold bottom for 4 s: power off.
- Hold top for 10 s anywhere: emergency reset. A bar appears after 5 s, and it still asks to confirm.

### 5.5 Menu

```
  City           Berlin    ›
  Adhan          On        ›   (per-prayer on/off)
  Volume         ■■■■□□    ›
  Test adhan               ›   (plays the next prayer's adhan, as at prayer time)
  Back
  Wi-Fi          HomeNet   ›   (status, set up again, forget)
  About                    ›   (version, last sync, battery, temp/humidity)
  Reset                    ›   (see §5.7)
```

- **City picker:** a scrolling list of 6 rows, in the order from §4.2 (İstanbul, Berlin, then by offset), showing `UTC+1  Berlin` with the current city marked ●. BOOT moves down, long BOOT moves up, PWR selects. Selecting saves to NVS, applies the TZ, fetches new times (showing "Fetching times…") and returns to Home.
- **Adhan:** toggles for Fajr, Dhuhr, Asr, Maghrib, Isha (all on by default), plus **Test** to play 10 s of each recording.
- **Volume:** 6 steps. Changing the volume plays a 2 s preview of the adhan.

### 5.6 Wi-Fi connection

The device has no keyboard, so Wi-Fi credentials come from your phone or your Mac. There are three ways in, all ending in the same NVS keys (`wifi_ssid`, `wifi_pass`).

**A. Phone setup (default, no computer needed).** This runs on first boot, or from Menu → Wi-Fi → Set up.

1. The device starts its own open Wi-Fi network, `EzanBox-XXXX` (the last 4 hex digits of the MAC).
2. The screen shows:
   ```
   ┌──────────────────────────────┐
   │ Setup                        │
   │ ▄▄▄▄▄▄▄  1. Scan to join     │
   │ █ QR  █     EzanBox-3C1A     │
   │ █     █  2. A page opens,    │
   │ ▀▀▀▀▀▀▀     pick your Wi-Fi  │
   │                              │
   │ or open 192.168.4.1          │
   └──────────────────────────────┘
   ```
   The QR code uses the standard `WIFI:T:nopass;S:EzanBox-3C1A;;` format, so the iOS and Android cameras join it with one tap.
3. The phone joins. The device runs a tiny DNS server that answers every lookup with itself, so the OS pops its "Sign in to network" sheet (a captive portal) automatically.
4. That page has one form: a network dropdown (from a scan the device ran just before starting the AP), a password field, a city dropdown and a **Save** button.
5. The device tries to connect while the phone is still attached. Both the page and the screen show the result:
   - Success: "Connected ✓", then the AP shuts down and the device syncs time and fetches prayer times.
   - Wrong password or no internet: the page shows the error and the form again. Nothing is saved until a connection succeeds.

**B. USB setup from the Mac (handy during development).**

- `cargo xtask setup --ssid "HomeNet" --pass "…" --city berlin` sends a one-line command over the USB serial port.
- The firmware's serial console parses it, writes NVS and reconnects. No reflash needed.

**C. Build-time defaults (development only).** If `EZANBOX_WIFI_SSID` and `EZANBOX_WIFI_PASS` are set when building, they're compiled in as the fallback when NVS is empty. Release builds don't use this.

**Daily behaviour.** Wi-Fi is off most of the time. Once a day (and after a city change) the device connects with a 20 s timeout, syncs time, refreshes the prayer-time cache and disconnects.

- If that fails, it retries with backoff (5 min, 30 min, 2 h, then daily). The clock and prayer times keep working from the RTC and the cache.
- After 3 days without a successful sync, a small "no Wi-Fi" mark appears on Home.
- Only once the cache runs out does the screen ask you to run setup again.

**Changing networks.** Menu → Wi-Fi shows the current network and the last sync, with **Set up** (restarts flow A) and **Forget** (clears the credentials).

One network is stored in v1. Only 2.4 GHz networks work, because the ESP32-S3 has no 5 GHz radio. The portal page says so if no networks are found.

The city can also be changed later from the device menu, without the phone.

### 5.7 Setup and reset, step by step

Every setup screen shows its step number (`1/5`) and what each button does on the bottom line, so you never have to guess. BOOT always means "back / cancel" on these screens and PWR always means "next / confirm".

#### First-time setup

**Step 0: power on.** Plug in USB-C, or hold PWR for 3 s on battery.

```
┌──────────────────────────────┐
│                              │
│          EzanBox             │
│                              │
│   Prayer times & adhan       │
│                              │
│                              │
│              PWR: start  →   │
└──────────────────────────────┘
```

**Step 1/5: connect your phone.** Pressing PWR starts the setup network, and the screen from §5.6 appears: a QR code for `EzanBox-3C1A`, with `192.168.4.1` as a fallback. The screen updates on its own as you go:

| What you do on the phone | What the device screen shows |
|---|---|
| Scan the QR code, join `EzanBox-3C1A` | "Phone connected ✓ — opening setup page…" |
| The setup page opens (or you open 192.168.4.1) | "Fill in the page on your phone" |

**Step 2/5: Wi-Fi (on the phone page).** Pick your network from the list (strongest first, 2.4 GHz only), type the password and tap **Connect**.

- The device screen shows "Connecting to HomeNet…" with a spinner made of three dots.
- **Success:** the phone shows "Connected ✓" and moves on to step 3.
- **Wrong password:** both the phone and the device say "Wrong password, try again", and the form stays filled in except for the password.
- **No internet** (Wi-Fi joined but the internet unreachable): "Connected to HomeNet but no internet. Check your router" with **Retry** and **Use anyway**. "Use anyway" works because the clock chip and cached times keep running.

**Step 3/5: city (on the phone page).** A dropdown of the cities in the same order (İstanbul, Berlin, then by offset), with Berlin preselected. Tap **Save**.

- The page then says "All set. You can disconnect your phone." The device turns its setup network off a few seconds later.
- If you close the page before this step, the device falls back to choosing the city on the device itself, using the same picker as Menu → City.

**Step 4/5: sync (on the device, automatic).**

```
┌──────────────────────────────┐
│ Setting up            4/5    │
│                              │
│  ✓ Wi-Fi     HomeNet         │
│  ✓ Clock     13:24  UTC+1    │
│  … Prayer times  Berlin      │
│                              │
│                              │
└──────────────────────────────┘
```

Each line turns into ✓ as it finishes. If the prayer-times fetch fails, it shows "✗ Prayer times: couldn't reach Diyanet" with "PWR: retry · BOOT: skip". If you skip, the device retries in the background and Home shows "Waiting for prayer times".

**Step 5/5: speaker check.**

```
┌──────────────────────────────┐
│ Speaker check         5/5    │
│                              │
│  Playing adhan sample…       │
│                              │
│  Volume  ■■■■□□              │
│                              │
│ BOOT: quieter    PWR: done ✓ │
└──────────────────────────────┘
```

It plays 10 s of the Dhuhr adhan on a loop. BOOT steps the volume down and wraps around from lowest to loudest, and PWR saves the volume. If you hear nothing, check the speaker plug on the MX1.25 header.

**Done:** "You're set. Next: Asr at 15:42" for 3 s, then Home.

#### Changing Wi-Fi

Menu → Wi-Fi shows:

```
┌──────────────────────────────┐
│ Wi-Fi                        │
│  Network   HomeNet           │
│  Signal    ▂▄▆_              │
│  Last sync today 03:00 ✓     │
│                              │
│ › Set up new network         │
│   Forget network             │
└──────────────────────────────┘
```

- **Set up new network:** runs steps 1–2 and 4 of the first-time setup. City and volume are kept. If you cancel (BOOT on the QR screen), the old network stays.
- **Forget network:** asks "Forget HomeNet? BOOT: cancel · PWR: forget". The device then keeps running offline from the clock chip and cache, and shows the "no Wi-Fi" mark.

#### Reset

Menu → Reset offers two levels:

| Option | Clears | Keeps |
|---|---|---|
| **Reset settings** | Wi-Fi, city, volume, adhan toggles, prayer-time cache | Clock (RTC), adhan recordings, firmware |
| **Restart** | nothing (just reboots) | everything |

Reset settings asks twice, so a stray press can't wipe anything:

1. "Reset all settings? Wi-Fi, city and volume will be erased." with "BOOT: cancel · PWR: continue".
2. "Are you sure? Hold PWR for 2 s to reset". It needs a 2 s hold, which is still well short of the 3 s hardware power-off. A progress bar fills while you hold, and letting go early cancels.
3. "Resetting…", then a reboot into Step 0 of first-time setup.

#### Emergency reset (when the menu isn't usable)

- **Buttons:** hold BOOT for 10 s on any screen, including the adhan screen and setup. After 3 s a bar appears ("Keep holding to reset"), and at 10 s the device goes to step 1 of Reset settings, so it still asks to confirm.
  - Don't hold BOOT *while plugging in*: that puts the chip into flashing mode (a dead-looking, blank-screen state). Unplug and plug back in to leave it.
- **USB from the Mac:** `cargo xtask reset` (settings only) or `cargo xtask reset --all` (also erases the audio partition, after which `cargo xtask flash` is needed).
- **Last resort:** `espflash erase-flash`, then `cargo xtask flash` to reinstall everything.

---

## 6. Prayer and adhan rules

- The Fajr adhan follows Turkish practice: the sabah (morning) adhan is called at Diyanet's İmsak time. A per-prayer minute offset (temkin) can be added later if wanted.
- No adhan at Sunrise.
- Missed adhan (the device was off or busy at the prayer minute): it doesn't play late. It only plays if the device wakes within 2 minutes of the time.
- Friday: no special handling in v1.

---

## 7. Audio

**Recordings:** all five are by Nurettin Okumuş, each in its traditional makam. They're stored in `assets/ezan/` and git-ignored, because they're third-party recordings kept for personal use.

| File | Prayer | Makam | Length |
|---|---|---|---|
| `sabah.mp3` | Fajr (with "es-salâtü hayrün mine'n-nevm") | Saba | 4:42 |
| `ogle.mp3` | Dhuhr | Uşşak | 4:02 |
| `ikindi.mp3` | Asr | Rast | 3:34 |
| `aksam.mp3` | Maghrib | Segâh | 1:59 |
| `yatsi.mp3` | Isha | Hicaz | 3:45 |
| | **Total** | | **18:02** |

A file can be swapped by replacing it and re-running `cargo xtask flash`. `sela.mp3` (the Friday salâ before Dhuhr) is optional and comes later.

**Size budget:** 18 minutes is too much for uncompressed audio or ADPCM in flash (22 kHz ADPCM would be about 12 MB). So the audio stays compressed in flash and is decoded on the device:

- **Build pipeline (`cargo xtask audio`):**
  1. Decode (symphonia).
  2. Trim leading and trailing silence.
  3. Mix down to mono and resample to 22.05 kHz (rubato).
  4. Loudness-normalize all five to the same level.
  5. Re-encode as **mono MP3 at 32 kbps**, which comes to about 4.3 MB for all five.
  6. Pack them with a small index into `audio.bin`.
- **On the device:** a pure-Rust MP3 decoder (`nanomp3`, a port of minimp3; built at `opt-level=1` because the Xtensa LLVM backend crashes on it at higher levels) streams frames from flash into a small PSRAM ring buffer, which feeds I²S. Decoding at 22 kHz mono uses a few percent of one core.
- A solo voice at 32 kbps mono sounds clean through a small speaker. If it doesn't, the fallback is the TF card with full-quality files under `/ezan/` (milestone 6).

**Storage:** a dedicated `audio` flash partition, written by `xtask flash`. Partition table for 8 MB:

```
nvs        24K
phy_init    4K
factory   2.5M   app (currently ~1.6 MB)
audio     5.4M   data/undefined, found by label; packed MP3s + index (≈ 22 min at 32 kbps)
```

`xtask audio` fails with a clear message if the pack exceeds the partition.

**Playback:**

1. Audio rail on (GPIO 42 low).
2. ES8311 init over I²C.
3. I²S at 22.05 kHz.
4. PA on (GPIO 46).
5. Stream-decode the MP3 for that prayer.
6. PA off, rail off.

Volume is set with the codec's DAC volume register. A 50 ms fade in and out avoids pops.

---

## 8. Settings persisted (NVS)

`wifi_ssid`, `wifi_pass`, `city_id`, `volume`, `adhan_mask` (5 bits), `source` (emushaf / awqat) + optional awqat credentials, `last_sync`, and the prayer-time cache blob.

---

## 9. Testing

- `core` crate: host unit tests for next-prayer selection, midnight and Isha→Fajr rollover, DST transition days (for example London in late March), countdown formatting, and the Diyanet JSON parser against saved fixtures.
- `simulator`: renders every screen at 200×200, with a `--snapshot` flag that writes PNGs so UI changes can be reviewed visually.
- On-device: a `--features selftest` firmware that cycles the display, beeps the speaker, reads the RTC and SHTC3, and prints a report over serial.

---

## 10. Milestones

| M | Deliverable | Done when |
|---|---|---|
| 0 | Toolchain (`espup`, `espflash`, `ldproxy`), workspace skeleton, hello-world flashed, board revision confirmed | Serial log from the board |
| 1 | SSD1681 driver + `core/ui` + simulator; static Home screen on the panel | Same pixels in the simulator and on the panel |
| 2 | Wi-Fi STA (hard-coded creds) + SNTP + TZ + PCF85063 | Correct local time for 3 test cities across a DST zone |
| 3 | Diyanet fetch + cache + live countdown with partial refresh | Runs 48 h with correct rollovers |
| 4 | ES8311 + MP3 player + xtask audio pipeline; the right makam at each prayer time | All five recordings play cleanly at the right minute |
| 5 | Buttons, menu, city picker, setup and reset flows (§5.7), USB setup | First-run, change-Wi-Fi and reset flows all work from a phone and on the device |
| 6 | Polish: battery and deep sleep, SD-card audio override, official awqat source, OTA (if partition space allows) | — |

---

## 11. Open questions

1. **Power:** always on USB, or should battery life matter in v1?
