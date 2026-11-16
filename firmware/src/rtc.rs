//! PCF85063A real-time clock (I²C 0x51), kept in UTC.
use esp_idf_svc::hal::delay::BLOCK;
use esp_idf_svc::hal::i2c::I2cDriver;
use ezan::Date;

const ADDR: u8 = 0x51;

fn bcd(v: u8) -> u8 {
    (v >> 4) * 10 + (v & 0x0F)
}
fn to_bcd(v: u32) -> u8 {
    (((v / 10) << 4) | (v % 10)) as u8
}

/// UTC unix time, or None if the oscillator stopped (time lost) or the chip is missing.
pub fn read(i2c: &mut I2cDriver) -> Option<i64> {
    let mut r = [0u8; 7];
    i2c.write_read(ADDR, &[0x04], &mut r, BLOCK).ok()?;
    if r[0] & 0x80 != 0 {
        return None; // OS flag: clock integrity not guaranteed
    }
    let sec = bcd(r[0] & 0x7F) as i64;
    let min = bcd(r[1] & 0x7F) as i64;
    let hour = bcd(r[2] & 0x3F) as i64;
    let day = bcd(r[3] & 0x3F);
    let month = bcd(r[5] & 0x1F);
    let year = 2000 + bcd(r[6]) as u16;
    if !(1..=12).contains(&month) || day == 0 {
        return None;
    }
    let days = Date::new(year, month, day).to_days() as i64;
    Some(days * 86_400 + hour * 3600 + min * 60 + sec)
}

pub fn write(i2c: &mut I2cDriver, unix: i64) -> anyhow::Result<()> {
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400) as u32;
    let d = Date::from_days(days as i32);
    let buf = [
        0x04,
        to_bcd(secs % 60), // clears OS flag
        to_bcd(secs / 60 % 60),
        to_bcd(secs / 3600),
        to_bcd(d.d as u32),
        d.weekday(),
        to_bcd(d.m as u32),
        to_bcd((d.y - 2000) as u32),
    ];
    i2c.write(ADDR, &buf, BLOCK)?;
    Ok(())
}
