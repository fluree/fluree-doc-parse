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

/// What `convert --parse-attachments -f doco` makes of a file an e-mail
/// carries, with no OCR and no model, its first four pages at most; `None`
/// where it is not read. For the fuzz targets under `fuzz/`: the e-mail
/// target reads a message's attachments through it, so a file that a
/// message carries meets the reader `convert` hands it to. Not part of the
/// command surface.
#[doc(hidden)]
pub fn convert_attachment(f: &fluree_doc_email::AttachedFile) -> Option<Result<String, String>> {
    use clap::Parser;
    let cli = cli::Cli::try_parse_from(["fdoc", "convert", "attachment", "-f", "doco"]).ok()?;
    let cli::Commands::Convert(args) = cli.command else {
        return None;
    };
    let cfg = commands::common::TierConfig::default();
    commands::convert::convert_carried(f, &cfg, &args, Some(&[1, 2, 3, 4]), true)
}

#[cfg(test)]
mod tests {
    use fluree_doc_email::AttachedFile;

    #[test]
    fn an_attachment_is_converted_by_the_reader_convert_hands_it_to() {
        let csv = AttachedFile::new(
            Some("data.csv".into()),
            "text/csv".into(),
            false,
            b"Region,Amount\nNorth,12\n".to_vec(),
        );
        let out = super::convert_attachment(&csv).unwrap().unwrap();
        assert!(out.contains("\"doc:cellValue\": \"North\""), "{out}");
        // An image is not read without OCR, nor a format with no reader.
        let png = AttachedFile::new(
            Some("logo.png".into()),
            "image/png".into(),
            false,
            b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec(),
        );
        assert!(super::convert_attachment(&png).is_none());
        let key = AttachedFile::new(
            Some("deck.key".into()),
            "application/octet-stream".into(),
            false,
            b"PK\x03\x04".to_vec(),
        );
        assert!(super::convert_attachment(&key).is_none());
    }
}
