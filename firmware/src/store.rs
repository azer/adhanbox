//! Persistent settings and the prayer-time cache (NVS namespace "ezan").
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use ezan::prayer::{decode_days, encode_days};
use ezan::DayTimes;

pub const VOLUME_MAX: u8 = 6;
pub const ADHAN_ALL: u8 = 0b1_1111;

#[derive(Clone, Debug)]
pub struct Settings {
    pub ssid: String,
    pub pass: String,
    pub city: usize,
    pub volume: u8,
    pub adhan_mask: u8,
    pub setup_done: bool,
    pub last_sync_ok: i64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            ssid: String::new(),
            pass: String::new(),
            city: ezan::cities::DEFAULT_CITY,
            volume: 4,
            adhan_mask: ADHAN_ALL,
            setup_done: false,
            last_sync_ok: 0,
        }
    }
}

pub struct Store {
    nvs: EspNvs<NvsDefault>,
}

const KEYS: [&str; 9] = ["ssid", "pass", "city", "volume", "adhan", "setup", "lastsync", "cache", "cachecity"];

impl Store {
    pub fn new(part: EspDefaultNvsPartition) -> anyhow::Result<Self> {
        Ok(Self { nvs: EspNvs::new(part, "ezan", true)? })
    }

    fn get_string(&self, k: &str) -> String {
        let mut buf = [0u8; 128];
        self.nvs.get_str(k, &mut buf).ok().flatten().unwrap_or("").to_string()
    }

    pub fn load(&self) -> Settings {
        let d = Settings::default();
        let city_key = self.get_string("city");
        Settings {
            ssid: self.get_string("ssid"),
            pass: self.get_string("pass"),
            city: ezan::cities::by_key(&city_key).unwrap_or(d.city),
            volume: self.nvs.get_u8("volume").ok().flatten().unwrap_or(d.volume).clamp(1, VOLUME_MAX),
            adhan_mask: self.nvs.get_u8("adhan").ok().flatten().unwrap_or(d.adhan_mask),
            setup_done: self.nvs.get_u8("setup").ok().flatten().unwrap_or(0) == 1,
            last_sync_ok: self.nvs.get_i64("lastsync").ok().flatten().unwrap_or(0),
        }
    }

    pub fn save(&mut self, s: &Settings) {
        let city = ezan::cities::get(s.city).key;
        let r = (|| -> anyhow::Result<()> {
            self.nvs.set_str("ssid", &s.ssid)?;
            self.nvs.set_str("pass", &s.pass)?;
            self.nvs.set_str("city", city)?;
            self.nvs.set_u8("volume", s.volume)?;
            self.nvs.set_u8("adhan", s.adhan_mask)?;
            self.nvs.set_u8("setup", s.setup_done as u8)?;
            self.nvs.set_i64("lastsync", s.last_sync_ok)?;
            Ok(())
        })();
        if let Err(e) = r {
            log::error!("settings save failed: {e:?}");
        }
    }

    /// Cached days, only if they belong to `city`.
    pub fn load_cache(&self, city: usize) -> Vec<DayTimes> {
        if self.get_string("cachecity") != ezan::cities::get(city).key {
            return Vec::new();
        }
        let mut buf = vec![0u8; DayTimes::ENCODED_LEN * 64];
        let Ok(Some(blob)) = self.nvs.get_blob("cache", &mut buf) else { return Vec::new() };
        let mut days = vec![DayTimes::default(); blob.len() / DayTimes::ENCODED_LEN];
        let n = decode_days(blob, &mut days);
        days.truncate(n);
        days
    }

    pub fn save_cache(&mut self, city: usize, days: &[DayTimes]) {
        let _ = self.nvs.set_blob("cache", &encode_days(days));
        let _ = self.nvs.set_str("cachecity", ezan::cities::get(city).key);
    }

    /// Reset settings: everything in the namespace.
    pub fn erase(&mut self) {
        for k in KEYS {
            let _ = self.nvs.remove(k);
        }
    }
}
