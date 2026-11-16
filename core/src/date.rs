//! Minimal civil-date math (proleptic Gregorian), enough for a prayer clock.

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct Date {
    pub y: u16,
    pub m: u8,
    pub d: u8,
}

impl Date {
    pub const fn new(y: u16, m: u8, d: u8) -> Self {
        Self { y, m, d }
    }

    /// Days since 1970-01-01 (Howard Hinnant's algorithm).
    pub fn to_days(self) -> i32 {
        let y = self.y as i32 - if self.m <= 2 { 1 } else { 0 };
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let yoe = y - era * 400;
        let m = self.m as i32;
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + self.d as i32 - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146097 + doe - 719468
    }

    pub fn from_days(z: i32) -> Self {
        let z = z + 719468;
        let era = if z >= 0 { z } else { z - 146096 } / 146097;
        let doe = z - era * 146097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
        let m = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
        Self { y: (y + if m <= 2 { 1 } else { 0 }) as u16, m, d }
    }

    pub fn add_days(self, n: i32) -> Self {
        Self::from_days(self.to_days() + n)
    }

    /// 0 = Monday … 6 = Sunday.
    pub fn weekday(self) -> u8 {
        ((self.to_days() + 3).rem_euclid(7)) as u8
    }
}

/// Wall-clock time in the selected city.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct LocalTime {
    pub date: Date,
    /// Seconds since local midnight, 0..86400.
    pub sec: u32,
}

impl LocalTime {
    pub fn new(date: Date, h: u32, m: u32, s: u32) -> Self {
        Self { date, sec: h * 3600 + m * 60 + s }
    }
    pub fn hour(&self) -> u32 {
        self.sec / 3600
    }
    pub fn minute(&self) -> u32 {
        self.sec / 60 % 60
    }
    pub fn minute_of_day(&self) -> u16 {
        (self.sec / 60) as u16
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip() {
        for z in [-1000, 0, 10_000, 20_408, 30_000] {
            assert_eq!(Date::from_days(z).to_days(), z);
        }
        assert_eq!(Date::new(2026, 11, 16).weekday(), 0); // Monday
        assert_eq!(Date::new(2026, 12, 31).add_days(1), Date::new(2027, 1, 1));
        assert_eq!(Date::new(2028, 2, 28).add_days(1), Date::new(2028, 2, 29));
    }
}
