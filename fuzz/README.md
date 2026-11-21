# Fuzzing the readers

Every reader that takes a file's bytes has a [cargo-fuzz] target here, fed
arbitrary bytes, so a file nobody thought of is tried before a user sends
it: all but the transcript reader's. A reader may refuse any input with an error; a target fails when one
panics, aborts, hangs, runs out of memory, or returns more than one
document's budget allows (`docs/reference/limits.md`).

This directory is a workspace of its own: the main build never compiles it,
and it needs a nightly toolchain and libFuzzer, which the main build does
not.

## Targets

| Target | Reads | Through |
| --- | --- | --- |
| `csv` | CSV, or TSV (the first byte picks) | `fluree_doc_csv::parse_with` |
| `xls` | Excel 97-2003 | `fluree_doc_xls::parse_with_info` |
| `xlsb` | Excel binary workbook | `fluree_doc_xls::parse_xlsb` |
| `doc` | Word 97-2003 | `fluree_doc_doc::parse` |
| `ppt` | PowerPoint 97-2003, and its pictures | `fluree_doc_ppt::parse_with_media`, `read_media` |
| `odf` | `.odt`, `.ods`, `.odp` | `fluree_doc_odf::parse_with_info` |
| `rtf` | RTF | `fluree_doc_rtf::parse_with_info` |
| `xps` | XPS and OpenXPS, their pictures, the page analysis | `fluree_doc_xps::parse`, `read_image`, `fluree_doc_pdf::document::analyze` |
| `docx` | Word, with notes and comments, and its pictures | `fluree_doc_docx::parse_with_media`, `read_media` |
| `xlsx` | Excel, and its pictures | `fluree_doc_xlsx::parse_with_media`, `read_media` |
| `pptx` | PowerPoint | `fluree_doc_pptx::parse_with_info` |
| `pdf` | PDF: outline, metadata, notes, embedded files, links; the extraction and analysis of a document of at most four pages | `convert`'s pipeline without OCR or a model |
| `html` | a web page, and an e-mail's HTML body | `fluree_doc_html::decode`, `parse`, `info`, `parse_with_quote_depth` |
| `markdown` | Markdown, as UTF-8 | `fluree_doc_markdown::parse` |
| `image` | PNG, JPEG, TIFF: the header, and the image decoded and read by a stand-in OCR engine (no models) | `fluree_doc_image::header`, `elements` |
| `email` | `.eml` and `.msg`, the files they carry, down through attached messages, each converted by the reader `convert` hands it to (no OCR, no model, four pages at most) | `fluree_doc_email::parse`, `parse_attachments`, `fluree_doc_cli::convert_attachment` |
| `detect` | which reader `convert` sends a file to, by content and name (the first byte picks the name) | `fluree_doc_cli::detect` |

Each target renders what the reader made through every output (Markdown,
XHTML, text, DoCO JSON-LD), as `convert` would, and asserts that it stands
for no more memory than one document may take, at the budget's own costs
(`src/lib.rs`), the copies DoCO writes of a table's cells included (a merged
value in each cell it covers, each cell's labels), so a reader that makes a
table without spending them fails. `cargo fuzz` builds with `--cfg fuzzing`, under which that
budget is a sixteenth of the shipped one, so an input that fills it stays
quick to run, and so are a reader's own bounds on the memory the budget
does not count (`fluree_doc_model::budget::scaled`: the cells calamine
allocates for `xls` and `xlsb`, an XPS document's parts and glyphs, the
pixels an image is decoded to; `docs/reference/limits.md` lists them), so an
input within them stays within the memory limit below.

## Running

```sh
rustup toolchain install nightly --profile minimal
cargo install cargo-fuzz

cd fuzz
cargo +nightly fuzz build -O -a -s none
cargo +nightly fuzz run xls corpus/xls seeds/xls -O -a -s none -- \
    -rss_limit_mb=320 -timeout=10 -max_len=262144 -max_total_time=600
```

* `-s none` builds without AddressSanitizer: the readers are safe Rust, a
  run is several times faster without it, and it can then run under
  `ulimit -v`. Build with the default (`-s address`) to look into a crash
  in a dependency's `unsafe` code.
* `-a` keeps debug assertions, so an arithmetic overflow panics rather than
  wrapping silently.
* `-rss_limit_mb=320`: built for fuzzing (`--cfg fuzzing`), the budget
  is a sixteenth of the shipped one (`docs/reference/limits.md`), 160 MiB.
  A target's process takes 30 to 90 MB before its input, so 320 MB leaves
  the input about 1.6 times its budget, the margin the shipped budget has
  in a 4 GB address space (2.5 GiB): a reader whose memory runs past its
  estimate by enough to abort there fails here too. A larger limit hides
  it: at 2048 MB, a reader could take twelve times what it accounts for.
* `corpus/<target>` is where libFuzzer keeps the inputs it finds (not
  committed); `seeds/<target>` is read only.
* Add `-fork=N -ignore_crashes=1 -ignore_ooms=1 -ignore_timeouts=1` to run
  N processes and carry on past a failure; `-artifact_prefix=DIR/` says
  where each failing input is written (`cargo fuzz run` uses
  `artifacts/<target>/`).

## What the fuzzer hardly reaches

libFuzzer mutates bytes, and almost never writes a valid deflate stream:
the zipped formats (`.docx`, `.xlsx`, `.pptx`, `.odt`, `.ods`, `.odp`,
`.xps`, `.xlsb`) are fuzzed through what their seeds' deflated parts hold
and through parts stored uncompressed, which `-max_len` bounds. An input of
256 KiB stored holds about 15,000 paragraphs, under the fuzzing budget, so
the amplification a few kilobytes of deflate carry (a part that inflates a
thousandfold) is out of its reach. That is for the unit tests of each
reader, which build such parts in code, and for seeds that amplify within
the bound (a long label over many cells, wide spans, deep nesting), as
`seeds/html` and `seeds/markdown` hold.

## Seeds

`seeds/<target>/` holds small valid files of each format, written by
`make_seeds.py` (LibreOffice converts flat OpenDocument sources to the
office formats, Pillow writes the images; the rest it writes itself), and small hostile files from
earlier reviews. To write them again, from the repository root:

```sh
SOFFICE=/path/to/soffice uv run fuzz/make_seeds.py --extra DIR --local fuzz/corpus
```

Generated files over 64 KiB, and hostile files over 8 KiB (`--extra`), go to
the local corpus (`--local`), not to the repository.

## A crash becomes a regression test

1. Minimise it: `cargo +nightly fuzz tmin <target> artifacts/<target>/crash-… -O -a -s none`.
2. Reproduce it: `cargo +nightly fuzz run <target> <minimised file> -O -a -s none`.
3. Find the cause, and fix it in the reader.
4. Add a unit test to the reader's crate that builds the smallest input
   showing the defect in code (a few lines of XML zipped, a record
   assembled byte by byte, as the crate's tests already do), rather than
   committing the fuzzer's file: the test then says what the file is,
   and survives a change of format details the bytes would not.
5. Run the target again over its corpus to see it pass.
