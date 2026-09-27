//! Dates as mail writes them, to ISO 8601.

const MONTHS: &[(&str, u32)] = &[
    // English, then the German, French, Spanish and Dutch names a mail
    // client writes in a quoted header, by their first three letters where
    // those are unambiguous.
    ("jan", 1),
    ("feb", 2),
    ("mar", 3),
    ("apr", 4),
    ("may", 5),
    ("jun", 6),
    ("jul", 7),
    ("aug", 8),
    ("sep", 9),
    ("oct", 10),
    ("nov", 11),
    ("dec", 12),
    ("mär", 3),
    ("mai", 5),
    ("okt", 10),
    ("dez", 12),
    ("janv", 1),
    ("févr", 2),
    ("fév", 2),
    ("mars", 3),
    ("avr", 4),
    ("juin", 6),
    ("juil", 7),
    ("août", 8),
    ("aoû", 8),
    ("déc", 12),
    ("ene", 1),
    ("abr", 4),
    ("ago", 8),
    ("dic", 12),
    ("mrt", 3),
    ("mei", 5),
];

fn month(word: &str) -> Option<u32> {
    let w = word.trim_end_matches('.').to_lowercase();
    if w.chars().count() < 3 || !w.chars().all(char::is_alphabetic) {
        return None;
    }
    // The longest listed name the word starts with, so `juillet` is July
    // and not June.
    MONTHS
        .iter()
        .filter(|(m, _)| w.starts_with(m))
        .max_by_key(|(m, _)| m.chars().count())
        .map(|(_, n)| *n)
}

/// An RFC 5322 `Date:` value with its offset: `Fri, 17 Jul 2026 13:48:00
/// -0500` is `2026-07-17T13:48:00-05:00`.
pub fn rfc5322(s: &str) -> Option<String> {
    // Comments like `(CDT)` carry nothing the offset does not.
    let mut clean = String::new();
    let mut depth = 0usize;
    for c in s.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => clean.push(' '),
            c if depth == 0 => clean.push(c),
            _ => {}
        }
    }
    let tokens: Vec<&str> = clean.split_whitespace().collect();
    let mut i = 0;
    // An optional day of the week.
    if tokens
        .first()
        .is_some_and(|t| t.chars().all(char::is_alphabetic))
        && month(tokens[0]).is_none()
    {
        i = 1;
    }
    let day: u32 = tokens.get(i)?.parse().ok()?;
    let mon = month(tokens.get(i + 1)?)?;
    let year = year(tokens.get(i + 2)?)?;
    let (h, min, sec) = clock(tokens.get(i + 3)?)?;
    // Anything past the zone is not this form: `8:42 PM` is a person's
    // date, which `loose` reads.
    let zone = match tokens.get(i + 4) {
        Some(z) => Some(zone(z)?),
        None => None,
    };
    if tokens.len() > i + 5 {
        return None;
    }
    iso(year, mon, day, Some((h, min, sec)), zone)
}

fn year(t: &str) -> Option<i32> {
    let y: i32 = t.parse().ok()?;
    Some(match (t.len(), y) {
        (2, y) if y < 50 => 2000 + y,
        (2, y) => 1900 + y,
        (3, y) => 1900 + y,
        (4, y) => y,
        _ => return None,
    })
}

fn clock(t: &str) -> Option<(u32, u32, u32)> {
    let mut parts = t.split(':');
    let h = parts.next()?.parse().ok()?;
    let m = parts.next()?.parse().ok()?;
    let s = match parts.next() {
        Some(s) => s.split('.').next()?.parse().ok()?,
        None => 0,
    };
    (h < 24 && m < 60 && s < 61 && parts.next().is_none()).then_some((h, m, s.min(59)))
}

/// A zone as an offset: `-0500` is `-05:00`, `GMT` is `Z`.
fn zone(z: &str) -> Option<String> {
    let offset = |sign: char, hhmm: &str| {
        (hhmm.len() == 4 && hhmm.bytes().all(|b| b.is_ascii_digit())).then(|| {
            if hhmm == "0000" {
                "Z".to_string()
            } else {
                format!("{sign}{}:{}", &hhmm[..2], &hhmm[2..])
            }
        })
    };
    if let Some(r) = z.strip_prefix('+') {
        return offset('+', r);
    }
    if let Some(r) = z.strip_prefix('-') {
        return offset('-', r);
    }
    Some(
        match z.to_ascii_uppercase().as_str() {
            "UT" | "UTC" | "GMT" | "Z" => "Z",
            "EDT" => "-04:00",
            "EST" | "CDT" => "-05:00",
            "CST" | "MDT" => "-06:00",
            "MST" | "PDT" => "-07:00",
            "PST" => "-08:00",
            _ => return None,
        }
        .to_string(),
    )
}

fn iso(
    y: i32,
    mon: u32,
    d: u32,
    time: Option<(u32, u32, u32)>,
    zone: Option<String>,
) -> Option<String> {
    if !(1..=12).contains(&mon) || !(1..=31).contains(&d) || !(1900..=2999).contains(&y) {
        return None;
    }
    let date = format!("{y:04}-{mon:02}-{d:02}");
    Some(match time {
        Some((h, m, s)) => format!("{date}T{h:02}:{m:02}:{s:02}{}", zone.unwrap_or_default()),
        None => date,
    })
}

/// A date as a mail client writes it in a quoted header, which is for
/// people and so has no fixed form: `Fri, Jul 17, 2026 at 10:05 AM`,
/// `Thursday, July 16, 2026 8:42 AM`, `17 Jul 2026, at 10:05`,
/// `17.07.2026 um 10:05`. Local time, so no offset unless one is written.
///
/// A date written with slashes is read only where it cannot be misread:
/// `7/16/2026` is July, but `7/6/2026` could be either month and is left
/// unread rather than guessed.
pub fn loose(s: &str) -> Option<String> {
    if let Some(d) = rfc5322(s).or_else(|| iso8601(s.trim())) {
        return Some(d);
    }
    let tokens: Vec<String> = s
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect();
    let (mut y, mut mon, mut d) = (None, None, None);
    let mut time: Option<(u32, u32, u32)> = None;
    let mut zone_found: Option<String> = None;
    let mut numbers: Vec<u32> = Vec::new();
    for (i, t) in tokens.iter().enumerate() {
        let t = t.trim_end_matches('.');
        if t.contains(':') {
            if let Some((h, m, sec)) = clock(t) {
                // AM/PM may follow as a word or as `a.m.`.
                let next = tokens.get(i + 1).map(|n| n.to_lowercase().replace('.', ""));
                let h = match next.as_deref() {
                    Some("pm") if h < 12 => h + 12,
                    Some("am") if h == 12 => 0,
                    _ => h,
                };
                time = Some((h, m, sec));
            }
            continue;
        }
        if t.starts_with(['+', '-']) && t.len() == 5 {
            zone_found = zone(t);
            continue;
        }
        if let Some(parts) = numeric_date(t) {
            (y, mon, d) = (Some(parts.0), Some(parts.1), Some(parts.2));
            continue;
        }
        // The last month name wins: French and Spanish abbreviate Tuesday
        // `mar`, which reads as March, and the day of the week comes first.
        if let Some(m) = month(t) {
            mon = Some(m);
            continue;
        }
        if let Ok(n) = t.parse::<u32>() {
            if t.len() == 4 {
                y.get_or_insert(n as i32);
            } else if t.len() <= 2 {
                numbers.push(n);
            }
        }
    }
    let d = d.or_else(|| numbers.first().copied())?;
    iso(y?, mon?, d, time, zone_found)
}

/// `2026-07-17`, `17.07.2026`, or an unambiguous `7/16/2026`, as (y, m, d).
fn numeric_date(t: &str) -> Option<(i32, u32, u32)> {
    let sep = ['-', '.', '/'].into_iter().find(|&c| t.contains(c))?;
    let p: Vec<&str> = t.split(sep).collect();
    if p.len() != 3
        || p.iter()
            .any(|x| x.is_empty() || !x.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    let n: Vec<u32> = p.iter().map(|x| x.parse().unwrap_or(0)).collect();
    match sep {
        '-' if p[0].len() == 4 => Some((n[0] as i32, n[1], n[2])),
        '.' if p[2].len() == 4 => Some((n[2] as i32, n[1], n[0])),
        '/' if p[2].len() == 4 => match (n[0], n[1]) {
            (a, b) if a <= 12 && b > 12 => Some((n[2] as i32, a, b)),
            (a, b) if a > 12 && b <= 12 => Some((n[2] as i32, b, a)),
            (a, b) if a == b => Some((n[2] as i32, a, b)),
            _ => None,
        },
        _ => None,
    }
}

/// An ISO 8601 timestamp, as written by software rather than a person:
/// `2026-07-17T18:48:00Z`, `2026-07-17T13:48:00-05:00`, `2026-07-17 13:48`.
fn iso8601(s: &str) -> Option<String> {
    let (date, rest) = s.split_at_checked(10)?;
    let (y, mon, d) = numeric_date(date).filter(|_| date.as_bytes()[4] == b'-')?;
    if rest.is_empty() {
        return iso(y, mon, d, None, None);
    }
    let rest = rest.strip_prefix(['T', ' '])?;
    let split = rest.find(['Z', '+', '-']).unwrap_or(rest.len());
    let (clock_text, zone_text) = rest.split_at(split);
    let time = clock(clock_text)?;
    let zone = match zone_text {
        "" => None,
        "Z" => Some("Z".to_string()),
        z => Some(zone(&z.replace(':', ""))?),
    };
    iso(y, mon, d, Some(time), zone)
}

/// ISO 8601 back to an RFC 5322 `Date:` value, for a message this crate
/// writes out: `2026-07-17T18:48:00Z` is `Fri, 17 Jul 2026 18:48:00 +0000`.
pub fn to_rfc5322(iso_text: &str) -> Option<String> {
    let norm = iso8601(iso_text)?;
    let y: i64 = norm[..4].parse().ok()?;
    let mon: u32 = norm[5..7].parse().ok()?;
    let d: u32 = norm[8..10].parse().ok()?;
    let time = norm.get(11..19).unwrap_or("00:00:00");
    let zone = match norm.get(19..) {
        Some("Z") => "+0000".to_string(),
        Some(z) if !z.is_empty() => z.replace(':', ""),
        _ => "-0000".to_string(),
    };
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const NAMES: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let weekday = DAYS[(days_from_civil(y, mon, d) + 4).rem_euclid(7) as usize];
    Some(format!(
        "{weekday}, {d} {} {y:04} {time} {zone}",
        NAMES[mon as usize - 1]
    ))
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = i64::from((m + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// A Windows FILETIME (100 ns ticks since 1601) as UTC ISO 8601.
pub fn filetime(ticks: u64) -> Option<String> {
    let secs = (ticks / 10_000_000).checked_sub(11_644_473_600)? as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil(days);
    let (h, min, s) = (rem / 3600, rem / 60 % 60, rem % 60);
    Some(format!("{y:04}-{m:02}-{d:02}T{h:02}:{min:02}:{s:02}Z"))
}

/// Days since 1970-01-01 as a civil date.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_dates_keep_their_offset() {
        assert_eq!(
            rfc5322("Fri, 17 Jul 2026 13:48:00 -0500").as_deref(),
            Some("2026-07-17T13:48:00-05:00")
        );
        assert_eq!(
            rfc5322("17 Jul 2026 13:48 +0000 (UTC)").as_deref(),
            Some("2026-07-17T13:48:00Z")
        );
        assert_eq!(
            rfc5322("Thu, 2 Jan 26 09:05:07 EST").as_deref(),
            Some("2026-01-02T09:05:07-05:00")
        );
        assert_eq!(rfc5322("tomorrow"), None);
    }

    #[test]
    fn quoted_dates_read_in_the_forms_clients_write() {
        for (text, want) in [
            ("Fri, Jul 17, 2026 at 10:05 AM", "2026-07-17T10:05:00"),
            ("Thursday, July 16, 2026 8:42 PM", "2026-07-16T20:42:00"),
            ("17 Jul 2026, at 12:05 AM", "2026-07-17T00:05:00"),
            ("17.07.2026 um 10:05", "2026-07-17T10:05:00"),
            ("ven. 17 juil. 2026 à 10:05", "2026-07-17T10:05:00"),
            ("mar. 14 juil. 2026 à 09:30", "2026-07-14T09:30:00"),
            ("2026-07-17 10:05", "2026-07-17T10:05:00"),
            ("7/16/2026 8:42 AM", "2026-07-16T08:42:00"),
            ("Monday, March 2, 2026", "2026-03-02"),
        ] {
            assert_eq!(loose(text).as_deref(), Some(want), "{text}");
        }
        assert_eq!(loose("7/6/2026 8:42 AM"), None, "month and day could swap");
    }

    #[test]
    fn iso_dates_round_trip_to_headers() {
        assert_eq!(
            loose("2026-07-17T13:48:00-05:00").as_deref(),
            Some("2026-07-17T13:48:00-05:00")
        );
        assert_eq!(
            to_rfc5322("2026-07-17T18:48:00Z").as_deref(),
            Some("Fri, 17 Jul 2026 18:48:00 +0000")
        );
        assert_eq!(
            to_rfc5322("2026-07-16T08:42:00").as_deref(),
            Some("Thu, 16 Jul 2026 08:42:00 -0000")
        );
    }

    #[test]
    fn a_filetime_is_utc() {
        // 2026-07-17T18:48:00Z
        assert_eq!(
            filetime(134_287_876_800_000_000).as_deref(),
            Some("2026-07-17T18:48:00Z")
        );
    }
}
