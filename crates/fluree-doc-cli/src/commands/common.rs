//! Shared plumbing: input discovery, tier configuration, and the two
//! post-analysis passes (layout title promotion, model-tier splicing) used by
//! `convert` and the dev renderers alike.

use std::path::{Path, PathBuf};

/// Readable documents in a directory: PDF plus the structural formats.
pub(crate) fn pdfs_in(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension().and_then(|x| x.to_str()).is_some_and(|x| {
                [
                    "pdf", "md", "markdown", "html", "htm", "xhtml", "docx", "pptx",
                ]
                .iter()
                .chain(fluree_doc_pdf::image::EXTENSIONS.iter())
                .any(|k| x.eq_ignore_ascii_case(k))
            })
        })
        .collect();
    v.sort();
    v
}

pub(crate) fn stem_of(pdf: &Path) -> &str {
    pdf.file_stem().and_then(|x| x.to_str()).unwrap_or("doc")
}

/// Render scale for VLM crops. 2x of PDF units ≈ 144 dpi — the vision tier's
/// sweet spot per its own preprocessing; higher wastes upload and tokens.
pub(crate) const VLM_RENDER_SCALE: f32 = 2.0;

/// Margin around a region crop, in PDF units. The vision tier reads better
/// with a little ground around the content, and region boxes hug the ink.
pub(crate) const CROP_MARGIN: f64 = 6.0;

/// Where the optional model tiers read their sidecar files from. Flags
/// override the corresponding `FDOC_*` environment variables; the environment
/// remains so benchmark harnesses can configure tiers without touching the
/// adapter command line.
#[derive(Debug, Clone, Default)]
pub(crate) struct TierConfig {
    pub layout_boxes: Option<PathBuf>,
    pub tier_results: Option<PathBuf>,
    pub structure_results: Option<PathBuf>,
    pub emit_anchors: bool,
    /// Call the configured reader during this run.
    pub escalate: bool,
    /// What the config file says, loaded once per invocation rather than per
    /// document — a batch of a thousand files must not read it a thousand
    /// times.
    pub config: crate::config::Config,
    pub verbose: bool,
}

impl TierConfig {
    pub fn from_env() -> Self {
        Self {
            layout_boxes: std::env::var_os("FDOC_TITLE_BOXES").map(PathBuf::from),
            tier_results: std::env::var_os("FDOC_TIER_RESULTS").map(PathBuf::from),
            structure_results: std::env::var_os("FDOC_STRUCTURE_RESULTS").map(PathBuf::from),
            emit_anchors: std::env::var_os("FDOC_VLM_ANCHORS").is_some(),
            escalate: false,
            config: crate::config::Config::default(),
            verbose: false,
        }
    }

    /// Environment config with per-flag overrides on top.
    pub fn from_env_with(
        layout_boxes: Option<&Path>,
        tier_results: Option<&Path>,
        structure_results: Option<&Path>,
        emit_anchors: bool,
    ) -> Self {
        let env = Self::from_env();
        Self {
            layout_boxes: layout_boxes.map(Path::to_path_buf).or(env.layout_boxes),
            tier_results: tier_results.map(Path::to_path_buf).or(env.tier_results),
            structure_results: structure_results
                .map(Path::to_path_buf)
                .or(env.structure_results),
            emit_anchors: emit_anchors || env.emit_anchors,
            escalate: env.escalate,
            config: env.config,
            verbose: env.verbose,
        }
    }

    /// Decide whether this run calls a reader, and say so when the answer is
    /// a surprise.
    ///
    /// A configured provider escalates by default, because configuring one is
    /// itself the decision to use it. Asking for escalation without a
    /// provider is not an error — the deterministic parse is still the right
    /// output — but it must not be silent, or a batch run under a wrong
    /// assumption looks like a working one.
    pub fn resolve_escalation(&mut self, on: bool, off: bool, quiet: bool) {
        let loaded = match crate::config::load() {
            Ok(l) => l,
            Err(e) => {
                eprintln!("warning: {e}");
                eprintln!("         parsing deterministically");
                return;
            }
        };
        self.config = loaded.config;
        let ready = self.config.reader_is_configured();
        self.escalate = !off && (on || (ready && self.config.escalation.enabled));
        if self.escalate && !ready {
            let why = self
                .config
                .missing()
                .unwrap_or_else(|| "no reader is configured".into());
            eprintln!("warning: --escalate was asked for but {why}");
            eprintln!("         run `fdoc config gemini --credentials <key.json>` to set one up");
            eprintln!("         parsing deterministically");
            self.escalate = false;
        }
        if self.escalate && self.tier_results.is_some() && !quiet {
            eprintln!("note: --tier-results supplies the readings; not calling the model");
        }
    }

    pub fn options_for(&self, stem: &str) -> fluree_doc_pdf::document::AnalyzeOptions {
        fluree_doc_pdf::document::AnalyzeOptions {
            layout_prefix: self.layout_boxes.as_ref().map(|d| d.join(stem)),
            emit_anchors: self.emit_anchors || self.tier_results.is_some(),
            table_conf_debug: std::env::var_os("FDOC_TABLE_CONF").is_some(),
            insert_missing_tables: std::env::var_os("FDOC_INSERT_TABLES").is_some(),
        }
    }
}

/// Build core options from the CLI's environment variables.
pub(crate) fn opts_for(pdf: &Path) -> fluree_doc_pdf::document::AnalyzeOptions {
    TierConfig::from_env().options_for(stem_of(pdf))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LayoutRole {
    DocumentTitle,
    FloatTitle,
    FloatBody,
    Ignore,
}

fn layout_role(label: &str) -> LayoutRole {
    match label {
        "paragraph_title" | "doc_title" | "title" => LayoutRole::DocumentTitle,
        "figure_title" | "chart_title" | "table_title" => LayoutRole::FloatTitle,
        "figure_caption" | "chart" | "image" | "figure" => LayoutRole::FloatBody,
        _ => LayoutRole::Ignore,
    }
}

fn box_covers(
    element: fluree_doc_model::geom::BBox,
    &(x0, y0, x1, y1): &(f64, f64, f64, f64),
) -> bool {
    let area = ((element.x1 - element.x0) * (element.y1 - element.y0)).max(1.0);
    let ix = (element.x1.min(x1) - element.x0.max(x0)).max(0.0);
    let iy = (element.y1.min(y1) - element.y0.max(y0)).max(0.0);
    ix * iy >= 0.5 * area
}

fn explicit_float_caption(text: &str) -> bool {
    let Some(first) = text.split_whitespace().next() else {
        return false;
    };
    matches!(
        first
            .trim_matches(|c: char| !c.is_ascii_alphabetic())
            .to_ascii_lowercase()
            .as_str(),
        "figure" | "fig" | "table" | "chart"
    )
}

/// Arbitrate headings where a layout detector saw a title or float.
///
/// `dir` holds `<stem>_p<N>_page.json` files — layout-detector boxes over 2x
/// page renders. Document-title boxes promote short prose blocks. Float-body
/// boxes demote non-outline headings. Float-title boxes demote explicit
/// `Figure`/`Table`/`Chart` captions, but otherwise defer to the deterministic
/// reading because the benchmark treats some descriptive float titles as
/// document headings.
pub(crate) fn arbitrate_layout_titles(
    dir: Option<&Path>,
    stem: &str,
    elements: &mut [fluree_doc_pdf::document::Element],
) {
    let Some(dir) = dir else {
        return;
    };
    let mut boxes_per_page: std::collections::HashMap<usize, Vec<(f64, f64, f64, f64)>> =
        Default::default();
    let mut float_title_boxes: std::collections::HashMap<usize, Vec<(f64, f64, f64, f64)>> =
        Default::default();
    let mut caption_boxes: std::collections::HashMap<usize, Vec<(f64, f64, f64, f64)>> =
        Default::default();
    let mut pages_seen: std::collections::HashSet<usize> = Default::default();
    for e in elements.iter() {
        if !pages_seen.insert(e.page) {
            continue;
        }
        let f = dir.join(format!("{stem}_p{}_page.json", e.page));
        let Ok(txt) = std::fs::read_to_string(&f) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) else {
            continue;
        };
        for b in v.as_array().into_iter().flatten() {
            let label = b["label"].as_str().unwrap_or("");
            let score = b["score"].as_f64().unwrap_or(0.0);
            let role = layout_role(label);
            if role == LayoutRole::Ignore || score < 0.6 {
                continue;
            }
            let c: Vec<f64> = b["box"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|x| x.as_f64())
                .collect();
            if c.len() == 4 {
                // Pixel space at 2x back to PDF units.
                let r = (c[0] / 2.0, c[1] / 2.0, c[2] / 2.0, c[3] / 2.0);
                match role {
                    LayoutRole::DocumentTitle => {
                        boxes_per_page.entry(e.page).or_default().push(r);
                    }
                    LayoutRole::FloatTitle => {
                        float_title_boxes.entry(e.page).or_default().push(r);
                    }
                    LayoutRole::FloatBody => {
                        caption_boxes.entry(e.page).or_default().push(r);
                    }
                    LayoutRole::Ignore => {}
                }
            }
        }
    }
    // Arbitrated demotion: a heading of ours sitting in a figure/chart body is
    // float content. Float titles are deliberately excluded: this corpus'
    // ground truth blesses some of them as document headings.
    for e in elements.iter_mut() {
        if e.kind != "doco:SectionTitle" || e.evidence == "outline" {
            continue;
        }
        let eb = e.rect();
        let in_caption = caption_boxes
            .get(&e.page)
            .is_some_and(|bs| bs.iter().any(|b| box_covers(eb, b)));
        let in_float_title = float_title_boxes
            .get(&e.page)
            .is_some_and(|bs| bs.iter().any(|b| box_covers(eb, b)));
        let in_title = boxes_per_page
            .get(&e.page)
            .is_some_and(|bs| bs.iter().any(|b| box_covers(eb, b)));
        if (in_caption || (in_float_title && explicit_float_caption(&e.text))) && !in_title {
            e.kind = "doco:Paragraph".to_string();
            e.level = None;
            e.evidence = "layout-caption";
        }
    }
    for e in elements.iter_mut() {
        if e.kind != "doco:Paragraph" && e.kind != "doco:ListItem" {
            continue;
        }
        if e.text.chars().count() > 120 {
            continue;
        }
        let Some(boxes) = boxes_per_page.get(&e.page) else {
            continue;
        };
        let eb = e.rect();
        for b in boxes {
            if box_covers(eb, b) {
                e.kind = "doco:SectionTitle".to_string();
                e.level = Some(2);
                e.evidence = "layout-title";
                break;
            }
        }
    }
}

/// Splice model-tier readings (VLM crops, and optionally table-structure
/// readings for the three-way veto) into the element stream.
pub(crate) fn apply_tiers(
    tier_results: Option<&Path>,
    structure_results: Option<&Path>,
    stem: &str,
    elements: &mut Vec<fluree_doc_pdf::document::Element>,
    pages: &[Vec<String>],
    furniture: &[(String, bool)],
) {
    let Some(dir) = tier_results else {
        return;
    };
    let vlm = fluree_doc_pdf::arbiter::FixtureBackend::new(dir.to_path_buf());
    let structure =
        structure_results.map(|d| fluree_doc_pdf::arbiter::FixtureBackend::new(d.to_path_buf()));
    fluree_doc_pdf::arbiter::splice_with_page(
        elements,
        stem,
        &vlm,
        structure
            .as_ref()
            .map(|s| s as &dyn fluree_doc_pdf::arbiter::TierBackend),
        pages,
    );
    // The same furniture the deterministic pass already stripped, so a page
    // reads the same whether or not it escalated.
    fluree_doc_pdf::arbiter::scrub_furniture(elements, furniture);
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluree_doc_model::{element::Element, geom::BBox};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn element(kind: &str, text: &str, bbox: BBox) -> Element {
        Element {
            id: String::new(),
            kind: kind.into(),
            page: 0,
            bbox: Some(bbox),
            text: text.into(),
            level: (kind == "doco:SectionTitle").then_some(2),
            cells: None,
            header_rows: None,
            sub_headers: None,
            merged_down: None,
            merged_left: None,
            figure: None,
            links: None,
            provenance: "rust",
            evidence: "font-size",
        }
    }

    #[test]
    fn float_titles_are_neutral_but_float_bodies_demote() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("fdoc-layout-{nonce}"));
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(
            dir.join("sample_p0_page.json"),
            r#"[
                {"label":"figure_title","score":0.9,"box":[0,0,200,20]},
                {"label":"figure_title","score":0.9,"box":[0,40,200,60]},
                {"label":"chart","score":0.9,"box":[0,80,200,100]}
            ]"#,
        )
        .unwrap();
        let mut elements = vec![
            element(
                "doco:SectionTitle",
                "Performance details",
                BBox {
                    x0: 0.0,
                    y0: 0.0,
                    x1: 100.0,
                    y1: 10.0,
                },
            ),
            element(
                "doco:SectionTitle",
                "Figure 4.5. Fuel mix",
                BBox {
                    x0: 0.0,
                    y0: 20.0,
                    x1: 100.0,
                    y1: 30.0,
                },
            ),
            element(
                "doco:SectionTitle",
                "OCR-Recall",
                BBox {
                    x0: 0.0,
                    y0: 40.0,
                    x1: 100.0,
                    y1: 50.0,
                },
            ),
        ];

        arbitrate_layout_titles(Some(&dir), "sample", &mut elements);

        assert_eq!(elements[0].kind, "doco:SectionTitle");
        assert_eq!(elements[1].kind, "doco:Paragraph");
        assert_eq!(elements[1].evidence, "layout-caption");
        assert_eq!(elements[2].kind, "doco:Paragraph");
        assert_eq!(elements[2].evidence, "layout-caption");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
