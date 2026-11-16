//! The device state machine: screens, buttons, sync schedule, adhan trigger.
//! UX reference: SPEC.md §5.
use crate::audio::Audio;
use crate::board::Power;
use crate::buttons::{Btn, Buttons, Event};
use crate::console::Cmd;
use crate::epd::Epd;
use crate::net::{self, Net};
use crate::portal::{self, Portal, WifiState};
use crate::store::{Settings, Store, VOLUME_MAX};
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
const MENU: [&str; 8] = ["City", "Adhan", "Volume", "Test adhan", "Wi-Fi", "About", "Reset", "Back"];
const M_CITY: usize = 0;
const M_ADHAN: usize = 1;
const M_VOLUME: usize = 2;
const M_TEST: usize = 3;
const M_WIFI: usize = 4;
const M_ABOUT: usize = 5;
const M_RESET: usize = 6;
const M_BACK: usize = 7;
const PREVIEW_SLOT: u8 = 1; // Dhuhr
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
    Volume { setup: bool },
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
            || matches!(self, Mode::City { setup: true, .. } | Mode::Volume { setup: true })
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
    pub console: Receiver<Cmd>,
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
            Mode::Adhan { .. } if !self.hw.audio.is_playing() => {
                self.hw.buttons.ignore_held();
                self.go(Mode::Home);
            }
            _ => {}
        }

        if self.mode.is_setup() {
            return;
        }

        // Adhan trigger
        if clock::valid() && !matches!(self.mode, Mode::Adhan { .. } | Mode::Volume { .. }) {
            let now = clock::local_now();
            if let Some(p) = schedule::due(now, &self.days) {
                let key = (now.date, p);
                if self.last_adhan != Some(key) {
                    self.last_adhan = Some(key);
                    let enabled = p.adhan_bit().map(|b| self.s.adhan_mask & (1 << b) != 0).unwrap_or(false);
                    if enabled && self.hw.audio.available {
                        log::info!("adhan: {p:?}");
                        let at = now.minute_of_day();
                        self.hw.audio.set_volume(self.s.volume);
                        self.hw.audio.play(p.adhan_bit().unwrap(), None);
                        self.go(Mode::Adhan { prayer: p, at });
                    }
                }
            }
        }

        // Daily sync (never during adhan)
        if !matches!(self.mode, Mode::Adhan { .. }) && clock::now() >= self.next_sync {
            self.sync_all();
        }
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
            self.start_preview();
            self.go(Mode::Volume { setup: true });
        }
    }

    fn finish_setup(&mut self) {
        self.s.setup_done = true;
        self.hw.store.save(&self.s);
        self.go(Mode::SetupDone { until: Instant::now() + Duration::from_secs(3) });
    }

    fn start_preview(&mut self) {
        self.hw.audio.set_volume(self.s.volume);
        if self.hw.audio.available {
            self.hw.audio.play(PREVIEW_SLOT, Some(PREVIEW_MS));
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
                M_VOLUME => {
                    self.start_preview();
                    self.go(Mode::Volume { setup: false });
                }
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
            (Mode::Volume { .. }, Short(Top)) => self.change_volume(1),
            (Mode::Volume { .. }, Short(Bottom)) => self.change_volume(-1),
            (Mode::Volume { setup }, Long(Top)) => {
                self.hw.audio.stop();
                self.hw.store.save(&self.s);
                self.hw.buttons.ignore_held();
                if setup {
                    self.finish_setup();
                } else {
                    self.go(Mode::Menu { sel: M_VOLUME });
                }
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

    fn change_volume(&mut self, delta: i8) {
        self.s.volume = (self.s.volume as i8 + delta).clamp(1, VOLUME_MAX as i8) as u8;
        self.hw.audio.set_volume(self.s.volume);
        if !self.hw.audio.is_playing() {
            self.start_preview();
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
        self.hw.audio.set_volume(self.s.volume);
        self.hw.audio.play(prayer.adhan_bit().unwrap_or(1), None);
        self.go(Mode::Adhan { prayer, at });
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
            Cmd::Stop => {
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
                    "ezan> ok v{VERSION} mode={:?} reset_reason={} uptime={}s city={} wifi={:?} clock_valid={} now={:?} days={} next={:?} next_sync_in={}s",
                    std::mem::discriminant(&self.mode),
                    unsafe { esp_idf_svc::sys::esp_reset_reason() },
                    unsafe { esp_idf_svc::sys::esp_timer_get_time() } / 1_000_000,
                    self.city().name,
                    self.s.ssid,
                    clock::valid(),
                    now,
                    self.days.len(),
                    schedule::next(now, &self.days).map(|n| (n.prayer, n.at)),
                    self.next_sync.saturating_sub(clock::now())
                );
            }
        }
    }

    fn factory_reset(&mut self) {
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
                let vals: [&str; 8] = [city, adhan, &vol, "", &wifi, "", "", ""];
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
            Mode::Volume { setup } => ui::volume(
                f,
                if *setup { "Speaker check" } else { "Volume" },
                if *setup { Some("5/5") } else { None },
                self.s.volume,
                VOLUME_MAX,
                self.hw.audio.available,
                "hold top: done",
            ),
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

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(n.saturating_sub(2)).collect();
        t.push_str("..");
        t
    }
}
