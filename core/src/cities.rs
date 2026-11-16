//! The city list. `cities_gen.rs` is generated from `cities.toml` by `cargo xtask cities`.

#[derive(Debug)]
pub struct City {
    pub key: &'static str,
    pub name: &'static str,
    pub tz_iana: &'static str,
    /// POSIX TZ string (includes DST rules), for `setenv("TZ")`.
    pub tz_posix: &'static str,
    /// Diyanet district (ilçe) ID.
    pub diyanet_id: u32,
    /// Standard UTC offset label, e.g. "UTC+3", "UTC+5:30".
    pub utc_label: &'static str,
}

include!("cities_gen.rs");

pub fn by_key(key: &str) -> Option<usize> {
    CITIES.iter().position(|c| c.key == key)
}

pub fn get(i: usize) -> &'static City {
    CITIES.get(i).unwrap_or(&CITIES[DEFAULT_CITY])
}
