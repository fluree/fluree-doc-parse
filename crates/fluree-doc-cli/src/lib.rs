//! `fdoc` — command-line surface for the fluree-doc-parse extraction core.
//!
//! Structured as a library plus a thin binary so the command set can be
//! mounted as a subcommand of a larger Fluree CLI: embed [`cli::Commands`]
//! (or the whole [`cli::Cli`]) and dispatch through [`commands::run`].

pub mod cli;
pub mod commands;
pub mod config;
pub(crate) mod escalate;

/// The reader `convert` sends a file named `name` holding `data` to
/// (`"Pdf"`, `"Xlsb"`, `"Unsupported"`, …), and why it refuses it where it
/// does: the content detection, for the fuzz targets under `fuzz/`. Not part
/// of the command surface.
#[doc(hidden)]
pub fn detect(name: &str, data: &[u8]) -> (String, Option<String>) {
    let path = std::path::Path::new(name);
    let kind = commands::common::input_kind(path, data);
    (
        format!("{kind:?}"),
        commands::common::unsupported(Some(path), data),
    )
}

/// What `convert --parse-attachments` makes of each file `email` carries,
/// down through attached messages, handed to `each` as it is made: each
/// converted to the output the caller reads for
/// (`fluree_doc_model::budget::converting_to`; DoCO outside it), within
/// what the message leaves of the budget, as `convert` converts it, with no
/// OCR and no model, its first four pages at most. A file no reader takes
/// is not handed. For the fuzz targets under `fuzz/`: the e-mail target
/// reads a message's files through it, so a file a message carries meets
/// the reader `convert` hands it to, on the budget `convert` gives it. Not
/// part of the command surface.
#[doc(hidden)]
pub fn convert_attachments(
    email: &fluree_doc_email::Email,
    mut each: impl FnMut(Result<String, String>),
) {
    use clap::Parser;
    use fluree_doc_model::budget::{output, Output};
    let format = match output() {
        Output::Doco => "doco",
        Output::Text => "text",
        Output::Flat => "md",
    };
    let Ok(cli) = cli::Cli::try_parse_from(["fdoc", "convert", "attachment", "-f", format]) else {
        return;
    };
    let cli::Commands::Convert(args) = cli.command else {
        return;
    };
    let cfg = commands::common::TierConfig::default();
    let convert =
        commands::convert::carried_converter(email, &cfg, &args, Some(&[1, 2, 3, 4]), true);
    let mut read = |f: &fluree_doc_email::AttachedFile, _depth: usize| {
        let out = convert(f)?;
        let kept = out.as_ref().map(|_| ()).map_err(Clone::clone);
        each(out);
        Some(kept)
    };
    fluree_doc_email::parse_attachments(email, &Default::default(), &mut read);
}

#[cfg(test)]
mod tests {
    use fluree_doc_model::budget::{converting_to, within, Output};

    #[test]
    fn an_attachment_is_converted_by_the_reader_convert_hands_it_to() {
        // A long body, so that what the message holds is more than the
        // CSV needs.
        let body = "See the orders attached. ".repeat(1000);
        let mail = format!(
            "From: Alex Sample <alex@example.com>\r\nSubject: Orders\r\n\
                    MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"b\"\r\n\r\n\
                    --b\r\nContent-Type: text/plain\r\n\r\n{body}\r\n\
                    --b\r\nContent-Type: text/csv\r\n\
                    Content-Disposition: attachment; filename=\"data.csv\"\r\n\r\n\
                    Region,Amount\r\nNorth,12\r\n\
                    --b\r\nContent-Type: application/octet-stream\r\n\
                    Content-Disposition: attachment; filename=\"deck.key\"\r\n\r\n\
                    PK\x03\x04\r\n--b--\r\n"
        );
        let email = fluree_doc_email::parse(mail.as_bytes()).unwrap();
        let converted = |output| {
            let mut out = Vec::new();
            converting_to(output, || {
                super::convert_attachments(&email, |r| out.push(r.unwrap()))
            });
            out
        };
        // A format with no reader is not handed; the CSV is, converted to
        // the output read for.
        let doco = converted(Output::Doco);
        assert_eq!(doco.len(), 1);
        assert!(
            doco[0].contains("\"doc:cellValue\": \"North\""),
            "{}",
            doco[0]
        );
        let md = converted(Output::Flat);
        assert!(md[0].contains("|North|12|"), "{}", md[0]);
        // Within what the message leaves: none, and the file is refused.
        let mut refused = Vec::new();
        within(held(&email), || {
            super::convert_attachments(&email, |r| refused.push(r))
        });
        assert_eq!(refused.len(), 1);
        let e = refused[0].as_ref().unwrap_err();
        assert!(e.contains("memory a document may take"), "{e}");
    }

    /// What `email` holds while its files are converted, at the budget's
    /// costs.
    fn held(email: &fluree_doc_email::Email) -> u64 {
        let mut b = fluree_doc_model::budget::Budget::default();
        b.spend_elements(email.elements.len() as u64).unwrap();
        for e in &email.elements {
            b.spend_text(&e.text).unwrap();
        }
        let files: usize = email.attachments.iter().map(|a| a.bytes.len()).sum();
        b.spend_bytes(files as u64).unwrap();
        fluree_doc_model::budget::MAX_MEMORY - b.memory_left()
    }
}
