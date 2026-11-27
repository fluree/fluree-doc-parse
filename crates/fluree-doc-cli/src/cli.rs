use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

/// Adaptive document parsing: text and structure (headings, tables, lists,
/// forms) from PDF, Markdown, HTML, DOCX, PPTX and XLSX, speaker turns from
/// WebVTT and SubRip transcripts, and threads from email.
///
/// PDF structure is inferred from layout, with per-page signals telling you
/// which pages would benefit from model-tier escalation; the other formats
/// declare their structure and are read directly. Every source produces the
/// same element model, so all five output formats work for all of them.
#[derive(Parser, Debug)]
#[command(name = "fdoc", version, propagate_version = true)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// Enable verbose output (per-document timing on stderr)
    #[arg(long, short = 'v', global = true, conflicts_with = "quiet")]
    pub verbose: bool,

    /// Suppress non-essential output
    #[arg(long, short = 'q', global = true, conflicts_with = "verbose")]
    pub quiet: bool,

    /// Disable colored output (also respects NO_COLOR env var)
    #[arg(long, global = true)]
    pub no_color: bool,
}

/// An OCR engine this build can run.
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum OcrEngineName {
    /// PP-OCRv6 small through oar-ocr, on ONNX Runtime
    #[value(name = "oar-ocr")]
    OarOcr,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Convert documents to Markdown, XHTML, DoCO JSON-LD, JSON or text
    ///
    /// Reads PDF, Markdown, plain text, HTML, DOCX, PPTX, XLSX, WebVTT and
    /// SubRip transcripts, and email (.eml, .msg). PDF structure is inferred
    /// from layout; the others declare theirs and carry no geometry.
    ///
    /// Examples:
    ///   fdoc convert report.pdf
    ///   fdoc convert report.pdf --format json -o report.json
    ///   fdoc convert ./docs/ --out-dir ./out -j 8
    ///   cat report.pdf | fdoc convert -
    Convert(ConvertArgs),

    /// Read crops or images with the built-in OCR engine into word files
    ///
    /// Writes one `--ocr-words` sidecar per image, `<image stem>.json`, for
    /// `fdoc convert --ocr-words`. Given the directory `fdoc dev
    /// render-routed` wrote, reads only the crops whose words a page takes
    /// (by the manifest's `reason`); given image files, reads them all.
    ///
    /// Examples:
    ///   fdoc dev render-routed report.pdf crops/
    ///   fdoc ocr crops/ --models ~/assets/ocr --out words/
    ///   fdoc convert report.pdf --ocr-words words/
    Ocr(OcrArgs),

    /// Extract AcroForm fields: name, type, value, bbox (JSON)
    ///
    /// Filled-in form values live in widget annotations, not the content
    /// stream, so a completed form converts as its blank template; this
    /// command reads the values, with placement in render coordinates.
    Forms {
        /// PDF file to read
        pdf: PathBuf,
    },

    /// Per-page routing verdicts: which pages need model escalation
    ///
    /// Every page is measured (glyph counts, Unicode resolution, image
    /// coverage) and reported as deterministic, or as needing escalation:
    /// Scanned / NearBlank (the text is pixels), BrokenText (glyphs whose
    /// Unicode cannot be trusted), or raster regions the text layer cannot
    /// read. Over a directory this prints the escalation rate — the number
    /// that prices a deployment.
    #[command(visible_alias = "route")]
    Triage {
        /// PDF file or directory of PDFs
        path: PathBuf,
    },

    /// Show or set configuration: the deep reader's provider and credentials
    ///
    /// With nothing configured, `fdoc convert` never leaves the deterministic
    /// tier and never reaches the network. Naming a provider is what turns
    /// escalation on.
    ///
    /// Examples:
    ///   fdoc config gemini --credentials ~/sa-key.json
    ///   fdoc config show
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },

    /// Render pages to PNG, in the coordinate space the output's bboxes use
    ///
    /// The images an annotation overlay is drawn on. Rendering elsewhere and
    /// positioning with our coordinates means reconciling two PDF
    /// implementations; these come from the same parse, so they agree by
    /// construction.
    ///
    /// Coordinates in `--format json` and `doco` are PDF user units with a
    /// top-left origin: multiply by the scale to get pixels.
    Render {
        /// PDF file or directory of PDFs
        path: PathBuf,
        /// Directory to write `<stem>_p<N>.png` into
        #[arg(default_value = "page-renders")]
        out: PathBuf,
        /// Oversampling factor; 2 is ~144 dpi
        #[arg(long, default_value_t = 2.0, value_name = "N")]
        scale: f32,
        /// Restrict to these 1-based pages, e.g. `3`, `1-5`, or `1,4,9-12`
        #[arg(long, value_name = "RANGES")]
        pages: Option<String>,
    },

    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },

    /// Pipeline-internals commands for debugging extraction (unstable)
    ///
    /// These expose intermediate layout state — raw glyphs, assembled lines,
    /// blocks, detected furniture, table geometry. Their output formats are
    /// not a compatibility surface and may change at any time.
    Dev {
        #[command(subcommand)]
        command: DevCommands,
    },

    // Hidden single-file compatibility forms of `convert` (equivalent to
    // `convert <pdf> --format <fmt> --no-escalate`); scripts shell these.
    #[command(hide = true)]
    Md { pdf: PathBuf },
    #[command(hide = true)]
    Json { pdf: PathBuf },
    #[command(hide = true)]
    Xhtml { pdf: PathBuf },
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// GitHub-flavored Markdown
    Md,
    /// XHTML fragments (h1-h6/p/ul/table)
    Xhtml,
    /// DoCO-typed elements with bounding boxes (flat JSON, not JSON-LD)
    Json,
    /// DoCO JSON-LD graph: sections, containment, table cells, char offsets,
    /// page/bbox provenance — insertable into a Fluree ledger directly
    Doco,
    /// Plain-text projection; `doco` char offsets index into exactly this
    Text,
    /// Projection conformance (dev): each format written, read back and
    /// held to the element model; the violations as JSON
    #[value(hide = true)]
    Conform,
}

/// How `--format md` writes a link.
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum MdLinks {
    /// `[words](address)`, or `<address>` where the words are the address
    Inline,
    /// The words alone; the addresses stay in json, doco and xhtml
    Text,
}

impl From<MdLinks> for fluree_doc_model::MdLinks {
    fn from(m: MdLinks) -> Self {
        match m {
            MdLinks::Inline => Self::Inline,
            MdLinks::Text => Self::Text,
        }
    }
}

#[derive(Args, Debug)]
pub struct ConvertArgs {
    /// Input documents: files, directories, or `-` for stdin (a PDF, a
    /// transcript or an email)
    #[arg(required = true, value_name = "FILE|DIR|-")]
    pub inputs: Vec<PathBuf>,

    /// Output format
    #[arg(long, short = 'f', value_enum, default_value_t = Format::Md)]
    pub format: Format,

    /// How `--format md` writes links
    #[arg(long, value_enum, default_value_t = MdLinks::Inline, value_name = "MODE")]
    pub md_links: MdLinks,

    /// Write output to this file (single input only; default stdout)
    #[arg(long, short = 'o', value_name = "FILE", conflicts_with = "out_dir")]
    pub output: Option<PathBuf>,

    /// Write one output file per input into this directory
    #[arg(long, value_name = "DIR")]
    pub out_dir: Option<PathBuf>,

    /// Restrict output to these 1-based pages, e.g. `3`, `1-5`, or `1,4,9-12`
    #[arg(long, value_name = "RANGES")]
    pub pages: Option<String>,

    /// Parallel workers for batch conversion (0 = one per core)
    #[arg(long, short = 'j', default_value_t = 0, value_name = "N")]
    pub jobs: usize,

    /// Directory of layout-detector sidecars (`<stem>_p<N>_page.json`) used
    /// to promote missed section titles [env: FDOC_TITLE_BOXES]
    #[arg(long, value_name = "DIR")]
    pub layout_boxes: Option<PathBuf>,

    /// Directory of model-tier readings (`<stem>_<crop>.json`) to splice into
    /// the output [env: FDOC_TIER_RESULTS]
    #[arg(long, value_name = "DIR")]
    pub tier_results: Option<PathBuf>,

    /// Directory of table-structure readings for three-way arbitration
    /// [env: FDOC_STRUCTURE_RESULTS]
    #[arg(long, value_name = "DIR")]
    pub structure_results: Option<PathBuf>,

    /// Directory of OCR word readings (`<stem>_p<N>_full.json`,
    /// `<stem>_p<N>_r<i>.json`; `<stem>.json` for a bare image,
    /// `<stem>_m<n>[_<k>].json` for a DOCX's pictures) read as glyphs on the
    /// pages and regions the router sends a reader [env: FDOC_OCR_WORDS]
    #[arg(long, value_name = "DIR", conflicts_with = "ocr")]
    pub ocr_words: Option<PathBuf>,

    /// Read routed pages and regions with this OCR engine, in this one
    /// command, before analysis [env: FDOC_OCR]
    ///
    /// Needs the assets directory (`--models`). Overrides `ocr.engine` in
    /// the config.
    #[arg(long, value_enum, value_name = "ENGINE")]
    pub ocr: Option<OcrEngineName>,

    /// The OCR assets directory: the models and ONNX Runtime, each checked
    /// against the hash this build pins before loading [env: FDOC_OCR_MODELS]
    #[arg(long, value_name = "DIR")]
    pub models: Option<PathBuf>,

    /// Most megapixels the OCR engine reads in one pass (default 12.5); a
    /// larger image is read in overlapping strips [env:
    /// FDOC_OCR_MAX_MEGAPIXELS]
    ///
    /// Overrides `ocr.max_megapixels` in the config. A pass costs about 230
    /// bytes a pixel while it is read.
    #[arg(long, value_name = "MPX")]
    pub ocr_max_megapixels: Option<f64>,

    /// Emit [[VLM:...]] anchor tokens where escalated crops belong, for an
    /// external tier to fill. With a reader (--tier-results, or a model
    /// called in the run), keep the ones it did not answer
    /// [env: FDOC_VLM_ANCHORS]
    #[arg(long)]
    pub emit_anchors: bool,

    /// Decode the images to tell those holding text from pictures, without
    /// reading them
    ///
    /// Escalation, OCR and the anchors probe the images already. A plain
    /// run does not, and reports each image the text layer does not cover
    /// as not examined (`Unprobed` in `doc:unreadRegions`); with this flag
    /// it reports them as the router sees them (`Region`, `Small`,
    /// `NotTexty`). The decode is most of what an image costs.
    #[arg(long)]
    pub probe_images: bool,

    /// What every node IRI minted by `--format doco` starts with, as
    /// `<IRI>-element-3` (default: the `--doc-iri`, else
    /// `urn:fluree-doc-parse:<stem>-<first 12 hex digits of the SHA-256>`).
    /// One input only
    #[arg(long, value_name = "IRI")]
    pub base_iri: Option<String>,

    /// Stamp `--format doco` nodes with `doc:sourceDocument <IRI>` — the tag
    /// a re-extraction's cleanup transaction retracts by. One input only
    #[arg(long, value_name = "IRI")]
    pub doc_iri: Option<String>,

    /// What `--format doco` records as the input's name, `doc:sourceName`
    /// (default: the input's file name; none from stdin). One input only
    #[arg(long, value_name = "NAME")]
    pub source_name: Option<String>,

    /// Read escalated pages with the configured model, in this one command
    ///
    /// On by default once `fdoc config gemini` has been run. This flag is for
    /// forcing it where the config disables it — with nothing configured it
    /// warns and parses deterministically.
    #[arg(long, conflicts_with = "no_escalate")]
    pub escalate: bool,

    /// Never call a model, whatever the config says
    #[arg(long)]
    pub no_escalate: bool,

    /// Fail a PDF that yields nothing: no text, and no page or region
    /// reported unread
    ///
    /// Such a PDF is warned about either way: a damaged file (a broken
    /// page tree, a flipped byte) reads exactly like a blank one, or one
    /// whose text is drawn as vector outlines. With this
    /// flag it counts as a failed input, and the exit status is non-zero.
    #[arg(long)]
    pub strict: bool,

    /// Save each email's attachments under DIR/<email stem>/, to convert
    /// on their own
    ///
    /// An attachment is a document of its own. Without this flag the
    /// output describes the attachments and does not include them.
    #[arg(long, value_name = "DIR")]
    pub attachments: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct OcrArgs {
    /// A crops directory with its `manifest.jsonl`, or image files (PNG,
    /// JPEG, TIFF, …)
    #[arg(required = true, value_name = "DIR|IMAGE")]
    pub inputs: Vec<PathBuf>,

    /// The engine
    #[arg(long, value_enum, default_value_t = OcrEngineName::OarOcr)]
    pub engine: OcrEngineName,

    /// The OCR assets directory [env: FDOC_OCR_MODELS]
    #[arg(long, value_name = "DIR")]
    pub models: Option<PathBuf>,

    /// Where to write the word files
    #[arg(long, value_name = "DIR")]
    pub out: PathBuf,

    /// Most megapixels the engine reads in one pass (default 12.5); a
    /// larger image is read in overlapping strips [env:
    /// FDOC_OCR_MAX_MEGAPIXELS]
    #[arg(long, value_name = "MPX")]
    pub max_megapixels: Option<f64>,

    /// Images read at once, each on one thread (0 = one per core)
    #[arg(long, short = 'j', default_value_t = 0, value_name = "N")]
    pub jobs: usize,
}

#[derive(Subcommand, Debug)]
pub enum ConfigCommands {
    /// Print the config file in effect, or where one would be written
    Path,
    /// Show the current settings, and whether escalation is ready
    Show,
    /// Write a commented starting config
    Init {
        /// Write the per-user config instead of `./.fdoc/config.toml`
        #[arg(long)]
        global: bool,
    },
    /// Set one dotted key, e.g. `escalation.enabled false`
    Set {
        /// Dotted key, e.g. `escalation.model`
        key: String,
        /// Value; `true`/`false` and integers keep their type
        value: String,
    },
    /// Configure Google Vertex AI as the deep reader
    ///
    /// Point this at a service-account JSON key holding the Vertex AI User
    /// role. The key is validated here rather than mid-batch.
    Gemini {
        /// Path to the service-account JSON key
        #[arg(long, value_name = "FILE")]
        credentials: PathBuf,
        /// Cloud project; read from the key file when omitted
        #[arg(long, value_name = "ID")]
        project: Option<String>,
        /// Model name, passed to Vertex unchanged
        #[arg(long, value_name = "NAME")]
        model: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum DevCommands {
    /// T0/T1 metrics over a corpus directory
    Probe { dir: PathBuf },
    /// Resolve a text span to overlay rectangles
    Find { pdf: PathBuf, text: String },
    /// Assembled lines (layout pass 1)
    Lines { pdf: PathBuf, page: Option<usize> },
    /// Horizontal gap distribution (word/block-split tuning)
    Gaps { path: PathBuf },
    /// Measured gap around a character pair
    Pair { pdf: PathBuf, text: String },
    /// Detected headers/footers/watermarks
    Furniture { pdf: PathBuf },
    /// Vertical gap distribution (leading vs paragraph breaks)
    Leading { path: PathBuf },
    /// Paragraph blocks with furniture stripped
    Blocks { pdf: PathBuf, page: Option<usize> },
    /// PDF bookmark tree (heading ground truth)
    Outline { pdf: PathBuf },
    /// Link annotations with the anchor text each one covers
    Links { pdf: PathBuf },
    /// Detected headings with evidence and level
    Headings { pdf: PathBuf },
    /// Ruling lines and fills (table geometry)
    Rules { pdf: PathBuf, page: Option<usize> },
    /// Detected table grids with cell text
    Tables { pdf: PathBuf, page: Option<usize> },
    /// Chart and diagram regions inferred from drawn shapes
    Figures { pdf: PathBuf, page: Option<usize> },
    /// Fidelity control: our own text checked against the page's glyphs
    Fidelity { pdf: PathBuf },
    /// Column regions with x-occupancy profiles
    Columns { pdf: PathBuf, page: Option<usize> },
    /// Aligned-table candidates before/after corroboration
    Aligned { pdf: PathBuf },
    /// Glyph weight histogram by font size (bold detection)
    Weights { pdf: PathBuf },
    /// Raw glyphs in draw order, optionally restricted to a y-band
    Glyphs {
        pdf: PathBuf,
        #[arg(default_value_t = 0)]
        page: usize,
        y0: Option<f64>,
        y1: Option<f64>,
    },
    /// Render every page to PNG at 2x
    RenderPages {
        path: PathBuf,
        #[arg(default_value = "page-renders")]
        out: PathBuf,
    },
    /// Render routed pages/regions to PNG crops with a splice manifest
    RenderRouted {
        path: PathBuf,
        #[arg(default_value = "routed-crops")]
        out: PathBuf,
        /// Also crop the raster regions too small to be worth a model's call,
        /// which an OCR engine still reads (reason `small_region`)
        #[arg(long)]
        ocr_regions: bool,
        /// Render each crop at the resolution of the image under it (up to
        /// 300 dpi) rather than the model's 2×, for an OCR engine. The
        /// manifest records each crop's `scale` either way
        #[arg(long)]
        stored_resolution: bool,
    },
    /// Render a region manifest's crops via the pipeline's own crop path
    RenderCrops {
        manifest: PathBuf,
        corpus: PathBuf,
        #[arg(default_value = "crops")]
        out: PathBuf,
    },
    /// Each format's projection read back and held to the element model:
    /// violations by format and class (docs/design/conformance.md)
    Conform {
        /// Files or directories to check
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
        /// One JSON line per file, every violation listed
        #[arg(long)]
        json: bool,
    },
    /// Per-stage wall clock over a file or corpus, measured in one process
    Timings {
        path: PathBuf,
        /// Discard this many passes before measuring.
        #[arg(long, default_value_t = 1)]
        warmup: usize,
        /// Report the median of this many measured passes.
        #[arg(long, default_value_t = 5)]
        runs: usize,
    },
}
