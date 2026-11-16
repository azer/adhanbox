//! Parser for ezanvakti.emushaf.net `/vakitler/{id}` (Diyanet monthly times).
use crate::date::Date;
use crate::prayer::{DayTimes, Hijri};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Raw {
    miladi_tarih_kisa: String,
    hicri_tarih_kisa: String,
    imsak: String,
    gunes: String,
    ogle: String,
    ikindi: String,
    aksam: String,
    yatsi: String,
}

fn hm(s: &str) -> Option<u16> {
    let (h, m) = s.trim().split_once(':')?;
    let (h, m): (u16, u16) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then_some(h * 60 + m)
}

/// "d.m.y" → (d, m, y)
fn dmy(s: &str) -> Option<(u8, u8, u16)> {
    let mut it = s.trim().split('.');
    let d = it.next()?.parse().ok()?;
    let m = it.next()?.parse().ok()?;
    let y = it.next()?.parse().ok()?;
    Some((d, m, y))
}

#[derive(Debug)]
pub struct ParseError(pub String);

impl core::fmt::Display for ParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ParseError {}

/// Parse the JSON array, returning days sorted by date. Bad rows are skipped.
pub fn parse_vakitler(json: &[u8]) -> Result<Vec<DayTimes>, ParseError> {
    let raw: Vec<Raw> = serde_json::from_slice(json).map_err(|e| ParseError(e.to_string()))?;
    let mut out: Vec<DayTimes> = raw
        .iter()
        .filter_map(|r| {
            let (d, m, y) = dmy(&r.miladi_tarih_kisa)?;
            let (hd, hmo, hy) = dmy(&r.hicri_tarih_kisa).unwrap_or((0, 0, 0));
            Some(DayTimes {
                date: Date::new(y, m, d),
                mins: [
                    hm(&r.imsak)?,
                    hm(&r.gunes)?,
                    hm(&r.ogle)?,
                    hm(&r.ikindi)?,
                    hm(&r.aksam)?,
                    hm(&r.yatsi)?,
                ],
                hijri: Hijri { d: hd, m: hmo, y: hy },
            })
        })
        .collect();
    if out.is_empty() {
        return Err(ParseError("no usable days in response".into()));
    }
    out.sort_by_key(|d| d.date);
    out.dedup_by_key(|d| d.date);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_sample() {
        let j = br#"[{"HicriTarihKisa":"4.6.1448","HicriTarihKisaIso8601":null,"HicriTarihUzun":"4 Cemaziyelahir 1448","HicriTarihUzunIso8601":null,"AyinSekliURL":"x","MiladiTarihKisa":"13.11.2026","MiladiTarihKisaIso8601":"13.11.2026","MiladiTarihUzun":"13 Kasim 2026 Cuma","MiladiTarihUzunIso8601":"2026-11-13T00:00:00.0000000+03:00","GreenwichOrtalamaZamani":3.0,"Aksam":"17:00","Gunes":"07:44","GunesBatis":"16:52","GunesDogus":"07:44","Ikindi":"14:40","Imsak":"06:14","KibleSaati":"11:31","Ogle":"12:53","Yatsi":"19:17"}]"#;
        let d = parse_vakitler(j).unwrap();
        assert_eq!(d[0].date, Date::new(2026, 11, 13));
        assert_eq!(d[0].mins[0], 6 * 60 + 14);
        assert_eq!(d[0].hijri, Hijri { d: 4, m: 6, y: 1448 });
    }
}
