//! GNSS position parsing for the supported modem families.

use crate::modem::parser::split_fields;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct GnssFix {
    pub fix: bool,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub altitude_m: Option<f64>,
    pub speed_kmh: Option<f64>,
    pub heading_deg: Option<f64>,
    pub satellites: Option<i64>,
    /// UTC time reported by the receiver (RFC 3339) when available.
    pub utc_time: Option<String>,
}

/// Commands to power the receiver on and to query a position.
pub fn commands(kind: &str) -> Option<(&'static str, &'static str)> {
    match kind {
        "simcom_cgns" => Some(("AT+CGNSPWR=1", "AT+CGNSINF")),
        "simcom_cgps" => Some(("AT+CGPS=1", "AT+CGPSINFO")),
        "quectel_qgps" => Some(("AT+QGPS=1", "AT+QGPSLOC=2")),
        _ => None,
    }
}

fn num(f: &[String], i: usize) -> Option<f64> {
    f.get(i).and_then(|s| s.parse::<f64>().ok())
}

/// `ddmm.mmmm` with hemisphere to decimal degrees.
fn nmea_deg(value: &str, hemi: &str) -> Option<f64> {
    let v: f64 = value.parse().ok()?;
    let deg = (v / 100.0).floor();
    let dec = deg + (v - deg * 100.0) / 60.0;
    Some(if matches!(hemi, "S" | "W") { -dec } else { dec })
}

/// `yyyyMMddhhmmss.sss` to RFC 3339.
fn compact_utc(s: &str) -> Option<String> {
    if s.len() < 14 || !s[..14].chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(format!(
        "{}-{}-{}T{}:{}:{}Z",
        &s[0..4],
        &s[4..6],
        &s[6..8],
        &s[8..10],
        &s[10..12],
        &s[12..14]
    ))
}

fn valid(lat: f64, lon: f64) -> bool {
    (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon) && !(lat == 0.0 && lon == 0.0)
}

/// Parse the response of the profile's position query.
pub fn parse(kind: &str, lines: &[String]) -> Option<GnssFix> {
    let find = |p: &str| {
        lines
            .iter()
            .find_map(|l| l.strip_prefix(p))
            .map(|r| split_fields(r.trim()))
    };
    match kind {
        "simcom_cgns" => {
            // +CGNSINF: run,fix,utc,lat,lon,alt,speed(km/h),course,mode,,hdop,pdop,vdop,,in_view,used,...
            let f = find("+CGNSINF:")?;
            let (lat, lon) = (num(&f, 3), num(&f, 4));
            let fix = f.get(1).map(|s| s == "1").unwrap_or(false)
                && matches!((lat, lon), (Some(a), Some(b)) if valid(a, b));
            Some(GnssFix {
                fix,
                latitude: lat.filter(|_| fix),
                longitude: lon.filter(|_| fix),
                altitude_m: num(&f, 5).filter(|_| fix),
                speed_kmh: num(&f, 6).filter(|_| fix),
                heading_deg: num(&f, 7).filter(|_| fix),
                satellites: num(&f, 15).or(num(&f, 14)).map(|v| v as i64),
                utc_time: f.get(2).and_then(|s| compact_utc(s)),
            })
        }
        "simcom_cgps" => {
            // +CGPSINFO: lat,N/S,lon,E/W,ddmmyy,hhmmss.s,alt,speed(knots),course
            let f = find("+CGPSINFO:")?;
            let lat = f.first().zip(f.get(1)).and_then(|(v, h)| nmea_deg(v, h));
            let lon = f.get(2).zip(f.get(3)).and_then(|(v, h)| nmea_deg(v, h));
            let fix = matches!((lat, lon), (Some(a), Some(b)) if valid(a, b));
            let utc = match (f.get(4), f.get(5)) {
                (Some(d), Some(t)) if d.len() == 6 && t.len() >= 6 => compact_utc(&format!(
                    "20{}{}{}{}",
                    &d[4..6],
                    &d[2..4],
                    &d[0..2],
                    &t[..6]
                )),
                _ => None,
            };
            Some(GnssFix {
                fix,
                latitude: lat.filter(|_| fix),
                longitude: lon.filter(|_| fix),
                altitude_m: num(&f, 6).filter(|_| fix),
                speed_kmh: num(&f, 7).filter(|_| fix).map(|k| k * 1.852),
                heading_deg: num(&f, 8).filter(|_| fix),
                satellites: None,
                utc_time: utc,
            })
        }
        "quectel_qgps" => {
            // +QGPSLOC: hhmmss.sss,lat,lon,hdop,alt,fix,cog,spkm,spkn,ddmmyy,nsat
            let f = find("+QGPSLOC:")?;
            let (lat, lon) = (num(&f, 1), num(&f, 2));
            let fix = matches!((lat, lon), (Some(a), Some(b)) if valid(a, b));
            let utc = match (f.first(), f.get(9)) {
                (Some(t), Some(d)) if d.len() == 6 && t.len() >= 6 => compact_utc(&format!(
                    "20{}{}{}{}",
                    &d[4..6],
                    &d[2..4],
                    &d[0..2],
                    &t[..6]
                )),
                _ => None,
            };
            Some(GnssFix {
                fix,
                latitude: lat.filter(|_| fix),
                longitude: lon.filter(|_| fix),
                altitude_m: num(&f, 4).filter(|_| fix),
                speed_kmh: num(&f, 7).filter(|_| fix),
                heading_deg: num(&f, 6).filter(|_| fix),
                satellites: num(&f, 10).map(|v| v as i64),
                utc_time: utc,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(s: &str) -> Vec<String> {
        vec![s.to_string()]
    }

    #[test]
    fn simcom_cgns() {
        let f = parse("simcom_cgns", &l("+CGNSINF: 1,1,20250102030405.000,55.676100,12.568300,12.500,3.20,180.5,1,,1.1,1.4,0.9,,11,7,,,40,,")).unwrap();
        assert!(f.fix);
        assert_eq!((f.latitude, f.longitude), (Some(55.6761), Some(12.5683)));
        assert_eq!(
            (f.altitude_m, f.speed_kmh, f.heading_deg, f.satellites),
            (Some(12.5), Some(3.2), Some(180.5), Some(7))
        );
        assert_eq!(f.utc_time.as_deref(), Some("2025-01-02T03:04:05Z"));
        let none = parse("simcom_cgns", &l("+CGNSINF: 1,0,,,,,,,0,,,,,,0,0,,,,,")).unwrap();
        assert!(!none.fix && none.latitude.is_none());
        assert!(parse("simcom_cgns", &l("garbage")).is_none());
    }

    #[test]
    fn simcom_cgps_and_quectel() {
        let f = parse(
            "simcom_cgps",
            &l("+CGPSINFO: 5540.5660,N,01234.0980,E,020125,030405.0,12.5,10.0,90.0"),
        )
        .unwrap();
        assert!(f.fix);
        assert!((f.latitude.unwrap() - 55.676_1).abs() < 1e-4);
        assert!((f.longitude.unwrap() - 12.568_3).abs() < 1e-4);
        assert!((f.speed_kmh.unwrap() - 18.52).abs() < 1e-6);
        assert_eq!(f.utc_time.as_deref(), Some("2025-01-02T03:04:05Z"));
        assert!(!parse("simcom_cgps", &l("+CGPSINFO: ,,,,,,,,")).unwrap().fix);
        let q = parse(
            "quectel_qgps",
            &l("+QGPSLOC: 030405.000,55.67610,12.56830,1.0,12.5,3,90.00,5.0,2.7,020125,08"),
        )
        .unwrap();
        assert!(q.fix);
        assert_eq!((q.satellites, q.speed_kmh), (Some(8), Some(5.0)));
        assert_eq!(q.utc_time.as_deref(), Some("2025-01-02T03:04:05Z"));
    }
}
