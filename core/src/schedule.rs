//! "What's next and how long until it" — the heart of the device.
use crate::date::{Date, LocalTime};
use crate::prayer::{DayTimes, Prayer};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Next {
    pub prayer: Prayer,
    pub date: Date,
    /// Minutes since midnight on `date`.
    pub at: u16,
    /// Seconds from `now` until the event (> 0).
    pub remaining_s: u32,
    /// Index into the `days` slice the event came from.
    pub day_index: usize,
}

/// First event strictly after `now`. `days` must be sorted by date.
pub fn next(now: LocalTime, days: &[DayTimes]) -> Option<Next> {
    let now_days = now.date.to_days() as i64;
    let now_abs = now_days * 86_400 + now.sec as i64;
    for (i, day) in days.iter().enumerate() {
        let d = day.date.to_days() as i64;
        if d < now_days {
            continue;
        }
        for p in Prayer::ALL {
            let t = d * 86_400 + day.time(p) as i64 * 60;
            if t > now_abs {
                return Some(Next {
                    prayer: p,
                    date: day.date,
                    at: day.time(p),
                    remaining_s: (t - now_abs) as u32,
                    day_index: i,
                });
            }
        }
    }
    None
}

/// The prayer whose minute is `now`'s minute today (adhan trigger), if any.
pub fn due(now: LocalTime, days: &[DayTimes]) -> Option<Prayer> {
    let today = days.iter().find(|d| d.date == now.date)?;
    let m = now.minute_of_day();
    Prayer::ALL.into_iter().find(|&p| today.time(p) == m)
}

/// Today's entry, if cached.
pub fn today(now: LocalTime, days: &[DayTimes]) -> Option<&DayTimes> {
    days.iter().find(|d| d.date == now.date)
}

/// Days of cache left from today (inclusive).
pub fn days_left(now: LocalTime, days: &[DayTimes]) -> usize {
    days.iter().filter(|d| d.date >= now.date).count()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Countdown {
    /// ≥ 1 hour: h:mm
    HoursMinutes(u32, u32),
    /// < 1 hour: N min
    Minutes(u32),
    /// < 1 minute
    Now,
}

/// Rounds up to whole minutes, so "1 min" shows until the very last minute.
pub fn countdown(remaining_s: u32) -> Countdown {
    if remaining_s < 60 {
        return Countdown::Now;
    }
    let mins = remaining_s.div_ceil(60);
    if mins >= 60 {
        Countdown::HoursMinutes(mins / 60, mins % 60)
    } else {
        Countdown::Minutes(mins)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prayer::Hijri;

    fn day(date: Date, t: [(u16, u16); 6]) -> DayTimes {
        let mut mins = [0; 6];
        for (i, (h, m)) in t.iter().enumerate() {
            mins[i] = h * 60 + m;
        }
        DayTimes { date, mins, hijri: Hijri::default() }
    }

    fn days() -> [DayTimes; 2] {
        let d1 = Date::new(2026, 11, 16);
        let d2 = d1.add_days(1);
        [
            day(d1, [(6, 17), (7, 48), (12, 53), (15, 42), (17, 58), (19, 22)]),
            day(d2, [(6, 18), (7, 49), (12, 53), (15, 41), (17, 57), (19, 21)]),
        ]
    }

    #[test]
    fn next_in_day() {
        let ds = days();
        let n = next(LocalTime::new(ds[0].date, 13, 28, 1), &ds).unwrap();
        assert_eq!(n.prayer, Prayer::Asr);
        assert_eq!(countdown(n.remaining_s), Countdown::HoursMinutes(2, 14));
    }

    #[test]
    fn exact_minute_moves_on() {
        let ds = days();
        let n = next(LocalTime::new(ds[0].date, 15, 42, 0), &ds).unwrap();
        assert_eq!(n.prayer, Prayer::Maghrib);
        assert_eq!(due(LocalTime::new(ds[0].date, 15, 42, 30), &ds), Some(Prayer::Asr));
    }

    #[test]
    fn after_isha_rolls_to_tomorrow() {
        let ds = days();
        let n = next(LocalTime::new(ds[0].date, 23, 0, 0), &ds).unwrap();
        assert_eq!((n.prayer, n.date, n.day_index), (Prayer::Fajr, ds[1].date, 1));
        assert_eq!(countdown(n.remaining_s), Countdown::HoursMinutes(7, 18));
    }

    #[test]
    fn countdown_format() {
        assert_eq!(countdown(59), Countdown::Now);
        assert_eq!(countdown(60), Countdown::Minutes(1));
        assert_eq!(countdown(61), Countdown::Minutes(2));
        assert_eq!(countdown(3540), Countdown::Minutes(59));
        assert_eq!(countdown(3541), Countdown::HoursMinutes(1, 0));
    }

    #[test]
    fn end_of_cache() {
        let ds = days();
        assert!(next(LocalTime::new(ds[1].date, 20, 0, 0), &ds).is_none());
        assert_eq!(days_left(LocalTime::new(ds[1].date, 0, 0, 0), &ds), 1);
    }

    #[test]
    fn encode_roundtrip() {
        let d = days()[0];
        assert_eq!(DayTimes::decode(&d.encode()), Some(d));
    }
}
