//! All user-facing text (English for now; one place to translate later).
use crate::prayer::Prayer;

pub fn prayer_name(p: Prayer) -> &'static str {
    match p {
        Prayer::Fajr => "Fajr",
        Prayer::Sunrise => "Sunrise",
        Prayer::Dhuhr => "Dhuhr",
        Prayer::Asr => "Asr",
        Prayer::Maghrib => "Maghrib",
        Prayer::Isha => "Isha",
    }
}

pub fn prayer_upper(p: Prayer) -> &'static str {
    match p {
        Prayer::Fajr => "FAJR",
        Prayer::Sunrise => "SUNRISE",
        Prayer::Dhuhr => "DHUHR",
        Prayer::Asr => "ASR",
        Prayer::Maghrib => "MAGHRIB",
        Prayer::Isha => "ISHA",
    }
}

pub const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
pub const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
pub const HIJRI_MONTHS: [&str; 12] = [
    "Muharram",
    "Safar",
    "Rabi al-Awwal",
    "Rabi al-Thani",
    "Jumada al-Ula",
    "Jumada al-Akhirah",
    "Rajab",
    "Sha'ban",
    "Ramadan",
    "Shawwal",
    "Dhu al-Qa'dah",
    "Dhu al-Hijjah",
];

pub const IN: &str = "IN";
pub const MIN: &str = "min";
pub const NOW: &str = "Now";
pub const NOW_LOWER: &str = "now";
pub const LEFT: &str = "left";
pub const AT: &str = "at";
pub const ADHAN_PLAYING: &str = "adhan playing";
pub const ANY_BUTTON_STOP: &str = "any button: stop";

/// Right side of the hint bar on list screens.
pub const SELECT_HINT: &str = "hold top: select";
