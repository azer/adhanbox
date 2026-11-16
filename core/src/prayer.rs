use crate::date::Date;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Prayer {
    Fajr = 0,
    Sunrise = 1,
    Dhuhr = 2,
    Asr = 3,
    Maghrib = 4,
    Isha = 5,
}

impl Prayer {
    pub const ALL: [Prayer; 6] = [
        Prayer::Fajr,
        Prayer::Sunrise,
        Prayer::Dhuhr,
        Prayer::Asr,
        Prayer::Maghrib,
        Prayer::Isha,
    ];

    pub fn from_index(i: usize) -> Prayer {
        Self::ALL[i]
    }

    /// Sunrise is a time, not a prayer: no adhan.
    pub fn has_adhan(self) -> bool {
        self != Prayer::Sunrise
    }

    /// Bit in the adhan on/off mask (Sunrise has none).
    pub fn adhan_bit(self) -> Option<u8> {
        match self {
            Prayer::Fajr => Some(0),
            Prayer::Sunrise => None,
            Prayer::Dhuhr => Some(1),
            Prayer::Asr => Some(2),
            Prayer::Maghrib => Some(3),
            Prayer::Isha => Some(4),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Hijri {
    pub d: u8,
    pub m: u8,
    pub y: u16,
}

/// One day of Diyanet times, as local wall-clock minutes since midnight.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct DayTimes {
    pub date: Date,
    pub mins: [u16; 6],
    pub hijri: Hijri,
}

impl DayTimes {
    pub const ENCODED_LEN: usize = 20;

    pub fn time(&self, p: Prayer) -> u16 {
        self.mins[p as usize]
    }

    /// Compact little-endian encoding for the on-device cache.
    pub fn encode(&self) -> [u8; Self::ENCODED_LEN] {
        let mut b = [0u8; Self::ENCODED_LEN];
        b[0..2].copy_from_slice(&self.date.y.to_le_bytes());
        b[2] = self.date.m;
        b[3] = self.date.d;
        for (i, m) in self.mins.iter().enumerate() {
            b[4 + i * 2..6 + i * 2].copy_from_slice(&m.to_le_bytes());
        }
        b[16] = self.hijri.d;
        b[17] = self.hijri.m;
        b[18..20].copy_from_slice(&self.hijri.y.to_le_bytes());
        b
    }

    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() < Self::ENCODED_LEN {
            return None;
        }
        let u16at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
        let mut mins = [0u16; 6];
        for (i, m) in mins.iter_mut().enumerate() {
            *m = u16at(4 + i * 2);
            if *m >= 24 * 60 {
                return None;
            }
        }
        let date = Date::new(u16at(0), b[2], b[3]);
        if date.m == 0 || date.m > 12 || date.d == 0 || date.d > 31 {
            return None;
        }
        Some(Self { date, mins, hijri: Hijri { d: b[16], m: b[17], y: u16at(18) } })
    }
}

/// Encode a run of days into one blob (stored in NVS).
#[cfg(feature = "std")]
pub fn encode_days(days: &[DayTimes]) -> Vec<u8> {
    days.iter().flat_map(|d| d.encode()).collect()
}

/// Decode a cache blob into `out`, returning how many days were read.
pub fn decode_days(blob: &[u8], out: &mut [DayTimes]) -> usize {
    let mut n = 0;
    for chunk in blob.chunks_exact(DayTimes::ENCODED_LEN) {
        if n == out.len() {
            break;
        }
        if let Some(d) = DayTimes::decode(chunk) {
            out[n] = d;
            n += 1;
        }
    }
    n
}
