//! System clock (UTC) + POSIX TZ for the selected city.
use esp_idf_svc::sys;
use ezan::{Date, LocalTime};

pub fn set_tz(posix: &str) {
    std::env::set_var("TZ", posix);
    unsafe { sys::tzset() };
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn set_now(unix: i64) {
    let tv = sys::timeval { tv_sec: unix as _, tv_usec: 0 };
    unsafe { sys::settimeofday(&tv, core::ptr::null()) };
}

const BUILD_UNIX: &str = env!("EZAN_BUILD_UNIX");

/// Clock is plausible: not earlier than this firmware's build time.
pub fn valid() -> bool {
    now() >= BUILD_UNIX.parse::<i64>().unwrap_or(0) - 86_400
}

pub fn local(unix: i64) -> LocalTime {
    let t: sys::time_t = unix as _;
    let mut tm: sys::tm = unsafe { core::mem::zeroed() };
    unsafe { sys::localtime_r(&t, &mut tm) };
    LocalTime::new(
        Date::new((tm.tm_year + 1900) as u16, (tm.tm_mon + 1) as u8, tm.tm_mday as u8),
        tm.tm_hour as u32,
        tm.tm_min as u32,
        tm.tm_sec as u32,
    )
}

pub fn local_now() -> LocalTime {
    local(now())
}
