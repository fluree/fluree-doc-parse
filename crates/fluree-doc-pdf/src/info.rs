//! What a PDF declares about itself: its document information dictionary.

use fluree_doc_model::DocumentInfo;
use hayro_syntax::object::DateTime;
use hayro_syntax::Pdf;

/// The Info dictionary's `/Title`, `/Author`, `/CreationDate` and
/// `/ModDate`, as the file states them.
pub fn read(pdf: &Pdf) -> DocumentInfo {
    let m = pdf.metadata();
    let text = |raw: &Option<Vec<u8>>| {
        raw.as_deref()
            .map(crate::outline::decode_text_string)
            .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|t| !t.is_empty())
    };
    DocumentInfo {
        title: text(&m.title),
        creators: text(&m.author).into_iter().collect(),
        created: m.creation_date.as_ref().and_then(iso),
        modified: m.modification_date.as_ref().and_then(iso),
    }
}

/// A PDF date as an ISO 8601 date-time, or `None` for a day that does not
/// exist.
///
/// The offset is written only when it is not zero. The parser reads `Z`,
/// `+00'00'` and no offset at all the same way, and of those, a date-time
/// with no offset — local time, zone unknown — is the one that cannot be
/// wrong.
fn iso(d: &DateTime) -> Option<String> {
    let mut s = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        d.year, d.month, d.day, d.hour, d.minute, d.second
    );
    if d.utc_offset_hour != 0 || d.utc_offset_minute != 0 {
        let sign = if d.utc_offset_hour < 0 { '-' } else { '+' };
        s.push_str(&format!(
            "{sign}{:02}:{:02}",
            d.utc_offset_hour.unsigned_abs(),
            d.utc_offset_minute
        ));
    }
    fluree_doc_model::xsd_date_time(&s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(day: u8, hour: i8, minute: u8) -> DateTime {
        DateTime {
            year: 2024,
            month: 2,
            day,
            hour: 10,
            minute: 30,
            second: 5,
            utc_offset_hour: hour,
            utc_offset_minute: minute,
        }
    }

    #[test]
    fn a_pdf_date_is_iso_with_the_offset_it_states() {
        assert_eq!(iso(&dt(29, 0, 0)).as_deref(), Some("2024-02-29T10:30:05"));
        assert_eq!(
            iso(&dt(29, -5, 0)).as_deref(),
            Some("2024-02-29T10:30:05-05:00")
        );
        assert_eq!(
            iso(&dt(29, 5, 30)).as_deref(),
            Some("2024-02-29T10:30:05+05:30")
        );
        assert_eq!(iso(&dt(30, 0, 0)), None, "no such day");
    }
}
