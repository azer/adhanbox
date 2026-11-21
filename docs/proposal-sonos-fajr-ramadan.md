# Proposal: Sonos playback, Fajr volume, Ramadan

Status: Sonos and Fajr volume are implemented and tested on the device (section 8). Ramadan labels, the sahur alarm and the Fajr fade-in are not built. Sonos behaviour was first tested from a Mac on the same network (section 7), 21 Nov 2026.

## Principle

Prayer times must stay clockwork. Every feature here is optional and off by default, and none of them may delay or lose an ezan:

- With Sonos off, the firmware takes exactly the path it takes today.
- The box's own speaker is always the fallback, with a bounded worst-case delay of about 5 seconds.
- Any failure ends quiet, never loud.

---

## 1. Sonos

### 1.1 Menu

A new main-menu row, `Sonos`, showing `Off` or `2 rooms`. Opening it:

| Row | Behaviour |
|---|---|
| Sonos: On / Off | Hold top to toggle. Off is the default. |
| Volume | Own 1–6 scale, previewed on the ticked Sonos speakers. |
| Fajr volume | `Same` or its own quieter level. |
| One row per room | Hold top to tick or untick. |
| Back | |

- Opening the screen joins Wi-Fi and finds the speakers. No account or login.
- Ticked rooms and volumes are remembered while Sonos is off.
- Speakers are remembered by their Sonos ID, not their network address, and looked up again before each prayer.
- A line-out device (Sonos Port) set to fixed output is not offered, because its volume cannot be controlled. A Port with variable output is listed but should normally stay unticked: its real loudness depends on the external amplifier.
- "Reset settings" clears the Sonos settings too.

### 1.2 At prayer time

1. About one minute before the prayer, the box joins Wi-Fi and checks the ticked speakers are reachable.
2. At the prayer minute, for each ticked speaker: set the volume, read it back, and only then start playback (see 1.3).
3. The box serves the ezan recording from its own flash; the speakers fetch it over the local network. No internet is needed.
4. If several speakers are ticked, they are grouped for the ezan so they play in sync, and returned to their previous groups afterwards.
5. The screen shows the Adhan screen as today. Any button stops the ezan on the Sonos speakers too.
6. Afterwards: stop, confirm stopped, restore each speaker's volume and what it was playing, then turn Wi-Fi off.

The daily 03:00 sync is unchanged.

### 1.3 Volume guarantee

A speaker's volume number is shared by every source, so a loud TV or tablet session leaves it high. Rules, per speaker:

1. **Set, read back, then play.** Play is only sent if the speaker reports the exact volume the box asked for. A speaker that does not confirm is skipped.
2. **Hard cap.** The six steps map to modest Sonos levels (roughly 4 to 23 out of 100, to be tuned by ear). The top step is a fixed ceiling in the firmware.
3. **Restore only after silence.** The previous volume is restored only once the speaker confirms it has stopped. If that cannot be confirmed, the speaker is left at the quiet level.
4. **Per speaker, not per group.** Each grouped speaker is set and checked individually.

Known limits: someone could raise the volume in the ~50 ms between readback and play; and a Port's loudness depends on its external amplifier.

### 1.4 Knowing it really played, and the fallback

Tests showed Sonos answers "OK" to a play command even when playback fails, so the command result is never used as proof. Playback counts as confirmed only when the speaker reports:

- state "playing",
- on the box's own file,
- with the position counter advancing,
- unmuted, at the volume the box set.

Fallback rules:

- Wi-Fi not up at the prayer minute: the box's speaker plays at the exact minute.
- Not confirmed within about 5 seconds: the box's speaker plays the ezan from the start.
- Confirmed, but the speaker stops before the ezan should end and nobody pressed stop: the box's speaker takes over.
- The box checks once a second for the whole ezan.

This proves the speaker is playing the stream at a known volume. It does not prove sound reached the room (for example, a Port whose amplifier is switched off).

### 1.5 Test results

| Check | Result |
|---|---|
| Discovery without login | All four devices found (two Era 100, one Beam, one Port) |
| Plays the box's file format (22 kHz mono, 32 kbps) | Yes, playing 0.65 s after the command |
| Set and read volume, mute | Works, about 10 ms per command |
| Group and ungroup two speakers | Works, about 0.3 s each way |
| Missing file | "Stopped" within 0.35 s |
| Server not answering | "Stopped" within 0.1 s |
| Server unreachable | "Stopped" after 3.5 s |
| Stream dies mid-playback | Played the 12 s it had, then "stopped" within about 0.5 s |
| Speaker unreachable | Command times out (box would use 2 s) |

Sonos fetched the whole 1 MB file in about 0.1 s from a fast server, so once the file is handed over, playback does not depend on the box's Wi-Fi.

Not yet tested:

- The box itself: finding speakers, serving the file from flash, time to join Wi-Fi.
- Interrupting and resuming something that is playing (music, TV on the Beam).
- Restoring previous speaker groups after a multi-room ezan.
- A speaker losing power or Wi-Fi mid-ezan.

### 1.6 Audio quality

The recordings are 32 kbps mono, tuned for the small speaker, and will sound thinner on Sonos. The five recordings use about 4.3 MB of the 5.4 MB audio partition, so better-quality copies do not fit. Proposed: ship as is, judge by ear, revisit only if it bothers.

---

## 2. Fajr volume (box speaker)

- New main-menu row `Fajr volume`, directly under `Volume`: `Same` or its own level (for example 2/6), set on the same preview screen.
- Main-menu `Volume` and `Fajr volume` apply to the box's own speaker, which is also what the Sonos fallback uses.
- Sonos has its own pair of settings (1.1).

Suggested, not yet agreed: a slow fade-in for Fajr on both the box and Sonos.

---

## 3. Ramadan

Suggested, not yet agreed. The box already has the Hijri date from Diyanet, so it knows when it is Ramadan.

- **Labels:** during Ramadan the home screen says "Iftar" for the Maghrib countdown and "Imsak" for Fajr. The times themselves are already shown today.
- **Sahur alarm:** `Off / 30 / 45 / 60 / 90 min before imsak`, active only in Ramadan. It needs its own sound: a soft synthesized chime (no flash space) or a supplied recording (about 1.1 MB is free).

---

## 4. Open decisions

1. **Several speakers, one fails:** is the ezan "delivered" if at least one ticked speaker is confirmed (proposed), or should the box's speaker also play when any ticked speaker fails?
2. **Volume raised mid-ezan:** leave it alone (proposed), or pull it back to the box's level?
3. **Muted speaker:** unmute for the ezan and re-mute after (proposed), or treat mute as "skip this speaker"?
4. **Fajr fade-in:** yes or no.
5. **Ramadan labels and sahur alarm:** yes or no; chime or recording.
6. **One Sonos volume for all rooms** (proposed) or one per room.

## 5. Build order

1. **Test firmware for the box's side.** A console command only; the prayer path is untouched. Confirms discovery, serving the file, and Wi-Fi join time. Requires flashing the device.
2. **TV test from the Mac**, with the TV playing loudly on the Beam: ezan starts quiet, TV sound returns afterwards.
3. **Fajr volume** (small, independent of Sonos).
4. **Sonos**, behind the on/off switch, in this order: settings screen and room ticking, playback with volume guarantee, verification and fallback, restore.
5. **Ramadan labels and sahur alarm**, if agreed.

Each step ships only after the existing ezan path is re-tested on the device with Sonos off.

---

## 6. Implementation spec

### 6.1 Sonos protocol (local UPnP, no cloud, no auth)

**Discovery.** UDP multicast to `239.255.255.250:1900`:

```
M-SEARCH * HTTP/1.1
HOST: 239.255.255.250:1900
MAN: "ssdp:discover"
MX: 2
ST: urn:schemas-upnp-org:device:ZonePlayer:1
```

Each speaker replies with `LOCATION: http://<ip>:1400/xml/device_description.xml` and `USN: uuid:RINCON_<id>::...`. One reply is enough: `GetZoneGroupState` on any speaker returns every speaker's ID, room name, address and current group. Send the search three times; UDP can drop.

**Control.** HTTP POST to `http://<ip>:1400<path>` with headers `Content-Type: text/xml; charset="utf-8"` and `SOAPACTION: "<urn>#<Action>"`, body:

```xml
<?xml version="1.0"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"
            s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
  <s:Body><u:ACTION xmlns:u="URN">ARGS</u:ACTION></s:Body>
</s:Envelope>
```

| Service | Path | URN |
|---|---|---|
| AVTransport | `/MediaRenderer/AVTransport/Control` | `urn:schemas-upnp-org:service:AVTransport:1` |
| RenderingControl | `/MediaRenderer/RenderingControl/Control` | `urn:schemas-upnp-org:service:RenderingControl:1` |
| ZoneGroupTopology | `/ZoneGroupTopology/Control` | `urn:schemas-upnp-org:service:ZoneGroupTopology:1` |

Actions used (all take `<InstanceID>0</InstanceID>` except topology):

| Purpose | Action | Arguments / result |
|---|---|---|
| List speakers and groups | `GetZoneGroupState` | result `ZoneGroupState` (escaped XML): `ZoneGroup Coordinator=…`, `ZoneGroupMember UUID=… ZoneName=… Location=…` |
| Read state | `GetTransportInfo` | `CurrentTransportState`: `STOPPED`, `TRANSITIONING`, `PLAYING`, `PAUSED_PLAYBACK` |
| Read position | `GetPositionInfo` | `TrackURI`, `RelTime` (`H:MM:SS`), `TrackDuration` |
| Read current source | `GetMediaInfo` | `CurrentURI`, `CurrentURIMetaData` |
| Set source | `SetAVTransportURI` | `<CurrentURI>…</CurrentURI><CurrentURIMetaData></CurrentURIMetaData>` |
| Play / stop | `Play` (`<Speed>1</Speed>`), `Stop` | |
| Seek (restore) | `Seek` | `<Unit>REL_TIME</Unit><Target>H:MM:SS</Target>` (untested) |
| Volume | `GetVolume`, `SetVolume` | `<Channel>Master</Channel>`, `<DesiredVolume>0..100</DesiredVolume>` |
| Mute | `GetMute`, `SetMute` | `<Channel>Master</Channel>`, `<DesiredMute>0|1</DesiredMute>` |
| Fixed line-out | `GetOutputFixed` | `CurrentFixed` 0 or 1 |
| Join a group | `SetAVTransportURI` on the joining speaker | `CurrentURI` = `x-rincon:RINCON_<coordinator id>` |
| Leave a group | `BecomeCoordinatorOfStandaloneGroup` | |

Rules:

- Transport actions go to the group coordinator; volume and mute go to each speaker.
- Every HTTP call has a 2 s timeout. A timeout counts as failure for that speaker.
- HTTP status 200 on `Play` proves nothing (section 1.4).

**Serving the file.** The box runs an HTTP server on the station interface for the duration of the ezan, at `http://<box ip>:<port>/adhan/<slot>.mp3`. Observed Sonos behaviour the server must handle:

- Plain `GET` first, then `GET` with `Range: bytes=<n>-` (once for the last 128 bytes, once from byte 10). Must answer `206` with `Content-Range`, and `Accept-Ranges: bytes` on every response.
- Two or three connections at once; some are dropped by the speaker mid-transfer. Each sends `Connection: close`.
- `Content-Type: audio/mpeg`, exact `Content-Length`.
- The body comes straight from the memory-mapped audio partition (`audio::Partition::entry`), so no copy and no flash API calls.

### 6.2 Firmware changes

| File | Change |
|---|---|
| `firmware/src/sonos.rs` (new) | SSDP discovery, SOAP client, topology parsing, the play / verify / restore sequence. No UI. |
| `firmware/src/portal.rs` or new `serve.rs` | HTTP handler for the adhan file with Range support. |
| `firmware/src/net.rs` | Station connect that reports the box's IP; UDP multicast send for discovery. |
| `firmware/src/store.rs` | New settings (below), added to `KEYS` so reset clears them. |
| `firmware/src/app.rs` | Menu rows, `Mode::Sonos`, pre-prayer Wi-Fi join, routing in the adhan trigger, stop handling, fallback. |
| `firmware/src/audio.rs` | Per-play volume (Fajr); optional fade-in length. |
| `firmware/src/console.rs` | Test commands (6.4). |
| `core/src/ui/screens.rs`, `strings.rs` | Sonos screen, Ramadan labels. |
| `core/src/schedule.rs` | "Seconds until next adhan" for the pre-join; sahur alarm time; unit-tested on the host. |

New settings (NVS namespace `ezan`):

| Key | Type | Default | Meaning |
|---|---|---|---|
| `fajrvol` | u8 | 0 | Box Fajr volume; 0 = same as `volume` |
| `sonos` | u8 | 0 | Sonos on/off |
| `sonosvol` | u8 | 3 | Sonos volume step 1–6 |
| `sonosfajr` | u8 | 0 | Sonos Fajr step; 0 = same |
| `sonosrooms` | blob | empty | Ticked speakers: Sonos ID + room name each |
| `sahur` | u8 | 0 | Sahur alarm, minutes before imsak; 0 = off |

### 6.3 Prayer-time sequence

All Sonos work runs on its own thread so the main loop, buttons and local audio never block on the network.

```
T-60 s   if sonos on and rooms ticked: join Wi-Fi, start file server,
         discover, GetZoneGroupState, resolve ticked IDs to addresses.
         Record result: READY or NOT_READY.

T        NOT_READY ─────────────────────────────► local speaker, now.
         READY:
           for each ticked speaker (parallel, 2 s timeouts):
             snapshot: volume, mute, group, CurrentURI + metadata,
                       RelTime, transport state
             SetVolume(target) → GetVolume == target ? else drop speaker
             SetMute(0)
           pick coordinator; others join it (x-rincon:…)
           SetAVTransportURI(box url) → Play
           poll every 250 ms: PLAYING && TrackURI is ours && RelTime > 0

T+5 s    nothing confirmed ─► Stop on Sonos, local speaker from the start.

playing  poll every 1 s. Not PLAYING before expected end, and not stopped
         by a button ─► local speaker takes over.
         Button press ─► Stop on Sonos (and local), go Home.

end      Stop → confirm STOPPED → ungroup → restore URI/position →
         restore volume and mute → resume if it was playing → Wi-Fi off.
         Any restore step that cannot be confirmed is skipped; volume is
         restored last and only after STOPPED is confirmed.
```

Invariants:

- With `sonos = 0`, none of the above runs; `tick()` behaves as today.
- The local speaker never waits on a network call. The only added delay is the fixed 5 s confirmation window, and only when Sonos was READY.
- The daily sync never overlaps the pre-join window or an ezan.
- A crash or hang in the Sonos thread must not block the ezan: the main loop owns the T+5 s deadline and starts local playback itself.

### 6.4 Test plan

Host unit tests (`cargo test`, no hardware): topology XML parsing, SOAP response parsing, `RelTime` parsing, Range header parsing, volume step mapping and cap, pre-join timing, sahur time, Fajr volume selection.

Console commands for on-device testing: `sonos scan`, `sonos play <slot>`, `sonos stop`, and `adhan in <seconds>` to run the full prayer-time sequence without waiting for a real prayer.

Acceptance cases, each run on the device with real speakers:

| # | Case | Expected |
|---|---|---|
| 1 | Sonos off | Identical to current firmware; Wi-Fi stays off at prayer time |
| 2 | Sonos on, one room | Plays on Sonos within 1–2 s of the minute; box silent |
| 3 | Router off | Box speaker at the exact minute |
| 4 | Speaker unplugged before prayer | Box speaker at the exact minute (NOT_READY) or within 5 s |
| 5 | Speaker unplugged mid-ezan | Box speaker takes over within a few seconds |
| 6 | Speaker at volume 60 beforehand | Ezan at the box's level; 60 restored after |
| 7 | TV playing on the Beam | Ezan quiet; TV sound returns |
| 8 | Music playing | Paused, then resumed where it was |
| 9 | Speaker muted | Per open decision 3 |
| 10 | Two rooms ticked | In sync; previous groups restored |
| 11 | Button during Sonos ezan | Stops on Sonos, volumes restored |
| 12 | Fajr | Uses the Fajr levels on both box and Sonos |
| 13 | Seven days unattended, Sonos on | Every ezan logged as delivered (Sonos or fallback), none missed |

---

## 7. Tests already run

Run from a Mac on the same network using two small Python scripts (SSDP search, SOAP calls) and a logging HTTP server serving the box's own packed recordings (`target/audio/*.mp3`: 22,050 Hz mono, 32 kbps). Speaker firmware: Sonos 98.0-82170 (S2). All speakers were idle (`STOPPED`, nothing queued) throughout.

| # | Test | Method | Result |
|---|---|---|---|
| 1 | Discovery | SSDP M-SEARCH for `ZonePlayer:1`, sent three times | Four devices answered: two Era 100, one Beam, one Port |
| 2 | Topology | `GetZoneGroupState` on one speaker | 200 in 0.125 s; all four rooms, IDs, addresses; four standalone groups |
| 3 | State read | `GetTransportInfo`, `GetPositionInfo`, `GetMediaInfo`, `GetVolume`, `GetMute` on each | About 10 ms per call; volumes 9–12, none muted |
| 4 | Play the box's file | Beam: `SetVolume 8`, `SetAVTransportURI` (Dhuhr recording), `Play`, poll every 250 ms for 9 s, `Stop`, restore volume | `TRANSITIONING` at 0.34 s, `PLAYING` at 0.65 s, `RelTime` advanced one second per second, duration read correctly (4:00) |
| 5 | How Sonos fetches | Server log during test 4 | Two plain GETs, then `Range: bytes=962014-` (last 128 bytes) and `Range: bytes=10-`; `Connection: close`; whole 962 KB taken in about 0.1 s |
| 6 | Missing file | URL that returns 404 | `Play` returned 200; state `STOPPED` at 0.35 s |
| 7 | Server not listening | URL on a closed port | `Play` returned 200; `STOPPED` at 0.06 s |
| 8 | Host unreachable | URL on an unused address | `Play` returned 200; `TRANSITIONING` until 3.2 s, `STOPPED` at 3.45 s |
| 9 | Mute | `SetMute 1`, `GetMute`, `SetMute 0`, `GetMute` (nothing playing) | Reads back 1 then 0 |
| 10 | Speaker unreachable | SOAP call to an unused address, 3 s timeout | Times out at 3.0 s, no hang |
| 11 | Stream dies mid-playback | Throttled server (~48 KB/s) that stops for good after 48 KB; Beam at volume 5 | Played 12 s (the audio it had), then `STOPPED` at 13.7 s, about 0.5 s after running out; retried with `Range: bytes=49152-` first |
| 12 | Group / ungroup | Port joins Beam via `x-rincon:<id>`, then `BecomeCoordinatorOfStandaloneGroup`; nothing playing | Join 0.29 s, leave 0.33 s; topology showed 3 groups then 4 again |
| 13 | Device types and fixed output | `device_description.xml`, `GetOutputFixed`, `GetSupportsOutputFixed` | Port supports fixed output (currently variable); the others do not |

After the tests every speaker was back to `STOPPED`, its original volume, standalone, and the Beam's source pointed at its own queue.

Caveats on these results:

- Test 11's server had a bug: it also cut the connection on the tail Range request. The conclusion (speaker stops within about 0.5 s of running out of data) still holds, but it should be re-run against the real firmware (acceptance case 5).
- Volume readback after `SetVolume` was not read explicitly during playback tests; `GetVolume` was verified separately (test 3) and the restore was verified by reading the final state.
- Nothing was playing on any speaker, so interrupting and resuming a source is untested.
- All timings are from a wired-speed Mac. The box will serve the file more slowly; time-to-`PLAYING` on the real device is unmeasured.

---

## 8. Implementation status and on-device results (21 Nov 2026)

Built: `firmware/src/sonos.rs` (protocol, session, file server), `firmware/src/sonos_task.rs` (worker thread), the Sonos screen, Sonos volume and Fajr volume, box Fajr volume, the prayer-time flow with fallback, and console commands. `cargo xtask sonos scan|play` runs the same Sonos module from a computer.

Differences from the plan above:

- Wi-Fi is joined 75 s before the prayer (it takes about 7 s on this network; the limit is 15 s).
- The daily sync no longer starts within 5 minutes of a prayer, so a slow sync cannot delay an adhan.
- A speaker that stops within 15 s of the end of the recording counts as finished, not lost.
- Open decisions 1, 2, 3 and 6 were built as proposed: one confirmed speaker counts as delivered; a volume raised mid-ezan is left alone; a muted speaker is unmuted and re-muted; one Sonos volume for all rooms.

Console commands (USB): `ezan sonos scan | on | off | tick NAME | untick NAME | vol N | fajr N`, `ezan adhan` (Menu, Test adhan), `ezan adhan in N` (moves Isha N minutes ahead in memory, to run the real trigger; a restart undoes it).

Results on the device, with a Beam as the ticked speaker:

| Case | Result |
|---|---|
| Box finds the speakers | All four, about 6.5 s including joining Wi-Fi |
| Test adhan via Sonos | Play sent 0.2 s after start, confirmed audible at 1.6 s, 1.1 MB served from flash |
| Real trigger (`adhan in 2`) | Wi-Fi joined 75 s ahead in 7.1 s; play sent 0.2 s after the minute; confirmed 1.6 s after |
| Sonos fails to start | Box speaker took over after 2.0 s (found by accident: lwIP could not report the box's address; fixed) |
| Speaker stopped mid-ezan (from the Mac) | Detected in about 1.3 s; box speaker took over |
| Speaker left at volume 35 | Ezan played at 7; 35 restored afterwards |
| Stop from the box | Speaker stopped, volume and source restored, Wi-Fi off 2 s later |
| Sonos screen via simulated buttons | Open, untick, tick, live volume preview (7 to 10 to 7 on the speaker), back, Wi-Fi off |
| Sonos off | Test adhan plays on the box; Wi-Fi stays off |
| Two devices grouped (from the Mac, same code) | Played in sync, groups and volumes restored |

Added after the first round:

| Case | Result |
|---|---|
| Wi-Fi down before a prayer (`ezan sonos nowifi`, then `adhan in 2`) | Join gave up after 15 s; box speaker started exactly on the minute |
| Three ticked speakers playing AirPlay music as one group (33 / 16 / 8) | All three taken over at volume 14; play sent 1.0 s after the minute, confirmed at 2.4 s; ezan ran to its natural end (223 s) and was recognised as ended; same group and old volumes restored 3 s later |
| AirPlay after the ezan | The speaker reported playing but the AirPlay session did not carry on by itself; music had to be started again |

Changes after the first round: the speakers moved to their own Speakers screen with "Search again"; the Sonos screen opens without touching the network; ticking a speaker switches Sonos on; Fajr volume steps from the level Fajr currently plays at, and setting it equal to the normal volume means "same"; a group whose every speaker was taken over is rebuilt under its old lead speaker.

Still untested: Wi-Fi dropping between the join and the prayer, a speaker losing power mid-ezan, resuming music started from the Sonos app or TV sound, a muted speaker, the Fajr levels at a real Fajr, and the seven-day unattended run. Preparing several speakers happens one after another; doing it in parallel would shorten the start.
