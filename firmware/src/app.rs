//! The device state machine: screens, buttons, sync schedule, adhan trigger.
//! UX reference: SPEC.md §5.
use crate::audio::Audio;
use crate::board::Power;
use crate::buttons::{Btn, Buttons, Event};
use crate::console::Cmd;
use crate::epd::Epd;
use crate::net::{self, Net};
use crate::portal::{self, Portal, WifiState};
use crate::sonos::{self, Speaker};
use crate::sonos_task::{Player, State as Sonos};
use crate::store::{Room, Settings, Store, ROOMS_MAX, VOLUME_MAX};
use crate::{clock, rtc};
use esp_idf_svc::hal::i2c::I2cDriver;
use ezan::cities::{self, CITIES};
use ezan::frame::Frame;
use ezan::schedule;
use ezan::ui::{self, Home, Mark, Status};
use ezan::strings::SELECT_HINT;
use ezan::{Date, DayTimes, Prayer};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const MENU: [&str; 10] = ["City", "Adhan", "Volume", "Fajr volume", "Sonos", "Test adhan", "Wi-Fi", "About", "Reset", "Back"];
const M_CITY: usize = 0;
const M_ADHAN: usize = 1;
const M_VOLUME: usize = 2;
const M_FAJR: usize = 3;
const M_SONOS: usize = 4;
const M_TEST: usize = 5;
const M_WIFI: usize = 6;
const M_ABOUT: usize = 7;
const M_RESET: usize = 8;
const PREVIEW_SLOT: u8 = 1; // Dhuhr
const FAJR_SLOT: u8 = 0;
// Sonos screen rows. The speakers have their own screen: one row each, then search + back.
const S_ON: usize = 0;
const S_VOLUME: usize = 1;
const S_FAJR: usize = 2;
const S_SPEAKERS: usize = 3;
const S_COUNT: usize = 5;
/// Join Wi-Fi this long before a prayer when Sonos is on.
const SONOS_LEAD_S: u32 = 75;
/// Wi-Fi must be up within this long, or the box plays on its own speaker.
const SONOS_WIFI_LIMIT: Duration = Duration::from_secs(15);
/// A Sonos speaker must confirm it is audibly playing within this long, or the box's own
/// speaker plays instead. This is the most an adhan can ever be late.
const SONOS_CONFIRM: Duration = Duration::from_secs(5);
/// Keep Wi-Fi up this long after joining for a prayer that then did not play.
const SONOS_HOLD: Duration = Duration::from_secs(150);
/// No daily sync this close to the next prayer time (a slow sync must never delay an adhan).
const SYNC_CLEAR_S: u32 = 300;
const PREVIEW_MS: u32 = 10_000;
const EMERGENCY_START_MS: u32 = 5_000;
const EMERGENCY_MS: u32 = 10_000;
const RESET_HOLD_MS: u32 = 2_000;
const POWER_OFF_MS: u32 = 4_000;
const IDLE_TO_HOME: Duration = Duration::from_secs(90);

#[derive(Clone, Debug, PartialEq)]
enum Mode {
    Home,
    Menu { sel: usize },
    City { sel: usize, setup: bool },
    AdhanToggles { sel: usize },
    Volume { setup: bool, fajr: bool },
    Sonos { sel: usize },
    SonosRooms { sel: usize },
    SonosVolume { fajr: bool },
    Wifi { sel: usize },
    WifiForget,
    About,
    ResetMenu { sel: usize },
    ResetConfirm,
    ResetHold { pct: u8 },
    Emergency { pct: u8, prev: Box<Mode> },
    Adhan { prayer: Prayer, at: u16 },
    SetupWelcome,
    SetupQr,
    SetupSync { rows: [Mark; 3], detail: [String; 3], failed: bool },
    SetupDone { until: Instant },
    Message { title: String, body: Vec<String> },
}

impl Mode {
    fn is_setup(&self) -> bool {
        matches!(self, Mode::SetupWelcome | Mode::SetupQr | Mode::SetupSync { .. } | Mode::SetupDone { .. })
            || matches!(self, Mode::City { setup: true, .. } | Mode::Volume { setup: true, .. })
    }
    /// Screens that warrant a flashing full refresh when entered.
    fn wants_full(&self) -> bool {
        matches!(self, Mode::Home | Mode::Adhan { .. } | Mode::SetupWelcome | Mode::SetupDone { .. })
    }
}

pub struct Hw {
    pub epd: Epd,
    pub buttons: Buttons,
    pub power: Power,
    pub i2c: Arc<Mutex<I2cDriver<'static>>>,
    pub net: Net,
    pub store: Store,
    pub audio: Audio,
    pub player: Player,
    pub console: Receiver<Cmd>,
}

/// An adhan that was handed to Sonos and is being watched.
#[derive(Clone, Copy)]
struct SonosAdhan {
    slot: u8,
    started: Instant,
    confirmed: bool,
}

pub struct App {
    hw: Hw,
    s: Settings,
    days: Vec<DayTimes>,
    mode: Mode,
    frame: Frame,
    shown: Frame,
    shown_mode: Option<Mode>,
    last_full: Instant,
    rendered_minute: Option<(Date, u16)>,
    dirty: bool,
    last_input: Instant,
    // setup
    portal: Option<Portal>,
    portal_status: String,
    ap_ip: String,
    setup_from_menu: bool,
    wifi_connected_at: Option<Instant>,
    // sync
    next_sync: i64,
    sync_failures: u32,
    last_adhan: Option<(Date, Prayer)>,
    // sonos
    /// Wi-Fi is up because of Sonos (a prayer, the Sonos screens), not a sync.
    sonos_wifi: bool,
    /// The prayer Wi-Fi was last joined for, and when.
    sonos_joined: Option<((Date, Prayer), Instant)>,
    sonos_adhan: Option<SonosAdhan>,
    /// Rows of the Sonos screen: speakers on the network plus ticked ones that are away.
    sonos_list: Vec<Room>,
    /// Test hook (`ezan sonos nowifi`): Sonos Wi-Fi joins use a network that does not exist.
    test_no_wifi: bool,
}

impl App {
    pub fn new(hw: Hw) -> Self {
        let mut s = hw.store.load();
        if s.ssid.is_empty() {
            if let Some(ssid) = option_env!("EZANBOX_WIFI_SSID") {
                s.ssid = ssid.into();
                s.pass = option_env!("EZANBOX_WIFI_PASS").unwrap_or("").into();
                s.setup_done = true;
            }
        }
        clock::set_tz(cities::get(s.city).tz_posix);
        if let Some(t) = rtc::read(&mut hw.i2c.lock().unwrap()) {
            clock::set_now(t);
            if !clock::valid() {
                log::warn!("RTC time is older than the firmware; ignoring it");
            }
            log::info!("clock from RTC: {:?}", clock::local_now());
        } else {
            log::warn!("RTC time invalid; waiting for SNTP");
        }
        let days = hw.store.load_cache(s.city);
        log::info!("city {} · {} cached days", cities::get(s.city).name, days.len());

        let mode = if s.setup_done || !s.ssid.is_empty() { Mode::Home } else { Mode::SetupWelcome };
        let mut app = Self {
            hw,
            s,
            days,
            mode,
            frame: Frame::new(),
            shown: Frame::new(),
            shown_mode: None,
            last_full: Instant::now(),
            rendered_minute: None,
            dirty: true,
            last_input: Instant::now(),
            portal: None,
            portal_status: String::new(),
            ap_ip: String::new(),
            setup_from_menu: false,
            wifi_connected_at: None,
            next_sync: 0,
            sync_failures: 0,
            last_adhan: None,
            sonos_wifi: false,
            sonos_joined: None,
            sonos_adhan: None,
            sonos_list: Vec::new(),
            test_no_wifi: false,
        };
        app.schedule_first_sync();
        app
    }

    pub fn run(mut self) -> ! {
        self.render();
        loop {
            while let Some(ev) = self.hw.buttons.poll() {
                self.last_input = Instant::now();
                self.on_button(ev);
            }
            while let Ok(cmd) = self.hw.console.try_recv() {
                self.on_cmd(cmd);
            }
            self.tick();
            self.render();
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn go(&mut self, m: Mode) {
        self.mode = m;
        self.dirty = true;
    }

    // ───────────────────────────── time & data ─────────────────────────────

    fn city(&self) -> &'static cities::City {
        cities::get(self.s.city)
    }

    fn status(&self) -> Status {
        let now = clock::local_now();
        Status {
            no_wifi: !self.s.ssid.is_empty() && clock::valid() && clock::now() - self.s.last_sync_ok > 3 * 86_400,
            battery: None,
            times_expiring: !self.days.is_empty() && schedule::days_left(now, &self.days) < 3,
        }
    }

    fn schedule_first_sync(&mut self) {
        if self.s.ssid.is_empty() {
            self.next_sync = i64::MAX;
            return;
        }
        let now = clock::local_now();
        let stale = !clock::valid()
            || schedule::days_left(now, &self.days) < 7
            || clock::now() - self.s.last_sync_ok > 20 * 3600;
        self.next_sync = if stale { clock::now() + 3 } else { self.next_daily_sync() };
    }

    /// Next 03:xx local (minute spread by MAC so devices don't all hit the API at once).
    fn next_daily_sync(&self) -> i64 {
        let now = clock::now();
        let local = clock::local(now);
        let spread = (self.hw.net.ap_ssid.bytes().map(|b| b as u32).sum::<u32>() % 50) * 60;
        let target = 3 * 3600 + spread;
        let mut delta = (target as i64 - local.sec as i64).rem_euclid(86_400);
        if delta < 600 {
            delta += 86_400;
        }
        now + delta
    }

    fn sync_wifi(&mut self) -> Result<(), net::ConnectError> {
        let (ssid, pass) = (self.s.ssid.clone(), self.s.pass.clone());
        self.hw.net.connect(&ssid, &pass)
    }

    fn sync_clock(&mut self) -> anyhow::Result<()> {
        let t = net::sntp_sync()?;
        let _ = rtc::write(&mut self.hw.i2c.lock().unwrap(), t);
        log::info!("clock synced: {:?}", clock::local_now());
        Ok(())
    }

    fn sync_times(&mut self) -> anyhow::Result<usize> {
        let mut days = net::fetch_times(self.city().diyanet_id)?;
        if clock::valid() {
            let yesterday = clock::local_now().date.add_days(-1);
            days.retain(|d| d.date >= yesterday);
        }
        self.hw.store.save_cache(self.s.city, &days);
        self.days = days;
        log::info!("prayer times: {} days for {}", self.days.len(), self.city().name);
        Ok(self.days.len())
    }

    /// Full silent sync (daily / after city change / console). Returns success.
    fn sync_all(&mut self) -> bool {
        let ok = match self.sync_wifi() {
            Ok(()) => {
                let c = self.sync_clock();
                let t = self.sync_times();
                if let Err(e) = &c {
                    log::warn!("sync clock: {e:?}");
                }
                if let Err(e) = &t {
                    log::warn!("sync times: {e:?}");
                }
                t.is_ok()
            }
            Err(e) => {
                log::warn!("sync wifi: {e:?}");
                false
            }
        };
        self.hw.net.stop();
        if ok {
            self.s.last_sync_ok = clock::now();
            self.hw.store.save(&self.s);
            self.sync_failures = 0;
            self.next_sync = self.next_daily_sync();
        } else {
            const BACKOFF: [i64; 4] = [300, 1800, 7200, 86_400];
            self.next_sync = clock::now() + BACKOFF[(self.sync_failures as usize).min(3)];
            self.sync_failures += 1;
        }
        self.hw.buttons.ignore_held();
        self.dirty = true;
        ok
    }

    // ───────────────────────────── periodic ─────────────────────────────

    fn tick(&mut self) {
        // Inactivity: menus fall back to Home.
        let in_menu = matches!(
            self.mode,
            Mode::Menu { .. }
                | Mode::City { setup: false, .. }
                | Mode::AdhanToggles { .. }
                | Mode::Sonos { .. }
                | Mode::SonosRooms { .. }
                | Mode::Wifi { .. }
                | Mode::About
                | Mode::ResetMenu { .. }
                | Mode::Message { .. }
        );
        if in_menu && self.last_input.elapsed() > IDLE_TO_HOME {
            self.go(Mode::Home);
        }

        match self.mode.clone() {
            Mode::SetupQr => self.tick_portal(),
            Mode::SetupDone { until } if Instant::now() >= until => self.go(Mode::Home),
            Mode::Adhan { .. } => self.tick_adhan(),
            _ => {}
        }

        if self.mode.is_setup() {
            return;
        }
        self.tick_sonos_wifi();

        // Adhan trigger
        if clock::valid() && !matches!(self.mode, Mode::Adhan { .. } | Mode::Volume { .. } | Mode::SonosVolume { .. }) {
            let now = clock::local_now();
            if let Some(p) = schedule::due(now, &self.days) {
                let key = (now.date, p);
                if self.last_adhan != Some(key) {
                    self.last_adhan = Some(key);
                    if self.adhan_enabled(p) && self.hw.audio.available {
                        log::info!("adhan: {p:?}");
                        self.begin_adhan(p, now.minute_of_day());
                    }
                }
            }
        }

        // Daily sync: never during an adhan, while Sonos has Wi-Fi, or just before a prayer.
        if !matches!(self.mode, Mode::Adhan { .. }) && !self.sonos_wifi && clock::now() >= self.next_sync {
            let soon = clock::valid() && schedule::next(clock::local_now(), &self.days).is_some_and(|n| n.remaining_s <= SYNC_CLEAR_S);
            if !soon {
                self.sync_all();
            }
        }
    }

    // ───────────────────────────── adhan ─────────────────────────────

    fn adhan_enabled(&self, p: Prayer) -> bool {
        p.adhan_bit().map(|b| self.s.adhan_mask & (1 << b) != 0).unwrap_or(false)
    }

    /// Play on the box's own speaker at this prayer's volume.
    fn play_local(&mut self, slot: u8) {
        self.hw.audio.set_volume(self.s.volume_for(slot));
        self.hw.audio.play(slot, None);
    }

    /// Start the adhan now: on Sonos when the ticked speakers are ready, otherwise on the box.
    fn begin_adhan(&mut self, prayer: Prayer, at: u16) {
        let slot = prayer.adhan_bit().unwrap_or(PREVIEW_SLOT);
        self.sonos_joined = None;
        if self.s.sonos_active() && self.sonos_wifi && self.hw.player.state() == Sonos::Ready {
            let volume = sonos::step_volume(self.s.sonos_step_for(slot));
            log::info!("adhan: via Sonos at volume {volume}");
            self.hw.player.play(slot, volume);
            self.sonos_adhan = Some(SonosAdhan { slot, started: Instant::now(), confirmed: false });
        } else {
            if self.s.sonos_active() {
                log::warn!("adhan: Sonos not ready ({:?}); using the box speaker", self.hw.player.state());
            }
            self.sonos_adhan = None;
            self.play_local(slot);
        }
        self.go(Mode::Adhan { prayer, at });
    }

    /// While the Adhan screen is up: leave when it ends; if Sonos is not provably playing, the
    /// box's own speaker takes over.
    fn tick_adhan(&mut self) {
        let Some(a) = self.sonos_adhan else {
            if !self.hw.audio.is_playing() {
                self.hw.buttons.ignore_held();
                self.go(Mode::Home);
            }
            return;
        };
        let fallback = match self.hw.player.state() {
            Sonos::Playing => {
                if !a.confirmed {
                    log::info!("adhan: Sonos confirmed after {} ms", a.started.elapsed().as_millis());
                    self.sonos_adhan = Some(SonosAdhan { confirmed: true, ..a });
                }
                None
            }
            Sonos::Ended => {
                self.sonos_adhan = None;
                self.hw.buttons.ignore_held();
                self.go(Mode::Home);
                None
            }
            Sonos::Failed => Some("did not start"),
            Sonos::Lost => Some("stopped early"),
            _ if !a.confirmed && a.started.elapsed() > SONOS_CONFIRM => Some("not confirmed in time"),
            _ => None,
        };
        if let Some(why) = fallback {
            log::warn!("adhan: Sonos {why} after {} ms; playing on the box speaker", a.started.elapsed().as_millis());
            self.hw.player.stop();
            self.sonos_adhan = None;
            self.play_local(a.slot);
        }
    }

    // ───────────────────────────── sonos ─────────────────────────────

    /// Join Wi-Fi and have the worker find the ticked speakers. Blocks for at most
    /// `SONOS_WIFI_LIMIT`. Returns false when Wi-Fi did not come up.
    fn sonos_join(&mut self) -> bool {
        let (mut ssid, pass) = (self.s.ssid.clone(), self.s.pass.clone());
        if self.test_no_wifi {
            ssid = "ezanbox-no-such-network".into();
        }
        let t = Instant::now();
        if !self.hw.net.connect_quick(&ssid, &pass, SONOS_WIFI_LIMIT) {
            log::warn!("sonos: Wi-Fi not up after {} ms", t.elapsed().as_millis());
            self.hw.net.stop();
            self.sonos_wifi = false;
            self.hw.buttons.ignore_held();
            return false;
        }
        log::info!("sonos: Wi-Fi up in {} ms", t.elapsed().as_millis());
        self.sonos_wifi = true;
        self.hw.buttons.ignore_held();
        true
    }

    /// Wi-Fi housekeeping for Sonos: join shortly before a prayer, leave when nothing needs it.
    fn tick_sonos_wifi(&mut self) {
        // Join ahead of the next adhan.
        if self.s.sonos_active() && clock::valid() && self.hw.audio.available && !matches!(self.mode, Mode::Adhan { .. } | Mode::Volume { .. } | Mode::SonosVolume { .. }) {
            if let Some(n) = schedule::next(clock::local_now(), &self.days) {
                let key = (n.date, n.prayer);
                if n.remaining_s <= SONOS_LEAD_S && self.adhan_enabled(n.prayer) && self.sonos_joined.map(|j| j.0) != Some(key) {
                    log::info!("sonos: {:?} in {} s, joining Wi-Fi", n.prayer, n.remaining_s);
                    self.sonos_joined = Some((key, Instant::now()));
                    if self.sonos_join() {
                        self.hw.player.prepare(&self.s.sonos_rooms, self.hw.net.sta_ip().unwrap_or(std::net::Ipv4Addr::UNSPECIFIED));
                    }
                }
            }
        }
        // Leave when no screen, prayer or speaker clean-up needs the network.
        if self.sonos_wifi {
            let wanted = matches!(self.mode, Mode::Sonos { .. } | Mode::SonosRooms { .. } | Mode::SonosVolume { .. })
                || self.sonos_adhan.is_some()
                || self.hw.player.busy()
                || self.sonos_joined.is_some_and(|j| j.1.elapsed() < SONOS_HOLD);
            if !wanted {
                self.sonos_leave();
            }
        }
    }

    /// Stop anything on Sonos, let the worker put the speakers back, then turn Wi-Fi off.
    fn sonos_leave(&mut self) {
        self.hw.player.release();
        if !self.hw.player.wait_idle(Duration::from_secs(8)) {
            log::warn!("sonos: worker still busy; turning Wi-Fi off anyway");
        }
        self.hw.net.stop();
        self.sonos_wifi = false;
        self.sonos_joined = None;
        log::info!("sonos: Wi-Fi off");
    }

    /// Menu → Sonos. Opens at once; the network is only used by the screens below it.
    fn open_sonos(&mut self) {
        if self.s.ssid.is_empty() {
            self.hw.buttons.ignore_held();
            return self.message("Sonos", &["Set up Wi-Fi first:", "Menu, Wi-Fi."]);
        }
        self.go(Mode::Sonos { sel: S_ON });
    }

    /// Sonos → Speakers: look for speakers and show them.
    fn open_sonos_rooms(&mut self, sel: usize) {
        self.message("Speakers", &["Looking for speakers..."]);
        self.sonos_scan();
        self.go(Mode::SonosRooms { sel: sel.min(self.sonos_list.len() + 1) });
    }

    /// Rebuild the speaker rows from the network.
    fn sonos_scan(&mut self) -> Vec<Speaker> {
        let mut found = Vec::new();
        if self.sonos_join() {
            let known: Vec<_> = self.s.sonos_rooms.iter().map(|r| r.ip).collect();
            match sonos::discover(&known) {
                // A fixed line-out level ignores volume commands, so it can never be played safely.
                Ok(v) => found = v.into_iter().filter(|s| !sonos::output_fixed(s.ip)).collect(),
                Err(e) => log::warn!("sonos: {e:#}"),
            }
        }
        // Keep ticked speakers' names and addresses current.
        let mut changed = false;
        for r in &mut self.s.sonos_rooms {
            if let Some(sp) = found.iter().find(|s| s.id == r.id) {
                if r.ip != sp.ip || r.name != sp.name {
                    (r.ip, r.name) = (sp.ip, sp.name.clone());
                    changed = true;
                }
            }
        }
        if changed {
            self.hw.store.save(&self.s);
        }
        self.sonos_list = found.iter().map(|s| Room { id: s.id.clone(), name: s.name.clone(), ip: s.ip }).collect();
        for r in &self.s.sonos_rooms {
            if !self.sonos_list.iter().any(|l| l.id == r.id) {
                self.sonos_list.push(r.clone());
            }
        }
        log::info!("sonos: {} speakers found, {} ticked", found.len(), self.s.sonos_rooms.len());
        self.hw.buttons.ignore_held();
        found
    }

    fn sonos_select(&mut self, sel: usize) {
        match sel {
            S_ON => {
                self.s.sonos_on = !self.s.sonos_on;
                self.hw.store.save(&self.s);
                self.dirty = true;
            }
            S_VOLUME | S_FAJR => self.open_sonos_volume(sel == S_FAJR),
            S_SPEAKERS => self.open_sonos_rooms(0),
            _ => self.go(Mode::Menu { sel: M_SONOS }),
        }
    }

    /// Speakers screen: tick/untick a speaker, search again, or go back.
    fn sonos_room_select(&mut self, sel: usize) {
        let rooms = self.sonos_list.len();
        if sel < rooms {
            let room = self.sonos_list[sel].clone();
            if let Some(at) = self.s.sonos_rooms.iter().position(|r| r.id == room.id) {
                self.s.sonos_rooms.remove(at);
            } else if self.s.sonos_rooms.len() < ROOMS_MAX {
                self.s.sonos_rooms.push(room);
                // Ticking a speaker means "play there": no second switch to remember.
                self.s.sonos_on = true;
            }
            self.hw.store.save(&self.s);
            self.dirty = true;
        } else if sel == rooms {
            self.open_sonos_rooms(usize::MAX);
        } else {
            self.go(Mode::Sonos { sel: S_SPEAKERS });
        }
    }

    /// Sonos volume step being edited/previewed (Fajr "same" previews the normal level).
    fn sonos_preview_volume(&self, fajr: bool) -> u8 {
        sonos::step_volume(self.s.sonos_step_for(if fajr { FAJR_SLOT } else { PREVIEW_SLOT }))
    }

    /// Sonos → Volume / Fajr volume: play a sample on the ticked speakers and adjust by ear.
    fn open_sonos_volume(&mut self, fajr: bool) {
        self.hw.buttons.ignore_held();
        if !self.s.sonos_rooms.is_empty() && !self.sonos_wifi {
            self.message("Sonos", &["Connecting to Sonos..."]);
            self.sonos_join();
        }
        if !self.s.sonos_rooms.is_empty() && self.sonos_wifi && self.hw.audio.available {
            self.hw.player.prepare(&self.s.sonos_rooms, self.hw.net.sta_ip().unwrap_or(std::net::Ipv4Addr::UNSPECIFIED));
            let t = Instant::now();
            while self.hw.player.state() == Sonos::Preparing && t.elapsed() < Duration::from_secs(6) {
                std::thread::sleep(Duration::from_millis(20));
            }
            if self.hw.player.state() == Sonos::Ready {
                self.hw.player.play(if fajr { FAJR_SLOT } else { PREVIEW_SLOT }, self.sonos_preview_volume(fajr));
            }
        }
        self.go(Mode::SonosVolume { fajr });
    }

    fn change_sonos_volume(&mut self, fajr: bool, delta: i8) {
        if fajr {
            self.s.sonos_fajr = fajr_step(self.s.sonos_step_for(FAJR_SLOT), self.s.sonos_volume, delta);
        } else {
            self.s.sonos_volume = (self.s.sonos_volume as i8 + delta).clamp(1, VOLUME_MAX as i8) as u8;
        }
        self.hw.player.volume(self.sonos_preview_volume(fajr));
        self.dirty = true;
    }

    // ───────────────────────────── setup ─────────────────────────────

    fn start_setup_ap(&mut self, from_menu: bool) {
        self.setup_from_menu = from_menu;
        self.message("Setup", &["Looking for Wi-Fi", "networks..."]);
        let nets = self.hw.net.scan();
        log::info!("scan: {} networks", nets.len());
        if let Err(e) = self.hw.net.start_setup_ap() {
            log::error!("setup AP: {e:?}");
        }
        let ip = self.hw.net.ap_ip();
        self.ap_ip = format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]);
        log::info!("setup AP {} at {}", self.hw.net.ap_ssid, self.ap_ip);
        match portal::start(nets, ip) {
            Ok(p) => self.portal = Some(p),
            Err(e) => log::error!("portal: {e:?}"),
        }
        self.wifi_connected_at = None;
        self.portal_status = "Waiting for phone...".into();
        self.go(Mode::SetupQr);
    }

    fn stop_setup_ap(&mut self) {
        self.portal = None;
        self.hw.net.stop();
    }

    fn tick_portal(&mut self) {
        let Some(portal) = &self.portal else { return };
        let shared = portal.shared.clone();
        let (req, city, opened, _probed) = {
            let mut g = shared.lock().unwrap();
            (g.wifi_request.take(), g.city_request.take(), g.page_opened, g.probed)
        };
        let status = if self.wifi_connected_at.is_some() {
            if self.setup_from_menu { "Connected." } else { "Now pick your city" }
        } else if opened {
            "Continue on your phone"
        } else if self.hw.net.ap_station_count() > 0 {
            "Phone joined"
        } else {
            "Waiting for phone..."
        };
        if status != self.portal_status {
            self.portal_status = status.into();
            self.dirty = true;
        }

        if let Some((ssid, pass)) = req {
            self.portal_status = format!("Connecting: {}", trunc(&ssid, 10));
            self.dirty = true;
            self.render();
            match self.hw.net.try_connect_keep_ap(&ssid, &pass) {
                Ok(()) => {
                    log::info!("setup: connected to {ssid}");
                    self.s.ssid = ssid.clone();
                    self.s.pass = pass;
                    self.hw.store.save(&self.s);
                    self.wifi_connected_at = Some(Instant::now());
                    shared.lock().unwrap().wifi = Some(WifiState::Connected(ssid));
                }
                Err(e) => {
                    log::warn!("setup: connect failed: {e:?}");
                    self.portal_status = match e {
                        net::ConnectError::WrongPassword => "Wrong password".into(),
                        net::ConnectError::NotFound => "Network not found".into(),
                        net::ConnectError::NoInternet => "No internet".into(),
                        net::ConnectError::Timeout => "Couldn't connect".into(),
                    };
                    shared.lock().unwrap().wifi = Some(WifiState::Failed(e.message().into()));
                }
            }
            self.dirty = true;
        }

        let finish = |app: &mut Self| {
            std::thread::sleep(Duration::from_secs(2)); // let the phone load the "All set" view
            app.stop_setup_ap();
            app.run_setup_sync();
        };
        if self.wifi_connected_at.is_some() && self.setup_from_menu {
            finish(self);
        } else if let Some(i) = city {
            self.set_city(i);
            finish(self);
        } else if self.wifi_connected_at.is_some_and(|t| t.elapsed() > Duration::from_secs(180)) {
            // Page closed before step 3: pick the city on the device.
            self.stop_setup_ap();
            self.go(Mode::City { sel: self.s.city, setup: true });
        }
    }

    fn set_city(&mut self, i: usize) {
        if i == self.s.city && !self.days.is_empty() {
            return;
        }
        self.s.city = i;
        self.hw.store.save(&self.s);
        clock::set_tz(self.city().tz_posix);
        self.days = self.hw.store.load_cache(i);
        self.last_adhan = None;
    }

    /// Setup step 4/5: Wi-Fi, clock, prayer times, rendered as a live checklist.
    fn run_setup_sync(&mut self) {
        let city = self.city().name.to_string();
        let mut rows = [Mark::Working, Mark::Todo, Mark::Todo];
        let mut detail = [trunc(&self.s.ssid, 12), String::new(), city.clone()];
        let show = |app: &mut Self, rows: [Mark; 3], detail: &[String; 3], failed: bool| {
            app.go(Mode::SetupSync { rows, detail: detail.clone(), failed });
            app.render();
        };
        show(self, rows, &detail, false);

        let mut failed = false;
        match self.sync_wifi() {
            Ok(()) => rows[0] = Mark::Done,
            Err(e) => {
                rows[0] = Mark::Failed;
                detail[0] = match e {
                    net::ConnectError::WrongPassword => "wrong password".into(),
                    net::ConnectError::NotFound => "not found".into(),
                    _ => "failed".into(),
                };
                failed = true;
            }
        }
        if !failed {
            rows[1] = Mark::Working;
            show(self, rows, &detail, false);
            match self.sync_clock() {
                Ok(()) => {
                    rows[1] = Mark::Done;
                    let l = clock::local_now();
                    detail[1] = format!("{:02}:{:02} {}", l.hour(), l.minute(), self.city().utc_label);
                }
                Err(_) => {
                    rows[1] = Mark::Failed;
                    detail[1] = "failed".into();
                    failed = true;
                }
            }
            rows[2] = Mark::Working;
            show(self, rows, &detail, false);
            match self.sync_times() {
                Ok(_) => rows[2] = Mark::Done,
                Err(e) => {
                    log::warn!("setup times: {e:?}");
                    rows[2] = Mark::Failed;
                    detail[2] = "failed".into();
                    failed = true;
                }
            }
        }
        self.hw.net.stop();
        if !failed {
            self.s.last_sync_ok = clock::now();
            self.hw.store.save(&self.s);
            self.next_sync = self.next_daily_sync();
        } else {
            self.next_sync = clock::now() + 300;
        }
        self.hw.buttons.ignore_held();
        show(self, rows, &detail, failed);
        if !failed {
            std::thread::sleep(Duration::from_millis(800));
            self.after_setup_sync();
        }
    }

    fn after_setup_sync(&mut self) {
        if self.setup_from_menu {
            self.s.setup_done = true;
            self.hw.store.save(&self.s);
            self.go(Mode::Home);
        } else {
            self.start_preview(false);
            self.go(Mode::Volume { setup: true, fajr: false });
        }
    }

    fn finish_setup(&mut self) {
        self.s.setup_done = true;
        self.hw.store.save(&self.s);
        self.go(Mode::SetupDone { until: Instant::now() + Duration::from_secs(3) });
    }

    fn start_preview(&mut self, fajr: bool) {
        let slot = if fajr { FAJR_SLOT } else { PREVIEW_SLOT };
        self.hw.audio.set_volume(self.s.volume_for(slot));
        if self.hw.audio.available {
            self.hw.audio.play(slot, Some(PREVIEW_MS));
        }
    }

    // ───────────────────────────── input ─────────────────────────────

    /// Button model (SPEC §5.4), by physical position:
    /// top press = up / louder, bottom press = down / quieter, hold top = select / done / confirm.
    /// The bottom button is the board's power key: it is never used for holds (except power-off).
    fn on_button(&mut self, ev: Event) {
        log::debug!("button {ev:?}");
        use Btn::*;
        use Event::*;
        // Global: power off (bottom held), emergency reset (top held 10 s).
        match ev {
            Held(Bottom, ms) if ms >= POWER_OFF_MS => return self.power_off(),
            Held(Top, ms) if ms >= EMERGENCY_START_MS && !matches!(self.mode, Mode::ResetHold { .. }) => {
                let pct = ((ms - EMERGENCY_START_MS) * 100 / (EMERGENCY_MS - EMERGENCY_START_MS)).min(100) as u8;
                if ms >= EMERGENCY_MS {
                    self.stop_setup_ap();
                    self.hw.buttons.ignore_held();
                    return self.go(Mode::ResetConfirm);
                }
                let prev = match &self.mode {
                    Mode::Emergency { prev, .. } => prev.clone(),
                    m => Box::new(m.clone()),
                };
                return self.go(Mode::Emergency { pct, prev });
            }
            Released(Top) => {
                if let Mode::Emergency { prev, .. } = &self.mode {
                    let p = (**prev).clone();
                    return self.go(p);
                }
            }
            _ => {}
        }

        let up = |i: usize, n: usize| (i + n - 1) % n;
        let down = |i: usize, n: usize| (i + 1) % n;
        match (self.mode.clone(), ev) {
            // Home: any press opens the menu
            (Mode::Home, Short(_)) => self.go(Mode::Menu { sel: 0 }),

            // Adhan playing: any button stops
            (Mode::Adhan { .. }, Short(_) | Long(_)) => {
                self.hw.player.stop();
                self.sonos_adhan = None;
                self.hw.audio.stop();
                self.hw.buttons.ignore_held();
                self.go(Mode::Home);
            }

            // Menu
            (Mode::Menu { sel }, Short(Top)) => self.go(Mode::Menu { sel: up(sel, MENU.len()) }),
            (Mode::Menu { sel }, Short(Bottom)) => self.go(Mode::Menu { sel: down(sel, MENU.len()) }),
            (Mode::Menu { sel }, Long(Top)) => match sel {
                M_CITY => self.go(Mode::City { sel: self.s.city, setup: false }),
                M_ADHAN => self.go(Mode::AdhanToggles { sel: 0 }),
                M_VOLUME | M_FAJR => {
                    self.start_preview(sel == M_FAJR);
                    self.go(Mode::Volume { setup: false, fajr: sel == M_FAJR });
                }
                M_SONOS => self.open_sonos(),
                M_TEST => self.test_adhan(),
                M_WIFI => self.go(Mode::Wifi { sel: 2 }),
                M_ABOUT => self.go(Mode::About),
                M_RESET => self.go(Mode::ResetMenu { sel: 0 }),
                _ => self.go(Mode::Home),
            },

            // City picker (holding top on the current city just goes back)
            (Mode::City { sel, setup }, Short(Top)) => self.go(Mode::City { sel: up(sel, CITIES.len()), setup }),
            (Mode::City { sel, setup }, Short(Bottom)) => self.go(Mode::City { sel: down(sel, CITIES.len()), setup }),
            (Mode::City { sel, setup }, Long(Top)) => {
                let changed = sel != self.s.city || self.days.is_empty();
                self.set_city(sel);
                self.hw.buttons.ignore_held();
                if setup {
                    self.run_setup_sync();
                } else if changed && !self.s.ssid.is_empty() {
                    self.message("Fetching times...", &[cities::get(sel).name]);
                    self.sync_all();
                    self.go(Mode::Home);
                } else {
                    self.go(Mode::Home);
                }
            }

            // Adhan on/off (row 5 = Back)
            (Mode::AdhanToggles { sel }, Short(Top)) => self.go(Mode::AdhanToggles { sel: up(sel, 6) }),
            (Mode::AdhanToggles { sel }, Short(Bottom)) => self.go(Mode::AdhanToggles { sel: down(sel, 6) }),
            (Mode::AdhanToggles { sel }, Long(Top)) => {
                if sel == 5 {
                    self.go(Mode::Menu { sel: M_ADHAN });
                } else {
                    self.s.adhan_mask ^= 1 << sel;
                    self.hw.store.save(&self.s);
                    self.dirty = true;
                }
            }

            // Volume (menu + setup step 5)
            (Mode::Volume { fajr, .. }, Short(Top)) => self.change_volume(fajr, 1),
            (Mode::Volume { fajr, .. }, Short(Bottom)) => self.change_volume(fajr, -1),
            (Mode::Volume { setup, fajr }, Long(Top)) => {
                self.hw.audio.stop();
                self.hw.store.save(&self.s);
                self.hw.buttons.ignore_held();
                if setup {
                    self.finish_setup();
                } else {
                    self.go(Mode::Menu { sel: if fajr { M_FAJR } else { M_VOLUME } });
                }
            }

            // Sonos
            (Mode::Sonos { sel }, Short(Top)) => self.go(Mode::Sonos { sel: up(sel, S_COUNT) }),
            (Mode::Sonos { sel }, Short(Bottom)) => self.go(Mode::Sonos { sel: down(sel, S_COUNT) }),
            (Mode::Sonos { sel }, Long(Top)) => self.sonos_select(sel),
            (Mode::SonosRooms { sel }, Short(Top)) => self.go(Mode::SonosRooms { sel: up(sel, self.sonos_list.len() + 2) }),
            (Mode::SonosRooms { sel }, Short(Bottom)) => self.go(Mode::SonosRooms { sel: down(sel, self.sonos_list.len() + 2) }),
            (Mode::SonosRooms { sel }, Long(Top)) => self.sonos_room_select(sel),
            (Mode::SonosVolume { fajr }, Short(Top)) => self.change_sonos_volume(fajr, 1),
            (Mode::SonosVolume { fajr }, Short(Bottom)) => self.change_sonos_volume(fajr, -1),
            (Mode::SonosVolume { fajr }, Long(Top)) => {
                self.hw.player.stop();
                self.hw.store.save(&self.s);
                self.hw.buttons.ignore_held();
                self.go(Mode::Sonos { sel: if fajr { S_FAJR } else { S_VOLUME } });
            }

            // Wi-Fi (rows 0–1 are info; 2 set up, 3 forget, 4 back)
            (Mode::Wifi { sel }, Short(Top)) => self.go(Mode::Wifi { sel: if sel <= 2 { 4 } else { sel - 1 } }),
            (Mode::Wifi { sel }, Short(Bottom)) => self.go(Mode::Wifi { sel: if sel >= 4 { 2 } else { sel + 1 } }),
            (Mode::Wifi { sel }, Long(Top)) => match sel {
                2 => self.start_setup_ap(true),
                3 if !self.s.ssid.is_empty() => self.go(Mode::WifiForget),
                _ => self.go(Mode::Menu { sel: M_WIFI }),
            },
            (Mode::WifiForget, Short(_)) => self.go(Mode::Wifi { sel: 3 }),
            (Mode::WifiForget, Long(Top)) => {
                self.s.ssid.clear();
                self.s.pass.clear();
                self.hw.store.save(&self.s);
                self.next_sync = i64::MAX;
                self.hw.buttons.ignore_held();
                self.go(Mode::Wifi { sel: 2 });
            }

            (Mode::About, Short(_) | Long(Top)) => self.go(Mode::Menu { sel: M_ABOUT }),
            (Mode::Message { .. }, Short(_)) => self.go(Mode::Home),

            // Reset (row 2 = Back)
            (Mode::ResetMenu { sel }, Short(Top)) => self.go(Mode::ResetMenu { sel: up(sel, 3) }),
            (Mode::ResetMenu { sel }, Short(Bottom)) => self.go(Mode::ResetMenu { sel: down(sel, 3) }),
            (Mode::ResetMenu { sel }, Long(Top)) => match sel {
                0 => {
                    self.hw.buttons.ignore_held();
                    self.go(Mode::ResetConfirm)
                }
                1 => self.restart(),
                _ => self.go(Mode::Menu { sel: M_RESET }),
            },
            (Mode::ResetConfirm, Short(_)) => self.go(if self.s.setup_done { Mode::Menu { sel: M_RESET } } else { Mode::SetupWelcome }),
            (Mode::ResetConfirm, Long(Top)) => {
                // Second step needs a fresh 2 s hold, so release first.
                self.hw.buttons.ignore_held();
                self.go(Mode::ResetHold { pct: 0 })
            }
            (Mode::ResetHold { .. }, Long(Top)) => self.go(Mode::ResetHold { pct: (800 * 100 / RESET_HOLD_MS) as u8 }),
            (Mode::ResetHold { .. }, Held(Top, ms)) => {
                if ms >= RESET_HOLD_MS {
                    return self.factory_reset();
                }
                self.go(Mode::ResetHold { pct: (ms * 100 / RESET_HOLD_MS) as u8 });
            }
            (Mode::ResetHold { .. }, Released(Top) | Short(_)) => self.go(Mode::ResetConfirm),

            // Setup
            (Mode::SetupWelcome, Short(_)) => self.start_setup_ap(false),
            (Mode::SetupQr, Long(Top)) => {
                self.stop_setup_ap();
                self.hw.buttons.ignore_held();
                self.go(if self.setup_from_menu { Mode::Wifi { sel: 2 } } else { Mode::SetupWelcome });
            }
            (Mode::SetupSync { failed: true, .. }, Short(Top)) => self.run_setup_sync(),
            (Mode::SetupSync { failed: true, .. }, Short(Bottom)) => self.after_setup_sync(),
            _ => {}
        }
    }

    fn change_volume(&mut self, fajr: bool, delta: i8) {
        if fajr {
            self.s.fajr_volume = fajr_step(self.s.volume_for(FAJR_SLOT), self.s.volume, delta);
        } else {
            self.s.volume = (self.s.volume as i8 + delta).clamp(1, VOLUME_MAX as i8) as u8;
        }
        self.hw.audio.set_volume(self.s.volume_for(if fajr { FAJR_SLOT } else { PREVIEW_SLOT }));
        if !self.hw.audio.is_playing() {
            self.start_preview(fajr);
        }
        self.dirty = true;
    }

    /// Menu → Test adhan: play the next prayer's adhan exactly as at prayer time.
    fn test_adhan(&mut self) {
        self.hw.buttons.ignore_held();
        if !self.hw.audio.available {
            return self.message("No adhan audio", &["Run `cargo xtask flash-audio`", "from the computer."]);
        }
        let now = clock::local_now();
        let (prayer, at) = schedule::next(now, &self.days)
            .filter(|n| n.prayer.has_adhan())
            .or_else(|| schedule::next(now, &self.days).and_then(|n| {
                // next is Sunrise: test with Dhuhr
                self.days.get(n.day_index).map(|d| schedule::Next { prayer: Prayer::Dhuhr, at: d.time(Prayer::Dhuhr), ..n })
            }))
            .map(|n| (n.prayer, n.at))
            .unwrap_or((Prayer::Dhuhr, now.minute_of_day()));
        log::info!("test adhan: {prayer:?}");
        if self.s.sonos_active() {
            // Same steps as before a real prayer, done on the spot.
            self.message("Test adhan", &["Connecting to Sonos..."]);
            if self.sonos_join() {
                self.hw.player.prepare(&self.s.sonos_rooms, self.hw.net.sta_ip().unwrap_or(std::net::Ipv4Addr::UNSPECIFIED));
                let t = Instant::now();
                while self.hw.player.state() == Sonos::Preparing && t.elapsed() < Duration::from_secs(8) {
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
        self.begin_adhan(prayer, at);
    }

    fn on_cmd(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Wifi { ssid, pass } => {
                self.s.ssid = ssid;
                self.s.pass = pass;
                self.s.setup_done = true;
                self.hw.store.save(&self.s);
                println!("ezan> ok wifi saved ({}); syncing", self.s.ssid);
                if self.portal.is_some() {
                    self.stop_setup_ap();
                }
                self.next_sync = clock::now();
                self.go(Mode::Home);
            }
            Cmd::City(key) => match cities::by_key(&key) {
                Some(i) => {
                    self.set_city(i);
                    self.days.clear();
                    self.next_sync = clock::now();
                    println!("ezan> ok city {}", self.city().name);
                    self.dirty = true;
                }
                None => {
                    let keys: Vec<_> = CITIES.iter().map(|c| c.key).collect();
                    println!("ezan> err unknown city; one of: {}", keys.join(", "));
                }
            },
            Cmd::Reset => {
                println!("ezan> ok resetting");
                self.factory_reset();
            }
            Cmd::Play(slot) => {
                println!("ezan> ok playing slot {slot} (available={})", self.hw.audio.available);
                self.hw.audio.set_volume(self.s.volume);
                self.hw.audio.play(slot, Some(PREVIEW_MS));
            }
            Cmd::Key(boot, long) => {
                let b = if boot { Btn::Top } else { Btn::Bottom };
                let ev = if long { Event::Long(b) } else { Event::Short(b) };
                println!("ezan> ok key {ev:?}");
                self.last_input = Instant::now();
                self.on_button(ev);
            }
            Cmd::Adhan => {
                println!("ezan> ok test adhan (sonos={})", self.s.sonos_active());
                self.test_adhan();
            }
            Cmd::AdhanIn(mins) => {
                // In memory only: a restart or the next sync brings the real times back.
                let now = clock::local_now();
                let at = now.minute_of_day() + mins;
                // Isha is the last time of the day, so moving it cannot reorder anything.
                match self.days.iter_mut().find(|d| d.date == now.date) {
                    Some(day) if at < 1440 => {
                        day.mins[Prayer::Isha as usize] = at;
                        self.last_adhan = None;
                        self.sonos_joined = None;
                        println!("ezan> ok Isha moved to {:02}:{:02} until restart", at / 60, at % 60);
                    }
                    _ => println!("ezan> err cannot move Isha today"),
                }
                self.dirty = true;
            }
            Cmd::Sonos(arg) => self.on_sonos_cmd(&arg),
            Cmd::Stop => {
                self.hw.player.stop();
                self.sonos_adhan = None;
                self.hw.audio.stop();
                println!("ezan> ok stopped");
            }
            Cmd::Setup => {
                println!("ezan> ok starting setup AP");
                self.start_setup_ap(self.s.setup_done);
            }
            Cmd::Sync => {
                println!("ezan> ok syncing");
                self.next_sync = clock::now();
            }
            Cmd::Status => {
                let now = clock::local_now();
                println!(
                    "ezan> ok v{VERSION} mode={:?} reset_reason={} uptime={}s city={} wifi={:?} clock_valid={} now={:?} days={} next={:?} next_sync_in={}s volume={} fajr_volume={} sonos={} sonos_volume={} sonos_fajr={} sonos_rooms={:?} sonos_state={:?} sonos_wifi={} heap_internal={}KB",
                    std::mem::discriminant(&self.mode),
                    unsafe { esp_idf_svc::sys::esp_reset_reason() },
                    unsafe { esp_idf_svc::sys::esp_timer_get_time() } / 1_000_000,
                    self.city().name,
                    self.s.ssid,
                    clock::valid(),
                    now,
                    self.days.len(),
                    schedule::next(now, &self.days).map(|n| (n.prayer, n.at)),
                    self.next_sync.saturating_sub(clock::now()),
                    self.s.volume,
                    self.s.fajr_volume,
                    self.s.sonos_on,
                    self.s.sonos_volume,
                    self.s.sonos_fajr,
                    self.s.sonos_rooms.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
                    self.hw.player.state(),
                    self.sonos_wifi,
                    unsafe { esp_idf_svc::sys::heap_caps_get_free_size(esp_idf_svc::sys::MALLOC_CAP_INTERNAL) } / 1024
                );
            }
        }
    }

    /// `ezan sonos scan | on | off | tick NAME | untick NAME | vol N | fajr N` (testing over USB).
    fn on_sonos_cmd(&mut self, arg: &str) {
        let (cmd, rest) = arg.split_once(' ').unwrap_or((arg, ""));
        let rest = rest.trim();
        match cmd {
            "scan" => {
                let t = Instant::now();
                let found = self.sonos_scan();
                for s in &found {
                    println!("ezan> speaker {:?} {} ticked={}", s.name, s.ip, self.s.sonos_rooms.iter().any(|r| r.id == s.id));
                }
                println!("ezan> ok {} speakers in {} ms", found.len(), t.elapsed().as_millis());
                // tick_sonos_wifi turns Wi-Fi back off when nothing else needs it
            }
            "nowifi" => {
                self.test_no_wifi = !self.test_no_wifi;
                println!("ezan> ok pretend Wi-Fi is down for Sonos: {} (until restart)", self.test_no_wifi);
            }
            "on" | "off" => {
                self.s.sonos_on = cmd == "on";
                self.hw.store.save(&self.s);
                println!("ezan> ok sonos {cmd}");
            }
            "tick" | "untick" => {
                let found = self.sonos_scan();
                let Some(sp) = found.iter().find(|s| s.name.eq_ignore_ascii_case(rest)) else {
                    return println!("ezan> err no speaker named {rest:?}");
                };
                self.s.sonos_rooms.retain(|r| r.id != sp.id);
                if cmd == "tick" && self.s.sonos_rooms.len() < ROOMS_MAX {
                    self.s.sonos_rooms.push(Room { id: sp.id.clone(), name: sp.name.clone(), ip: sp.ip });
                }
                self.hw.store.save(&self.s);
                println!("ezan> ok rooms {:?}", self.s.sonos_rooms.iter().map(|r| r.name.as_str()).collect::<Vec<_>>());
            }
            "vol" | "fajr" => {
                let n: u8 = rest.parse().unwrap_or(0).min(VOLUME_MAX);
                if cmd == "vol" {
                    self.s.sonos_volume = n.max(1);
                } else {
                    self.s.sonos_fajr = n;
                }
                self.hw.store.save(&self.s);
                println!("ezan> ok sonos volume {} fajr {}", self.s.sonos_volume, self.s.sonos_fajr);
            }
            _ => println!("ezan> err sonos scan|on|off|tick NAME|untick NAME|vol N|fajr N"),
        }
        self.dirty = true;
    }

    fn factory_reset(&mut self) {
        self.hw.player.stop();
        self.hw.audio.stop();
        self.message("Resetting...", &[]);
        self.hw.store.erase();
        std::thread::sleep(Duration::from_millis(500));
        self.restart();
    }

    fn restart(&mut self) -> ! {
        self.hw.epd.sleep();
        unsafe { esp_idf_svc::sys::esp_restart() };
        #[allow(unreachable_code)]
        loop {}
    }

    fn power_off(&mut self) {
        self.hw.player.stop();
        self.hw.audio.stop();
        self.message("Off", &["Hold the bottom button", "to turn on."]);
        self.hw.epd.sleep();
        std::thread::sleep(Duration::from_millis(300));
        self.hw.power.power_off();
        // Still running → on USB power. Any button press restarts.
        std::thread::sleep(Duration::from_secs(1));
        self.hw.buttons.ignore_held();
        loop {
            if let Some(Event::Short(_)) = self.hw.buttons.poll() {
                self.restart();
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    // ───────────────────────────── rendering ─────────────────────────────

    /// Draw a message immediately (used before blocking work).
    fn message(&mut self, title: &str, body: &[&str]) {
        self.go(Mode::Message { title: title.into(), body: body.iter().map(|s| s.to_string()).collect() });
        self.render();
    }

    fn render(&mut self) {
        let now = clock::local_now();
        if matches!(self.mode, Mode::Home) {
            let key = (now.date, now.minute_of_day());
            if self.rendered_minute != Some(key) {
                self.rendered_minute = Some(key);
                self.dirty = true;
            }
        }
        if !self.dirty {
            return;
        }
        self.dirty = false;
        self.draw(now);

        let mode_changed = self.shown_mode.as_ref().map(std::mem::discriminant) != Some(std::mem::discriminant(&self.mode));
        let full = (mode_changed && self.mode.wants_full())
            || (matches!(self.mode, Mode::Home) && self.last_full.elapsed() > Duration::from_secs(3600));
        if full {
            self.hw.epd.full(&self.frame);
            self.last_full = Instant::now();
        } else if self.frame.buf != self.shown.buf {
            self.hw.epd.partial(&self.frame);
        }
        self.shown.buf.copy_from_slice(&self.frame.buf);
        self.shown_mode = Some(self.mode.clone());
    }

    fn draw(&mut self, now: ezan::LocalTime) {
        let status = self.status();
        let f = &mut self.frame;
        match &self.mode {
            Mode::Home => {
                let (next, grid) = if clock::valid() {
                    let n = schedule::next(now, &self.days);
                    (n, n.map(|n| &self.days[n.day_index]))
                } else {
                    (None, None)
                };
                ui::home(f, &Home { city: cities::get(self.s.city).name, now, next, grid, status });
            }
            Mode::Adhan { prayer, at } => ui::adhan(f, *prayer, *at),
            Mode::Menu { sel } => {
                let vol = format!("{}/{}", self.s.volume, VOLUME_MAX);
                let adhan = if self.s.adhan_mask == 0 { "Off" } else if self.s.adhan_mask == 0b11111 { "On" } else { "Some" };
                let wifi = if self.s.ssid.is_empty() { "-".to_string() } else { trunc(&self.s.ssid, 11) };
                let city = cities::get(self.s.city).name;
                let fajr = if self.s.fajr_volume == 0 { "Same".to_string() } else { format!("{}/{}", self.s.fajr_volume, VOLUME_MAX) };
                let sonos = match (self.s.sonos_on, self.s.sonos_rooms.len()) {
                    (false, _) => "Off".to_string(),
                    (true, 0) => "No rooms".to_string(),
                    (true, 1) => "1 room".to_string(),
                    (true, n) => format!("{n} rooms"),
                };
                let vals: [&str; 10] = [city, adhan, &vol, &fajr, &sonos, "", &wifi, "", "", ""];
                ui::list(f, "Menu", MENU.len(), *sel, None, |i| (MENU[i], vals[i]), SELECT_HINT);
            }
            Mode::City { sel, .. } => {
                ui::list(f, "City", CITIES.len(), *sel, Some(self.s.city), |i| (CITIES[i].name, CITIES[i].utc_label), SELECT_HINT)
            }
            Mode::AdhanToggles { sel } => {
                let names = ["Fajr", "Dhuhr", "Asr", "Maghrib", "Isha", "Back"];
                let mask = self.s.adhan_mask;
                ui::list(
                    f,
                    "Adhan",
                    6,
                    *sel,
                    None,
                    |i| (names[i], if i == 5 { "" } else if mask & (1 << i) != 0 { "On" } else { "Off" }),
                    "hold top: toggle",
                )
            }
            Mode::Volume { fajr: true, .. } => ui::level_bars(
                f,
                "Fajr volume",
                None,
                if self.hw.audio.available { "Playing Fajr adhan sample..." } else { "" },
                if self.s.fajr_volume == 0 { "Same as other prayers" } else { "Own volume for Fajr" },
                self.s.volume_for(FAJR_SLOT),
                VOLUME_MAX,
                ["Top louder, bottom quieter", &format!("Other prayers: {}/{}", self.s.volume, VOLUME_MAX)],
                "hold top: done",
            ),
            Mode::Volume { setup, .. } => ui::volume(
                f,
                if *setup { "Speaker check" } else { "Volume" },
                if *setup { Some("5/5") } else { None },
                self.s.volume,
                VOLUME_MAX,
                self.hw.audio.available,
                "hold top: done",
            ),
            Mode::Sonos { sel } => {
                let vol = format!("{}/{}", self.s.sonos_volume, VOLUME_MAX);
                let fajr = if self.s.sonos_fajr == 0 { "Same".to_string() } else { format!("{}/{}", self.s.sonos_fajr, VOLUME_MAX) };
                let speakers = match self.s.sonos_rooms.as_slice() {
                    [] => "None".to_string(),
                    [one] => trunc(&one.name, 11),
                    many => format!("{} on", many.len()),
                };
                let rows: [(&str, &str); S_COUNT] = [
                    ("Sonos", if self.s.sonos_on { "On" } else { "Off" }),
                    ("Volume", &vol),
                    ("Fajr volume", &fajr),
                    ("Speakers", &speakers),
                    ("Back", ""),
                ];
                ui::list(f, "Sonos", S_COUNT, *sel, None, |i| rows[i], SELECT_HINT)
            }
            Mode::SonosRooms { sel } => {
                let n = self.sonos_list.len();
                let names: Vec<String> = self.sonos_list.iter().map(|r| trunc(&r.name, 14)).collect();
                let (rooms, list) = (&self.s.sonos_rooms, &self.sonos_list);
                ui::list(
                    f,
                    "Speakers",
                    n + 2,
                    *sel,
                    None,
                    |i| match i {
                        i if i < n => (names[i].as_str(), if rooms.iter().any(|r| r.id == list[i].id) { "On" } else { "Off" }),
                        i if i == n => (if n == 0 { "None found: search" } else { "Search again" }, ""),
                        _ => ("Back", ""),
                    },
                    "hold top: on/off",
                )
            }
            Mode::SonosVolume { fajr } => {
                let status = match self.hw.player.state() {
                    _ if self.s.sonos_rooms.is_empty() => "No speakers ticked.",
                    Sonos::Starting | Sonos::Playing | Sonos::Preparing | Sonos::Ready => "Playing on Sonos...",
                    _ => "Not playing on Sonos.",
                };
                let label = match (*fajr, self.s.sonos_fajr) {
                    (false, _) => "Sonos volume",
                    (true, 0) => "Same as other prayers",
                    (true, _) => "Own volume for Fajr",
                };
                let others = format!("Other prayers: {}/{}", self.s.sonos_volume, VOLUME_MAX);
                let help = if *fajr { others.as_str() } else { "Applies to every ticked speaker." };
                ui::level_bars(
                    f,
                    if *fajr { "Sonos: Fajr" } else { "Sonos volume" },
                    None,
                    status,
                    label,
                    self.s.sonos_step_for(if *fajr { FAJR_SLOT } else { PREVIEW_SLOT }),
                    VOLUME_MAX,
                    ["Top louder, bottom quieter", help],
                    "hold top: done",
                )
            }
            Mode::Wifi { sel } => {
                let net = if self.s.ssid.is_empty() { "none".to_string() } else { trunc(&self.s.ssid, 13) };
                let last = if self.s.last_sync_ok == 0 {
                    "never".to_string()
                } else {
                    let l = clock::local(self.s.last_sync_ok);
                    format!("{} {} {:02}:{:02}", l.date.d, ezan::strings::MONTHS[(l.date.m as usize + 11) % 12], l.hour(), l.minute())
                };
                let rows: [(&str, &str); 5] = [
                    ("Network", &net),
                    ("Last sync", &last),
                    ("Set up new network", ""),
                    ("Forget network", ""),
                    ("Back", ""),
                ];
                ui::list(f, "Wi-Fi", 5, *sel, None, |i| rows[i], SELECT_HINT)
            }
            Mode::WifiForget => {
                let q = format!("Forget {}?", trunc(&self.s.ssid, 14));
                ui::message(f, "Wi-Fi", None, &[&q, "", "AdhanBox keeps running", "offline from its clock", "and saved times."], "press: cancel", "hold top: forget")
            }
            Mode::About => {
                let l1 = format!("Firmware {VERSION}");
                let l2 = format!("City: {}", cities::get(self.s.city).name);
                let l3 = format!("Times cached: {} days", schedule::days_left(now, &self.days));
                let l4 = format!("Audio: {}", if self.hw.audio.available { "ok" } else { "missing" });
                let l5 = format!("Clock: {}", if clock::valid() { "set" } else { "not set" });
                ui::message(f, "About", None, &[&l1, &l2, &l3, &l4, &l5], "", "any button: back")
            }
            Mode::ResetMenu { sel } => {
                let rows = [("Reset settings", ""), ("Restart", ""), ("Back", "")];
                ui::list(f, "Reset", 3, *sel, None, |i| rows[i], SELECT_HINT)
            }
            Mode::ResetConfirm => ui::message(
                f,
                "Reset settings?",
                None,
                &["Wi-Fi, city, volume and", "saved prayer times will", "be erased.", "", "Adhan recordings stay."],
                "press: cancel",
                "hold top: continue",
            ),
            Mode::ResetHold { pct } => ui::hold(f, "Are you sure?", &["Hold the top button", "for 2 seconds to reset."], *pct, "Release to cancel"),
            Mode::Emergency { pct, .. } => ui::hold(f, "Emergency reset", &["Keep holding the top", "button to open reset."], *pct, "Release to cancel"),
            Mode::SetupWelcome => ui::welcome(f),
            Mode::SetupQr => ui::setup_qr(f, &self.hw.net.ap_ssid, &self.ap_ip, &self.portal_status),
            Mode::SetupSync { rows, detail, failed } => ui::checklist(
                f,
                "Setting up",
                Some("4/5"),
                &[(rows[0], "Wi-Fi", &detail[0]), (rows[1], "Clock", &detail[1]), (rows[2], "Prayer times", &detail[2])],
                if *failed { "bottom: skip" } else { "" },
                if *failed { "top: retry" } else { "" },
            ),
            Mode::SetupDone { .. } => {
                let next = schedule::next(now, &self.days)
                    .map(|n| format!("Next: {} at {:02}:{:02}", ezan::strings::prayer_name(n.prayer), n.at / 60, n.at % 60))
                    .unwrap_or_default();
                ui::message(f, "You're set", None, &[&next, "", "Press any button", "for the menu."], "", "")
            }
            Mode::Message { title, body } => {
                let b: Vec<&str> = body.iter().map(String::as_str).collect();
                ui::message(f, title, None, &b, "", "")
            }
        }
    }
}

/// One press on a Fajr volume screen. It starts from the level Fajr plays at now (the normal
/// level while it is "same"), so both buttons work from the first press. Landing on the normal
/// level stores 0 = "same as the other prayers".
fn fajr_step(current: u8, normal: u8, delta: i8) -> u8 {
    let v = (current as i8 + delta).clamp(1, VOLUME_MAX as i8) as u8;
    if v == normal { 0 } else { v }
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(n.saturating_sub(2)).collect();
        t.push_str("..");
        t
    }
}
