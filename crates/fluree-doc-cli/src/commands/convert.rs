//! `fdoc convert` — the product command: PDFs in, Markdown/XHTML/JSON out.

use crate::cli::{ConvertArgs, Format};
use crate::commands::common::{self, TierConfig};
use fluree_doc_pdf::document::Element;
use fluree_doc_pdf::{outline, ExtractOptions};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// The input being converted, beyond its bytes.
struct Source<'a> {
    /// What sidecar files and messages name it by: the file's stem, or
    /// `stdin`.
    stem: &'a str,
    /// What this input's outputs are named by in its run: its stem, unless
    /// another input shares that (see [`slots`]).
    slot: String,
    /// Whether `stem` is a file's own name rather than a stand-in.
    named: bool,
    /// What the output records as the input's name: `--source-name`, else
    /// the file's name. Standard input has none.
    name: Option<String>,
    /// The input's bytes as lowercase hex SHA-256.
    sha256: String,
    /// A file an e-mail carries, converted by `--parse-attachments`: the
    /// files it carries in turn are followed, or not, by that run, not
    /// saved by it.
    carried: bool,
}

impl<'a> Source<'a> {
    fn file(path: &'a Path, slot: &str, data: &[u8], args: &ConvertArgs) -> Self {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
        Source {
            stem: common::stem_of(path),
            slot: slot.to_string(),
            named: true,
            name: args.source_name.clone().or(name),
            sha256: fluree_doc_model::sha256_hex(data),
            carried: false,
        }
    }

    fn stdin(data: &[u8], args: &ConvertArgs) -> Self {
        Source {
            stem: "stdin",
            slot: "stdin".to_string(),
            named: false,
            name: args.source_name.clone(),
            sha256: fluree_doc_model::sha256_hex(data),
            carried: false,
        }
    }
}

pub fn run(args: &ConvertArgs, verbose: bool, quiet: bool) -> i32 {
    let status = convert(args, verbose, quiet);
    // An engine opened on its first read that could not open ends the run
    // as one refused before it started did, whatever was converted.
    if status == 0 && crate::commands::ocr::engine_failed() {
        2
    } else {
        status
    }
}

fn convert(args: &ConvertArgs, verbose: bool, quiet: bool) -> i32 {
    let pages = match args.pages.as_deref().map(parse_pages) {
        Some(Ok(set)) => Some(set),
        Some(Err(e)) => {
            eprintln!("error: --pages: {e}");
            return 2;
        }
        None => None,
    };
    let mut cfg = TierConfig::from_env_with(
        args.layout_boxes.as_deref(),
        args.tier_results.as_deref(),
        args.structure_results.as_deref(),
        args.ocr_words.as_deref(),
        args.emit_anchors,
    );
    // Once per invocation, not once per document.
    cfg.resolve_escalation(args.escalate, args.no_escalate, quiet);
    cfg.verbose = verbose;
    // A flag beats the environment: `--ocr` given on the command line reads
    // the pixels, whatever `FDOC_OCR_WORDS` holds (the two flags together are
    // refused by the parser).
    if args.ocr.is_some() && args.ocr_words.is_none() && cfg.ocr_words.take().is_some() && verbose {
        eprintln!("note: --ocr given, FDOC_OCR_WORDS ignored");
    }
    // A word directory that is not there is a mistake in the command, not
    // a batch with nothing read yet.
    if let Some(dir) = cfg.ocr_words.as_deref().filter(|d| !d.is_dir()) {
        eprintln!("error: --ocr-words: {} is not a directory", dir.display());
        return 2;
    }
    // Words already read win over reading them again with an engine the
    // config names, as `--tier-results` wins over a configured model.
    if cfg.ocr_words.is_none() {
        match crate::commands::ocr::engine_for_convert(
            args.ocr,
            args.models.as_deref(),
            args.ocr_max_megapixels,
            &cfg.config.ocr,
        ) {
            Ok(engine) => cfg.ocr = engine.map(common::OcrHandle),
            Err(e) => {
                eprintln!("error: --ocr: {e}");
                return 2;
            }
        }
    }
    if args.models.is_some() && cfg.ocr.is_none() && !quiet {
        eprintln!(
            "warning: --models names an OCR engine's assets, but no engine reads with them (--ocr)"
        );
    }
    let cfg = &cfg;

    // Expand directories; `-` means stdin and must be the sole input.
    let stdin_input = args.inputs.len() == 1 && args.inputs[0] == Path::new("-");
    let files: Vec<PathBuf> = if stdin_input {
        Vec::new()
    } else {
        let mut v = Vec::new();
        for input in &args.inputs {
            if input.is_dir() {
                v.extend(common::sources_in(input));
            } else {
                v.push(input.clone());
            }
        }
        v
    };

    if stdin_input {
        let mut data = Vec::new();
        if let Err(e) = std::io::stdin().lock().read_to_end(&mut data) {
            eprintln!("error: reading stdin: {e}");
            return 1;
        }
        let src = Source::stdin(&data, args);
        // Named as nothing: read by what its content is, through the one
        // dispatch every input takes (`input_kind`), so a format told by
        // its content is read from stdin as from a file.
        let converted = convert_source(
            Path::new("-"),
            data,
            &src,
            cfg,
            args,
            pages.as_deref(),
            quiet,
        );
        return match converted {
            Ok(out) => write_out(&out, args.output.as_deref()),
            Err(e) => {
                eprintln!("error: stdin: {e}");
                1
            }
        };
    }

    // Readings are looked up by stem: two inputs of one stem (`report.pdf`
    // and `report.png`) would each find the other's.
    if (cfg.ocr_words.is_some() || cfg.tier_results.is_some()) && !quiet {
        let mut seen: std::collections::HashMap<&str, &Path> = Default::default();
        for f in &files {
            let stem = common::stem_of(f);
            if let Some(other) = seen.insert(stem, f) {
                eprintln!(
                    "warning: {} and {} share the name {stem}: readings looked up by it may be the other's",
                    other.display(),
                    f.display()
                );
            }
        }
    }

    if files.is_empty() {
        eprintln!("error: no PDF inputs found");
        return 2;
    }
    if files.len() > 1 && args.out_dir.is_none() {
        eprintln!("error: multiple inputs require --out-dir");
        return 2;
    }
    // Given to several inputs, a flag naming one document would name them
    // all alike: one set of node IRIs, so their graphs merge in a store,
    // and one tag, so a retraction removes them together.
    if let (true, Some(flag)) = (files.len() > 1, single_input_flag(args)) {
        eprintln!(
            "error: {flag} names one input, and there are {}",
            files.len()
        );
        return 2;
    }

    // Single file: to stdout or -o.
    if files.len() == 1 && args.out_dir.is_none() {
        let f = &files[0];
        let t0 = std::time::Instant::now();
        let r = convert_path(f, common::stem_of(f), cfg, args, pages.as_deref(), quiet);
        if verbose {
            eprintln!("{}: {:.1}ms", f.display(), t0.elapsed().as_secs_f64() * 1e3);
        }
        return match r {
            Ok(out) => write_out(&out, args.output.as_deref()),
            Err(e) => {
                eprintln!("error: {}: {e}", f.display());
                1
            }
        };
    }

    // Batch: one output file per input, worker threads over a shared cursor.
    let out_dir = args.out_dir.as_deref().unwrap();
    if let Err(e) = std::fs::create_dir_all(out_dir) {
        eprintln!("error: cannot create {}: {e}", out_dir.display());
        return 1;
    }
    let cores = std::thread::available_parallelism()
        .map(std::num::NonZero::get)
        .unwrap_or(1);
    // Each input, which its document holds whole beside its budget.
    let sizes: Vec<u64> = files
        .iter()
        .map(|f| std::fs::metadata(f).map_or(0, |m| m.len()))
        .collect();
    let smallest = sizes.iter().copied().min().unwrap_or(0);
    // What the caller says a document takes, in place of the estimate.
    let per_job = match job_memory(
        args.job_memory,
        std::env::var("FDOC_JOB_MEMORY").ok().as_deref(),
    ) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    // The OCR engine reads for every document, in memory of its own.
    let ocr = cfg.ocr.as_ref().map_or(0, |e| e.0.most_memory());
    let free = free_memory();
    let (jobs, lowered) = batch_jobs(args.jobs, cores, free, smallest, ocr, per_job);
    // Told what a document takes, each is read within two thirds of it, as
    // the default budget is of the 3.75 GiB a document is counted to take.
    let share = per_job.map_or(u64::MAX, |m| m / 3 * 2);
    // Lowered to no fewer than the files there are, it changes nothing.
    if let Some(why) = lowered.filter(|_| !quiet && jobs < files.len()) {
        eprintln!("note: {why}");
    }
    let jobs = jobs.min(files.len());
    // `report.pdf` and `report.docx` share a stem, so naming outputs by stem
    // alone makes one silently overwrite the other. Disambiguate only where
    // a stem actually repeats, so the ordinary single-format batch keeps
    // plain names.
    let slots = slots(&files);
    let cursor = AtomicUsize::new(0);
    let failures = AtomicUsize::new(0);
    // Each document admitted by its own size: a large input waits for room
    // rather than lowering `-j` for the whole batch.
    let admission = Admission::new(free.map(|f| f.saturating_sub(ocr)));
    // Each worker reads within the limits this command was given.
    let limits = fluree_doc_model::budget::limits();
    std::thread::scope(|s| {
        for _ in 0..jobs {
            s.spawn(|| {
                fluree_doc_model::budget::limited_to(limits, || loop {
                    let i = cursor.fetch_add(1, Ordering::Relaxed);
                    let Some(f) = files.get(i) else { break };
                    let _room = admission.admit(job_need(sizes[i], per_job));
                    let t0 = std::time::Instant::now();
                    let converted = fluree_doc_model::budget::within(share, || {
                        convert_path(f, &slots[i], cfg, args, pages.as_deref(), quiet)
                    });
                    match converted {
                        Ok(out) => {
                            let dst = &out_dir.join(format!("{}.{}", slots[i], ext(args.format)));
                            if let Err(e) = std::fs::write(dst, out) {
                                eprintln!("error: writing {}: {e}", dst.display());
                                failures.fetch_add(1, Ordering::Relaxed);
                            } else if verbose {
                                eprintln!(
                                    "{}: {:.1}ms",
                                    f.display(),
                                    t0.elapsed().as_secs_f64() * 1e3
                                );
                            }
                        }
                        Err(e) => {
                            eprintln!("error: {}: {e}", f.display());
                            failures.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                })
            });
        }
    });
    let failed = failures.load(Ordering::Relaxed);
    if !quiet {
        eprintln!(
            "converted {}/{} files -> {}",
            files.len() - failed,
            files.len(),
            out_dir.display()
        );
    }
    if failed > 0 {
        1
    } else {
        0
    }
}

/// What a document takes beside its budget and its input, whatever the
/// budget: the readers' own bounds on what the budget does not count (an
/// image decoded within 1 GiB, a part read beside the document) are fixed,
/// sized for the shipped budget, and do not shrink with `--max-memory`.
/// 1.25 GiB, half the shipped budget: a document at it was measured to fit
/// in 4 GB.
const LIBRARY_MEMORY: u64 = 5 << 28;

/// What one document of a batch may take at once, its input `input` bytes:
/// its whole budget (the memory of the limits in effect, `MAX_MEMORY`
/// unless `--max-memory` says otherwise), its input, held whole, and
/// [`LIBRARY_MEMORY`]: 3.75 GiB and the input by default. The last two do
/// not shrink with the budget: a margin that did let a lowered
/// `--max-memory` run more documents at once than the memory free holds.
///
/// A page limit past the pages a PDF's page tree may list by default
/// lets the parser keep that many more pages before any is read: what
/// they take is counted too
/// ([`fluree_doc_pdf::page_tree::MAX_PAGE_TREE_PAGES`]).
fn document_memory(input: u64) -> u64 {
    use fluree_doc_pdf::page_tree::{MAX_PAGE_TREE_PAGES, PAGE_TREE_PAGE_BYTES};
    let limits = fluree_doc_model::budget::limits();
    let pages = (limits.pages as u64).saturating_sub(MAX_PAGE_TREE_PAGES);
    limits
        .memory
        .saturating_add(LIBRARY_MEMORY)
        .saturating_add(pages.saturating_mul(PAGE_TREE_PAGE_BYTES))
        .saturating_add(input)
}

/// What one document of a batch is counted to take at once, its input
/// `input` bytes: what `--job-memory` says (`per_job`), and its input, held
/// whole beside it; else the estimate, [`document_memory`]. Told, it is the
/// caller's own measure of what a document takes on that machine, and the
/// document is read within two thirds of it.
fn job_need(input: u64, per_job: Option<u64>) -> u64 {
    per_job.map_or_else(|| document_memory(input), |m| m.saturating_add(input))
}

/// The memory a batch's documents take at once ([`job_need`]),
/// within `room`, the memory free less what OCR sets aside: each admitted
/// by its own size, waiting while those in flight leave too little, and
/// admitted alone when none is. `None`, not known: each admitted at once.
struct Admission {
    room: Option<u64>,
    /// What the documents in flight take, and how many they are.
    taken: std::sync::Mutex<(u64, usize)>,
    freed: std::sync::Condvar,
}

/// A document admitted: its memory given back when it is dropped.
struct Admitted<'a> {
    admission: &'a Admission,
    need: u64,
}

impl Admission {
    fn new(room: Option<u64>) -> Self {
        Self {
            room,
            taken: std::sync::Mutex::new((0, 0)),
            freed: std::sync::Condvar::new(),
        }
    }

    /// Wait for room for a document that may take `need`, and take it.
    fn admit(&self, need: u64) -> Admitted<'_> {
        let mut taken = self.taken.lock().unwrap_or_else(|e| e.into_inner());
        while !self.fits(*taken, need) {
            taken = self.freed.wait(taken).unwrap_or_else(|e| e.into_inner());
        }
        self.take(&mut taken, need)
    }

    /// [`Admission::admit`], or `None` where it would wait.
    #[cfg(test)]
    fn try_admit(&self, need: u64) -> Option<Admitted<'_>> {
        let mut taken = self.taken.lock().unwrap_or_else(|e| e.into_inner());
        self.fits(*taken, need).then(|| self.take(&mut taken, need))
    }

    /// Whether a document taking `need` runs beside those in flight,
    /// `(taken, count)`: within the room, or alone.
    fn fits(&self, (taken, count): (u64, usize), need: u64) -> bool {
        count == 0
            || self
                .room
                .is_none_or(|room| taken.saturating_add(need) <= room)
    }

    fn take(&self, taken: &mut (u64, usize), need: u64) -> Admitted<'_> {
        *taken = (taken.0.saturating_add(need), taken.1 + 1);
        Admitted {
            admission: self,
            need,
        }
    }
}

impl Drop for Admitted<'_> {
    fn drop(&mut self) {
        let a = self.admission;
        let mut taken = a.taken.lock().unwrap_or_else(|e| e.into_inner());
        *taken = (taken.0.saturating_sub(self.need), taken.1.saturating_sub(1));
        a.freed.notify_all();
    }
}

/// What `--job-memory` (`flag`), or else `FDOC_JOB_MEMORY` (`env`), sets a
/// batch document to take, in bytes: `None` where neither is set.
fn job_memory(flag: Option<u64>, env: Option<&str>) -> Result<Option<u64>, String> {
    let mib = match (flag, env.map(str::trim).filter(|v| !v.is_empty())) {
        (Some(m), _) => m,
        (None, Some(v)) => v
            .parse::<u64>()
            .map_err(|_| format!("FDOC_JOB_MEMORY={v:?} is not a whole number of MiB"))?,
        (None, None) => return Ok(None),
    };
    if mib == 0 {
        return Err("--job-memory: 0 MiB leaves a document nothing to be read with".into());
    }
    Ok(Some(mib.saturating_mul(1 << 20)))
}

/// How many documents a batch converts at once, and why, where it is fewer
/// than `requested` (`0`, one per core of `cores`), its smallest input
/// `smallest` bytes, with `beside` bytes set aside for an OCR engine the
/// documents share, and `per_job` what `--job-memory` says a document
/// takes, where it is set.
///
/// Each document may take its whole budget and what goes with it
/// ([`job_need`]: [`document_memory`], or what `per_job` says) at once, so
/// no more run together than `memory`, the memory free to the process less
/// `beside`, holds, and at least one; where that is not known, no more than
/// one per core, unless `per_job` is set, which says the caller knows what
/// the machine holds (a batch waiting on the network wants more than its
/// cores). Counted here at the smallest input, as many as could ever run at
/// once: each document is then admitted by its own size ([`Admission`]), so
/// one large input waits for room rather than lowering `-j` for every
/// other. A document past its budget fails on its own, but documents within
/// theirs that together pass what the machine has would abort the whole
/// batch, or have it killed.
fn batch_jobs(
    requested: usize,
    cores: usize,
    memory: Option<u64>,
    smallest: u64,
    beside: u64,
    per_job: Option<u64>,
) -> (usize, Option<String>) {
    let asked = if requested == 0 { cores } else { requested };
    let document = job_need(smallest, per_job).max(1);
    let (bound, why) = match (memory, per_job) {
        (Some(free), _) => (
            usize::try_from(free.saturating_sub(beside) / document)
                .unwrap_or(usize::MAX)
                .max(1),
            if beside == 0 {
                format!(
                    "{} MiB of memory free, {} MiB a document",
                    free >> 20,
                    document >> 20
                )
            } else if beside >= free {
                format!(
                    "{} MiB of memory free, less than the {} MiB OCR sets aside, {} MiB a document",
                    free >> 20,
                    beside >> 20,
                    document >> 20
                )
            } else {
                format!(
                    "{} MiB of memory free, {} MiB of it for OCR, {} MiB a document",
                    free >> 20,
                    beside >> 20,
                    document >> 20
                )
            },
        ),
        (None, Some(_)) => return (asked, None),
        (None, None) => (
            cores,
            "the free memory is not known; one per core".to_string(),
        ),
    };
    if asked <= bound {
        return (asked, None);
    }
    let jobs_flag = if requested == 0 {
        "-j 0 (one per core)".to_string()
    } else {
        format!("-j {requested}")
    };
    (
        bound,
        Some(format!("{jobs_flag} lowered to {bound}: {why}")),
    )
}

/// The memory free to this process. On Linux, the least of: what the
/// system has available (`MemAvailable`), what its control groups leave it
/// (a container's limit), and what its address space may grow to. On
/// macOS, what `vm_stat` counts free, inactive or speculative, the pages
/// the system hands out before it compresses or swaps ([`vm_stat_available`]);
/// a limit on the process is not looked at there. `None` where nothing is
/// known (on any other system).
pub(crate) fn free_memory() -> Option<u64> {
    #[cfg(target_os = "macos")]
    if let Some(free) = std::process::Command::new("/usr/bin/vm_stat")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| vm_stat_available(&String::from_utf8_lossy(&o.stdout)))
    {
        return Some(free);
    }
    free_memory_in(|path| std::fs::read_to_string(path).ok())
}

/// The bytes `vm_stat`'s report `out` counts free, inactive or speculative,
/// at the page size its first line states; `None` where it states none, or
/// counts none of them.
#[cfg(any(test, target_os = "macos"))]
fn vm_stat_available(out: &str) -> Option<u64> {
    let page: u64 = out
        .split("page size of ")
        .nth(1)?
        .split(' ')
        .next()?
        .parse()
        .ok()?;
    let mut pages = None;
    for line in out.lines() {
        let Some((name, count)) = line.split_once(':') else {
            continue;
        };
        if matches!(
            name.trim(),
            "Pages free" | "Pages inactive" | "Pages speculative"
        ) {
            let n: u64 = count.trim().trim_end_matches('.').parse().ok()?;
            pages = Some(pages.unwrap_or(0u64).saturating_add(n));
        }
    }
    pages.map(|p| p.saturating_mul(page))
}

/// [`free_memory`], its files read by `read`.
fn free_memory_in(read: impl Fn(&str) -> Option<String>) -> Option<u64> {
    [
        read("/proc/meminfo").and_then(|m| mem_available(&m)),
        read("/proc/self/cgroup").and_then(|c| cgroup_headroom(&c, &read)),
        read("/proc/self/limits").and_then(|l| address_space(&l)),
    ]
    .into_iter()
    .flatten()
    .min()
}

/// `MemAvailable` of `/proc/meminfo`, in bytes.
fn mem_available(meminfo: &str) -> Option<u64> {
    let kb = meminfo
        .lines()
        .find_map(|l| l.strip_prefix("MemAvailable:"))?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?;
    Some(kb.saturating_mul(1024))
}

/// What the memory control groups of this process, as `/proc/self/cgroup`
/// names them, leave it: the least, over its group and each above it, of
/// its limit less what the group uses, a group without a limit not
/// counting. What it uses is its working set, as docker and the kubelet
/// count it: its use less the file cache the kernel reclaims first under
/// the limit (`inactive_file` in its `memory.stat`), which a group that has
/// read or written much keeps near its limit. `read` reads a file under
/// `/sys/fs/cgroup`. cgroup v2 names one group (`0::/path`), whose
/// `memory.max`, `memory.current` and `memory.stat` are read; v1 a group
/// per controller, the memory one's `memory.limit_in_bytes`,
/// `memory.usage_in_bytes` and `memory.stat` (`total_inactive_file`, its
/// subgroups' included, as its use) read under `/sys/fs/cgroup/memory`. A container
/// sees its own group at the root of the mount and its path from the host,
/// so each group from the path's to the mount's root is read where it is
/// there. `None` where no group has a limit.
fn cgroup_headroom(cgroup: &str, read: impl Fn(&str) -> Option<String>) -> Option<u64> {
    let number = |path: String| read(&path)?.trim().parse::<u64>().ok();
    let mut least: Option<u64> = None;
    for line in cgroup.lines() {
        let mut fields = line.splitn(3, ':');
        let (Some(id), Some(controllers), Some(path)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let (root, limit, usage, cache) = if id == "0" && controllers.is_empty() {
            (
                "/sys/fs/cgroup",
                "memory.max",
                "memory.current",
                "inactive_file",
            )
        } else if controllers.split(',').any(|c| c == "memory") {
            (
                "/sys/fs/cgroup/memory",
                "memory.limit_in_bytes",
                "memory.usage_in_bytes",
                "total_inactive_file",
            )
        } else {
            continue;
        };
        // The group, then each above it, to the mount's root.
        let mut dir = path.trim_end_matches('/');
        loop {
            // `max` (v2) is no limit, and does not parse; v1 writes a huge
            // number, which counts for nothing beside the others.
            if let (Some(max), Some(used)) = (
                number(format!("{root}{dir}/{limit}")),
                number(format!("{root}{dir}/{usage}")),
            ) {
                let reclaimable = read(&format!("{root}{dir}/memory.stat"))
                    .and_then(|stat| stat_field(&stat, cache))
                    .unwrap_or(0);
                let left = max.saturating_sub(used.saturating_sub(reclaimable));
                least = Some(least.map_or(left, |l| l.min(left)));
            }
            match dir.rfind('/') {
                Some(i) => dir = &dir[..i],
                None => break,
            }
        }
    }
    least
}

/// The value of `key` in a cgroup's `memory.stat` (`key value` lines).
fn stat_field(stat: &str, key: &str) -> Option<u64> {
    stat.lines().find_map(|l| {
        let (k, v) = l.split_once(' ')?;
        (k == key).then(|| v.trim().parse().ok())?
    })
}

/// The soft limit on the address space in `/proc/self/limits`, in bytes;
/// `None` where it is `unlimited`.
fn address_space(limits: &str) -> Option<u64> {
    limits
        .lines()
        .find_map(|l| l.strip_prefix("Max address space"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// The first flag given that names a single document.
fn single_input_flag(args: &ConvertArgs) -> Option<&'static str> {
    [
        ("--doc-iri", args.doc_iri.is_some()),
        ("--base-iri", args.base_iri.is_some()),
        ("--source-name", args.source_name.is_some()),
    ]
    .into_iter()
    .find_map(|(flag, given)| given.then_some(flag))
}

fn convert_path(
    path: &Path,
    slot: &str,
    cfg: &TierConfig,
    args: &ConvertArgs,
    pages: Option<&[usize]>,
    quiet: bool,
) -> Result<String, String> {
    // What the OCR engine says about an image it reads names this file.
    let _doc = super::ocr::Reading::of(path);
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let src = Source::file(path, slot, &data, args);
    convert_source(path, data, &src, cfg, args, pages, quiet)
}

/// Convert `data`, named by `path` (whose extension may say what it is),
/// as the input `src`.
///
/// Every input is read through here, so here the readers are told the
/// output: a document's budget counts what that output writes of it
/// (`fluree_doc_model::budget::Output`), so a table too large for DoCO,
/// which writes each cell as a node, still converts to Markdown.
fn convert_source(
    path: &Path,
    data: Vec<u8>,
    src: &Source,
    cfg: &TierConfig,
    args: &ConvertArgs,
    pages: Option<&[usize]>,
    quiet: bool,
) -> Result<String, String> {
    use fluree_doc_model::budget::{converting_to, Output};
    let output = match args.format {
        Format::Doco => Output::Doco,
        Format::Text => Output::Text,
        // Conformance writes Markdown and XHTML from the elements.
        Format::Md | Format::Xhtml | Format::Json | Format::Conform => Output::Flat,
    };
    converting_to(output, || {
        read_source(path, data, src, cfg, args, pages, quiet)
    })
}

/// [`convert_source`], within its output's budget.
fn read_source(
    path: &Path,
    data: Vec<u8>,
    src: &Source,
    cfg: &TierConfig,
    args: &ConvertArgs,
    pages: Option<&[usize]>,
    quiet: bool,
) -> Result<String, String> {
    use common::SourceKind;
    // By its content where that says, else by its name (`input_kind`).
    let kind = common::input_kind(path, &data);
    match kind {
        SourceKind::Transcript => return convert_transcript(&data, src, args, quiet),
        SourceKind::Email => return convert_email(&data, src, cfg, args, quiet),
        SourceKind::Image => return convert_image(data, src, cfg, args, quiet),
        SourceKind::Xps => return convert_xps(&data, src, cfg, args, pages, quiet),
        SourceKind::Unsupported => {
            return Err(common::unsupported(Some(path), &data)
                .unwrap_or_else(|| "unsupported format".into()))
        }
        _ => {}
    }
    // What a file declares about itself: its title, author and dates.
    let declared = |info| fluree_doc_model::Notes {
        info,
        ..Default::default()
    };
    // Structural formats: the source declares what a PDF makes us infer, so
    // these readers map rather than measure and carry no geometry.
    let (elements, notes) = match kind {
        SourceKind::Markdown => {
            let text = String::from_utf8(data).map_err(|e| format!("not UTF-8: {e}"))?;
            (
                fluree_doc_markdown::parse(&text)
                    .map_err(|e| format!("Markdown document too large: {}", common::refused(&e)))?,
                fluree_doc_model::Notes::default(),
            )
        }
        // Plain text is read as lines, not as Markdown: an indented line
        // is not code and a row of `=` makes no heading. Markdown saved
        // under a text file's name is told by its markup and read as such.
        SourceKind::Text => {
            let text = fluree_doc_text::decode(data);
            let elements = if fluree_doc_text::is_markdown(&text) {
                fluree_doc_markdown::parse(&text)
                    .map_err(|e| format!("Markdown document too large: {}", common::refused(&e)))?
            } else {
                fluree_doc_text::parse(&text)
                    .map_err(|e| format!("text document too large: {}", common::refused(&e)))?
            };
            (elements, fluree_doc_model::Notes::default())
        }
        SourceKind::Html => {
            let text = fluree_doc_html::decode(&data);
            (
                fluree_doc_html::parse(&text)
                    .map_err(|e| format!("HTML document too large: {}", common::refused(&e)))?,
                declared(fluree_doc_html::info(&text)),
            )
        }
        SourceKind::Docx => {
            let options = fluree_doc_docx::Options {
                comments: args.comments,
            };
            let mut pkg = fluree_doc_docx::Package::open(&data)
                .map_err(|e| fluree_doc_docx::DocxError::Zip(e).to_string())?;
            // The document's budget: what its pictures' readings make is
            // spent from what the document left of it.
            let mut budget = fluree_doc_model::budget::Budget::default();
            let (mut elements, info, media) =
                fluree_doc_docx::parse_package_within(&mut pkg, &options, &mut budget)
                    .map_err(|e| common::refused(&e))?;
            // The images placed in the flow: a pasted screenshot's text
            // follows its figure, read by OCR. Their bytes are only read
            // when something is there to read them, from the package opened
            // once; each image's first bytes alone tell its format, without
            // decompressing the rest.
            let pkg = std::cell::RefCell::new(pkg);
            let media = read_media_with(
                src.stem,
                media,
                &mut |part| fluree_doc_docx::read_media_head(&mut pkg.borrow_mut(), part),
                &mut |part| fluree_doc_docx::read_media(&mut pkg.borrow_mut(), part),
                &mut budget,
                cfg,
                quiet,
            );
            fluree_doc_image::place_after_figures(&mut elements, media.read);
            // The page header and footer are read into the text, marked as
            // what they are; their lines are also the running text a PDF's
            // identity block is kept in, so a graph finds a document's
            // identity in one place whatever it was read from.
            let running_text = elements
                .iter()
                .filter(|e| matches!(e.evidence, "header" | "footer"))
                .flat_map(|e| {
                    fluree_doc_model::doco::projection_text(e)
                        .lines()
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .filter(|l| !l.trim().is_empty())
                .collect();
            (
                elements,
                fluree_doc_model::Notes {
                    running_text,
                    unread_images: media.unread,
                    ocr: media.ocr,
                    ..declared(info)
                },
            )
        }
        SourceKind::Pptx => {
            let (elements, info) =
                fluree_doc_pptx::parse_with_info(&data).map_err(|e| common::refused(&e))?;
            (elements, declared(info))
        }
        // A workbook: each sheet is a page, its islands of cells are tables.
        // A picture anchored to its cells is read as a DOCX's is, its text
        // placed after its figure.
        SourceKind::Xlsx => {
            let mut pkg = fluree_doc_xlsx::Package::open(&data)
                .map_err(|e| fluree_doc_xlsx::XlsxError::Zip(e).to_string())?;
            let mut budget = fluree_doc_model::budget::Budget::default();
            let (mut elements, info, media) =
                fluree_doc_xlsx::parse_package_within(&mut pkg, &mut budget)
                    .map_err(|e| common::refused(&e))?;
            // Read from the package opened once, as a DOCX's are.
            let pkg = std::cell::RefCell::new(pkg);
            let media = read_media_with(
                src.stem,
                media,
                &mut |part| fluree_doc_xlsx::read_media_head(&mut pkg.borrow_mut(), part),
                &mut |part| fluree_doc_xlsx::read_media(&mut pkg.borrow_mut(), part),
                &mut budget,
                cfg,
                quiet,
            );
            fluree_doc_image::place_after_figures(&mut elements, media.read);
            (
                elements,
                fluree_doc_model::Notes {
                    unread_images: media.unread,
                    ocr: media.ocr,
                    ..declared(info)
                },
            )
        }
        // A delimited file: one table under its header row. A `.tsv` is
        // tab-separated; a `.csv`'s delimiter is sniffed.
        SourceKind::Csv => {
            let tsv = path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("tsv"));
            let elements = fluree_doc_csv::parse_with(&data, tsv.then_some('\t'))
                .map_err(|e| common::refused(&e))?;
            (elements, fluree_doc_model::Notes::default())
        }
        // An OpenDocument text, spreadsheet or presentation, by its
        // `mimetype`.
        SourceKind::Odf => {
            let (elements, info, _) =
                fluree_doc_odf::parse_with_info(&data).map_err(|e| common::refused(&e))?;
            (elements, declared(info))
        }
        // An Excel binary workbook, laid out as an XLSX one is.
        SourceKind::Xlsb => {
            let (elements, info) =
                fluree_doc_xls::parse_xlsb(&data).map_err(|e| common::refused(&e))?;
            (elements, declared(info))
        }
        // A Rich Text Format document, mapped as a Word file is.
        SourceKind::Rtf => {
            let (elements, info) =
                fluree_doc_rtf::parse_with_info(&data).map_err(|e| common::refused(&e))?;
            (elements, declared(info))
        }
        // A PowerPoint 97-2003 deck, a slide a page. A picture on a slide is
        // read as a DOCX's is, its text placed after its figure.
        SourceKind::Ppt => {
            let mut budget = fluree_doc_model::budget::Budget::default();
            let (mut elements, media) = fluree_doc_ppt::parse_with_media_within(&data, &mut budget)
                .map_err(|e| common::refused(&e))?;
            // The deck opened once, when the first picture is read. Each
            // picture's first bytes are copied alone to tell its format:
            // the entries of a deck's store may all point at one large
            // picture, and each is looked at.
            let deck = std::cell::OnceCell::new();
            let opened = || {
                deck.get_or_init(|| {
                    fluree_doc_ppt::MediaReader::open(&data).map_err(|e| e.to_string())
                })
                .as_ref()
                .map_err(Clone::clone)
            };
            let media = read_media_with(
                src.stem,
                media,
                &mut |part| opened()?.head(part, fluree_doc_pdf::image::Format::SNIFFED),
                &mut |part| opened()?.read(part),
                &mut budget,
                cfg,
                quiet,
            );
            fluree_doc_image::place_after_figures(&mut elements, media.read);
            (
                elements,
                fluree_doc_model::Notes {
                    unread_images: media.unread,
                    ocr: media.ocr,
                    ..Default::default()
                },
            )
        }
        // A Word 97-2003 document's text, through its piece table, and
        // what its summary information declares. What it holds and this
        // reader leaves out (headers and footers, or text boxes, whose list
        // of stories does not read) is said, rather than lost without a
        // word.
        SourceKind::Doc => {
            let read = fluree_doc_doc::parse_with_info(&data).map_err(|e| common::refused(&e))?;
            if !read.left_out.is_empty() && !quiet {
                eprintln!(
                    "warning: {}: not read: {}",
                    src.stem,
                    read.left_out.join(", ")
                );
            }
            (read.elements, declared(read.info))
        }
        // An Excel 97-2003 workbook, laid out as an XLSX one is.
        SourceKind::Xls => {
            let (elements, info) =
                fluree_doc_xls::parse_with_info(&data).map_err(|e| common::refused(&e))?;
            (elements, declared(info))
        }
        _ => return convert_bytes(data, src, cfg, args, pages, quiet),
    };
    // The comments an OpenDocument, RTF or Word 97-2003 document carries,
    // read with it, are left out unless asked for (`--comments`), as a
    // DOCX's are. The ids around them are kept, so an element is named
    // alike with and without them.
    let mut elements = elements;
    if !args.comments {
        elements.retain(|e| e.evidence != "comment");
    }
    Ok(render(&elements, src, args, Vec::new(), &notes))
}

/// A meeting transcript or a caption file: one paragraph per speaker turn.
///
/// A transcript with no cues converts to nothing, and says so, because that
/// output is otherwise indistinguishable from a file that was not read.
fn convert_transcript(
    data: &[u8],
    src: &Source,
    args: &ConvertArgs,
    quiet: bool,
) -> Result<String, String> {
    let stem = src.stem;
    let elements = fluree_doc_transcript::parse(data).map_err(|e| common::refused(&e))?;
    if elements.is_empty() && !quiet {
        eprintln!("note: {stem}: the transcript holds no cues, so the output is empty");
    }
    Ok(render(
        &elements,
        src,
        args,
        Vec::new(),
        &fluree_doc_model::Notes::default(),
    ))
}

/// An email: each message of its thread, and its attachments described.
///
/// An attachment is a document of its own, so it is not in this output.
/// `--attachments DIR` saves the files for converting on their own; without
/// it, the note on stderr says what was left out. An image the body shows
/// is also drawn where it is shown, and its text, read by OCR, placed there.
fn convert_email(
    data: &[u8],
    src: &Source,
    cfg: &TierConfig,
    args: &ConvertArgs,
    quiet: bool,
) -> Result<String, String> {
    let stem = src.stem;
    let mut email = fluree_doc_email::parse(data).map_err(|e| common::refused(&e))?;
    // The images the body shows: a pasted screenshot's text follows its
    // figure, read as a DOCX picture's is, within what the message left of
    // its budget.
    let mut budget = std::mem::take(&mut email.budget);
    let media = read_media(
        stem,
        email.media.clone(),
        &|part| {
            email
                .media_bytes(part)
                .ok_or_else(|| format!("{part}: not in the message"))
        },
        &mut budget,
        cfg,
        quiet,
    );
    // An image the body shows, read into the body where it is shown, is
    // converted with the message; any other file is a document of its own,
    // and this output does not hold it. So is an image read with no text in
    // it: the body holds nothing of it.
    let read: std::collections::HashSet<usize> = email
        .media
        .iter()
        .filter(|m| (media.read.iter()).any(|(figure, els)| *figure == m.figure && !els.is_empty()))
        .filter_map(|m| email.media_attachment(m.source.part()?))
        .collect();
    let files: Vec<(&fluree_doc_model::Attachment, &[u8])> = email
        .attachments
        .iter()
        .map(|a| (&a.info, a.bytes.as_slice()))
        .collect();
    let read: Vec<usize> = read.into_iter().collect();
    save_attachments(&files, src, args, &read, "attachment", quiet)?;
    if args.parse_attachments {
        convert_attachments(&email, src, cfg, args, quiet)?;
    }
    let notes = fluree_doc_model::Notes {
        unread_images: media.unread,
        ocr: media.ocr,
        ..email.notes()
    };
    let mut elements = email.elements;
    fluree_doc_image::place_after_figures(&mut elements, media.read);
    Ok(render(&elements, src, args, Vec::new(), &notes))
}

/// Save the files a document carries (an e-mail's attachments, a PDF's
/// embedded files, `what` names them) under `--attachments DIR`, in the
/// input's own directory; without it, say on stderr what was left out. The
/// files at `read` (images read into the body) were converted with it and
/// are not counted as left out. A carried file's files are not saved: an
/// attached message's are converted by the run that carries it, and of any
/// other's, what is left out is said without asking for a directory the run
/// was given.
fn save_attachments(
    files: &[(&fluree_doc_model::Attachment, &[u8])],
    src: &Source,
    args: &ConvertArgs,
    read: &[usize],
    what: &str,
    quiet: bool,
) -> Result<(), String> {
    let stem = src.stem;
    let count = files.len();
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    match &args.attachments {
        Some(dir) if count > 0 => {
            // The slot, not the stem: two `mail.eml` in one batch would
            // otherwise save their attachments over each other's.
            let dir = dir.join(&src.slot);
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
            let mut taken = std::collections::HashSet::new();
            for (i, (info, bytes)) in files.iter().enumerate() {
                let path = dir.join(file_name(info.filename.as_deref(), i, &mut taken));
                std::fs::write(&path, bytes)
                    .map_err(|e| format!("writing {}: {e}", path.display()))?;
            }
            if !quiet {
                eprintln!(
                    "note: {stem}: {count} {what}{} saved to {}",
                    plural(count),
                    dir.display()
                );
            }
        }
        None if src.carried && what == "attachment" => {}
        None if count > 0 && !quiet => {
            let names: Vec<String> = files
                .iter()
                .enumerate()
                .filter(|(i, _)| !read.contains(i))
                .map(|(_, (info, _))| {
                    info.filename
                        .clone()
                        .unwrap_or_else(|| info.content_type.clone())
                })
                .collect();
            let left = names.len();
            let images = if read.is_empty() {
                String::new()
            } else {
                format!(
                    "; {} inline image{} read into the body",
                    read.len(),
                    plural(read.len())
                )
            };
            let hint = if src.carried {
                "; the files a carried file holds are not saved"
            } else {
                "; pass --attachments DIR to save them"
            };
            if left > 0 {
                eprintln!(
                    "note: {stem}: {left} {what}{} not converted ({}){images}{hint}",
                    plural(left),
                    names.join(", ")
                );
            }
        }
        _ => {}
    }
    Ok(())
}

/// Convert the files an e-mail carries, down through attached messages
/// (`fluree_doc_email::parse_attachments`, its default bounds), each into
/// `DIR/<slot>/<sha256>.<format>` beside the saved files: one output per
/// file, by its bytes, however often and under whatever names it was sent.
///
/// Each output is written as it is made, so no more than one is held at a
/// time, and each file is converted within what the message itself leaves
/// of a document's budget ([`attachments_share`]): the message is held
/// while its files are read.
///
/// A file is converted as `convert` would convert it on its own, by its
/// content and the extension of its name, but as a document of its own: the
/// flags naming the e-mail's document (`--doc-iri`, `--base-iri`,
/// `--source-name`) are not its, and it saves no files of its own (the
/// attachments of a message attached are followed here). An image the body
/// shows is converted with the body, not again; another image only when
/// something is configured to read it.
fn convert_attachments(
    email: &fluree_doc_email::Email,
    src: &Source,
    cfg: &TierConfig,
    args: &ConvertArgs,
    quiet: bool,
) -> Result<(), String> {
    use fluree_doc_email::Outcome;
    let Some(dir) = args.attachments.as_ref().map(|d| d.join(&src.slot)) else {
        return Ok(());
    };
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let child_args = ConvertArgs {
        doc_iri: None,
        base_iri: None,
        source_name: None,
        attachments: None,
        parse_attachments: false,
        ..args.clone()
    };
    // A conversion that cannot be written stops the run, as the message's
    // own output would.
    let mut unwritten: Option<String> = None;
    let mut read = |f: &fluree_doc_email::AttachedFile, _depth: usize| {
        if unwritten.is_some() {
            return Some(Err(
                "not converted: an earlier output was not written".to_string()
            ));
        }
        let out = convert_carried(f, cfg, &child_args, None, quiet)?;
        Some(out.and_then(|out| {
            let path = dir.join(format!("{}.{}", f.info.sha256, ext(args.format)));
            std::fs::write(&path, out).map_err(|e| {
                let e = format!("writing {}: {e}", path.display());
                unwritten = Some(e.clone());
                e
            })
        }))
    };
    let children = walk_carried(email, &mut read);
    if let Some(e) = unwritten {
        return Err(e);
    }
    let mut converted = 0usize;
    let mut left: Vec<String> = Vec::new();
    for c in &children {
        let name = c
            .info
            .filename
            .clone()
            .unwrap_or_else(|| c.info.content_type.clone());
        match &c.outcome {
            Outcome::Read(()) => converted += 1,
            Outcome::NoReader => left.push(format!("{name}: no reader")),
            Outcome::Failed(e) => left.push(format!("{name}: {e}")),
            Outcome::Skipped(why) => left.push(format!("{name}: {why}")),
        }
    }
    if !quiet && !children.is_empty() {
        let tail = if left.is_empty() {
            String::new()
        } else {
            format!("; not converted: {}", left.join("; "))
        };
        eprintln!(
            "note: {}: {converted} of {} attached file(s) converted to {}{tail}",
            src.stem,
            children.len(),
            dir.display()
        );
    }
    Ok(())
}

/// Walk the files `email` carries with `read`, as `--parse-attachments`
/// does (`fluree_doc_email::parse_attachments`, its default bounds),
/// within what the message leaves of the budget ([`attachments_share`]):
/// each file, and each attached message the walk opens to follow its
/// files, is read within that share, not on a budget of its own beside
/// the message it came in.
pub(crate) fn walk_carried<T>(
    email: &fluree_doc_email::Email,
    read: &mut fluree_doc_email::ReadAttachment<'_, T>,
) -> Vec<fluree_doc_email::Child<T>> {
    fluree_doc_model::budget::within(attachments_share(email), || {
        fluree_doc_email::parse_attachments(email, &Default::default(), read)
    })
}

/// The memory each file an e-mail carries may be converted within: a
/// document's budget less what the message holds while they are read, its
/// elements and their text at their costs, and its files' bytes, each held
/// by the message and by the walk over them, at a part's two bytes a byte.
/// Nothing where the message alone spends the whole budget.
fn attachments_share(email: &fluree_doc_email::Email) -> u64 {
    let mut held = fluree_doc_model::budget::Budget::default();
    let spent = (|| {
        held.spend_elements(email.elements.len() as u64)?;
        for e in &email.elements {
            held.spend_text(&e.text)?;
        }
        let files: usize = email.attachments.iter().map(|a| a.bytes.len()).sum();
        held.spend_bytes(files as u64)
    })();
    match spent {
        Ok(()) => held.memory_left(),
        Err(_) => 0,
    }
}

/// Convert a file an e-mail carries, as `--parse-attachments` does (within
/// what [`walk_carried`] leaves it), with
/// `args` for a child (no attachments of its own saved), its pages `pages`;
/// `None` where no reader takes it: a format with none, or an image no
/// tier is configured to read (a signature's logo, drawn in the body, never).
pub(crate) fn convert_carried(
    f: &fluree_doc_email::AttachedFile,
    cfg: &TierConfig,
    args: &ConvertArgs,
    pages: Option<&[usize]>,
    quiet: bool,
) -> Option<Result<String, String>> {
    let readable_images =
        cfg.ocr.is_some() || cfg.ocr_words.is_some() || cfg.tier_results.is_some() || cfg.escalate;
    let name = f
        .info
        .filename
        .clone()
        .unwrap_or_else(|| f.info.sha256.clone());
    let path = Path::new(&name);
    match common::input_kind(path, &f.bytes) {
        common::SourceKind::Unsupported => return None,
        common::SourceKind::Image if f.info.inline || !readable_images => return None,
        _ => {}
    }
    let child = Source {
        stem: common::stem_of(path),
        slot: f.info.sha256.clone(),
        named: true,
        name: Some(name.clone()),
        sha256: f.info.sha256.clone(),
        carried: true,
    };
    Some(convert_source(
        path,
        f.bytes.clone(),
        &child,
        cfg,
        args,
        pages,
        quiet,
    ))
}

/// A safe, unique file name for an attachment: its own name with any path
/// taken off, or a numbered stand-in, and unique within its email.
///
/// A name is the sender's bytes: a control character (a NUL, which no file
/// system takes, a line break) is written `_`, and a name is cut to
/// [`MAX_FILE_NAME_BYTES`], its extension kept, so that one file's name
/// cannot fail the whole input when it is saved.
///
/// A name of the form `--parse-attachments` writes a conversion under
/// (`<sha256>.<format>`, beside the saved files) is the sender's to choose,
/// and would pass a file of theirs off as another's conversion: it is saved
/// with a `_` before it, which no conversion's name has.
fn file_name(
    name: Option<&str>,
    index: usize,
    taken: &mut std::collections::HashSet<String>,
) -> String {
    let base = name
        .and_then(|n| n.rsplit(['/', '\\']).next())
        .map(|n| {
            let n: String = n
                .chars()
                .map(|c| if c.is_control() { '_' } else { c })
                .collect();
            shortened(n.trim().trim_start_matches('.'))
        })
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| format!("attachment-{}", index + 1));
    let base = if names_a_conversion(&base) {
        format!("_{base}")
    } else {
        base
    };
    let mut candidate = base.clone();
    let mut n = 2;
    while !taken.insert(candidate.clone()) {
        candidate = match base.rsplit_once('.') {
            Some((stem, ext)) => format!("{stem} ({n}).{ext}"),
            None => format!("{base} ({n})"),
        };
        n += 1;
    }
    candidate
}

/// The most bytes of an attachment's name kept: most file systems take 255,
/// and a name made unique (`name (2).pdf`) is a few bytes longer.
const MAX_FILE_NAME_BYTES: usize = 200;

/// `name`, at most [`MAX_FILE_NAME_BYTES`] of it, cut on a character and
/// keeping a short extension (`.pdf`).
fn shortened(name: &str) -> String {
    if name.len() <= MAX_FILE_NAME_BYTES {
        return name.to_string();
    }
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e)
        .filter(|e| !e.is_empty() && e.len() <= 16)
        .map_or(String::new(), |e| format!(".{e}"));
    let mut stem = String::new();
    for c in name.chars() {
        if stem.len() + c.len_utf8() + ext.len() > MAX_FILE_NAME_BYTES {
            break;
        }
        stem.push(c);
    }
    stem + &ext
}

/// Whether `name` is one a conversion's output could take: 64 hexadecimal
/// digits, in either case (a file system may not tell them apart), alone or
/// before a `.`.
fn names_a_conversion(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() >= 64
        && b[..64].iter().all(u8::is_ascii_hexdigit)
        && matches!(b.get(64), None | Some(b'.'))
}

/// A bare image: one page of pixels, read by an OCR engine or a model.
///
/// With an OCR engine (`--ocr`), or the words one wrote (`--ocr-words`,
/// `<stem>_p0_full.json` or `<stem>.json`), its words are laid out like a
/// scanned page's. Otherwise, or where OCR read nothing, only the deep reader
/// can read it.
///
/// Every other source has a deterministic reading to fall back on. This one
/// has none, so with no reader configured the honest output is nothing — and
/// saying so matters more here than anywhere else, because an empty document
/// is otherwise indistinguishable from a blank image.
fn convert_image(
    data: Vec<u8>,
    src: &Source,
    cfg: &TierConfig,
    args: &ConvertArgs,
    quiet: bool,
) -> Result<String, String> {
    let stem = src.stem;
    let format = fluree_doc_pdf::image::Format::sniff(&data).ok_or("not a recognised image")?;
    // Upright: a phone photo's page is the size the reader sees it at, as
    // OCR's is. Only the header is read.
    let header = fluree_doc_image::header(&data).ok();
    let pages = header.map_or(1, |h| h.pages);
    if pages > 1 && !quiet {
        eprintln!("warning: {stem}: only the first of its {pages} pages is read");
    }
    // A model may read what OCR leaves unread, as on a scanned PDF page.
    let model_after = cfg.escalate && cfg.tier_results.is_none();
    // What OCR made of it when it took no word: kept should the model fail,
    // and its reading, empty, listed should the model read nothing either.
    let mut ocr_read = None;
    if cfg.ocr.is_some() || cfg.ocr_words.is_some() {
        match ocr_image(&data, header, src, cfg, quiet) {
            // Whatever OCR's confidence in the words it took: it is reported
            // (`doc:ocrReadings`), not a reason to send the image to a model.
            // Whether a model reads an image OCR read is the run's to say.
            Ok(read) if read.read_something || !model_after => {
                return Ok(render_image_ocr(read, src, args, quiet));
            }
            Ok(read) => {
                if !quiet {
                    eprintln!("note: {stem}: OCR read nothing; the configured model reads it");
                }
                ocr_read = Some(read);
            }
            Err(e) if model_after => {
                if !quiet {
                    eprintln!("warning: {stem}: {e}; the configured model reads it");
                }
            }
            Err(e) => return Err(e),
        }
    }
    let (w, h) = header
        .map(|h| (h.width, h.height))
        .or_else(|| fluree_doc_pdf::image::dimensions(&data))
        .filter(|&(w, h)| w > 0 && h > 0)
        .ok_or_else(|| format!("{} header declares no usable size", format.mime()))?;
    let mut doc = fluree_doc_pdf::image::page_of(w, h, false);
    let sizes: Vec<fluree_doc_model::PageSize> = doc
        .pages
        .iter()
        .map(|p| fluree_doc_model::PageSize {
            index: p.index,
            width: p.width,
            height: p.height,
            folio: None,
        })
        .collect();
    let mut elements: Vec<Element> = Vec::new();
    let page = "p0_full";
    let readings = match image_readings(stem, page, &data, format.mime(), cfg, cfg.escalate) {
        Ok(readings) => readings,
        // OCR read it and took no word: the model failing leaves that
        // reading, as it leaves a placed picture's, not the conversion
        // failed. Where OCR had no reading of it (no words written for it),
        // the model was its only reader, and its failure the conversion's.
        Err(e) => match ocr_read {
            Some(read) if read.readings > 0 => {
                if !quiet {
                    eprintln!("warning: {stem}: the configured model could not read it: {e}");
                }
                return Ok(render_image_ocr(read, src, args, quiet));
            }
            _ => return Err(e),
        },
    };
    if let Some(readings) = readings {
        // One synthetic element for the splice to replace, so the image
        // travels the same page-tier path a scanned PDF page does.
        elements.push(Element {
            bbox: Some(doc.pages[0].images[0].bbox),
            ..Element::new("doco:Paragraph", String::new(), "rust", "layout")
        });
        // No line of OCR's to check the model's reading against: the model
        // reads an image OCR took no word in.
        splice_image_reading(&mut elements, stem, page, readings.as_ref(), &[]);
        // The tables of the reading, their copies spent as a PDF's are.
        fluree_doc_pdf::spend_table_copies(
            &mut doc,
            &mut elements,
            &mut fluree_doc_pdf::PageBudget::default(),
        )
        .map_err(|e| format!("image too large: {}", common::refused(&e)))?;
    }
    // An image nothing read is listed unread, so the output says so too. A
    // reading of the whole image read it, however short.
    let mut unread = if elements.is_empty() {
        fluree_doc_pdf::unread_pages(&doc, &elements)
    } else {
        Vec::new()
    };
    unread.extend(later_pages(pages));
    let mut notes = fluree_doc_model::Notes {
        unread,
        ..Default::default()
    };
    // The model read nothing either: OCR's reading, which took no word, is
    // still the image's, as an empty reading of a PDF page is.
    if let Some(read) = ocr_read.filter(|_| elements.is_empty()) {
        warn_listed_scores(read.scores, &read.notes.ocr, quiet);
        notes.ocr = read.notes.ocr;
    }
    // Warned about as a PDF's page nothing read is, with what could read it.
    if !quiet {
        if let Some(note) = notes.summary() {
            eprintln!("warning: {stem}: {note}");
        }
        if elements.is_empty() {
            eprintln!(
                "note: {stem}: {}",
                if cfg.tier_results.is_some() {
                    "--tier-results holds no reading of it (`<stem>_p0_full.json`)"
                } else if cfg.escalate {
                    "the configured model returned nothing for it"
                } else {
                    "read it with --ocr, --ocr-words, --tier-results or a configured model \
                     (`fdoc config gemini --credentials <key.json>`)"
                }
            );
        }
    }
    past_limits(stem, &notes, args.format, quiet);
    Ok(render(&elements, src, args, sizes, &notes))
}

/// The model readings of an image read whole, as a page: a reading someone
/// already has (`--tier-results`, `<stem>_<crop>.json`), else, where `live`
/// allows it, the configured reader, as for a scanned PDF page. `None` where
/// neither has one to give. The image's bytes are sent only to a live
/// reader.
fn image_readings(
    stem: &str,
    crop: &str,
    image: &[u8],
    mime: &str,
    cfg: &TierConfig,
    live: bool,
) -> Result<Option<Box<dyn fluree_doc_pdf::arbiter::TierBackend>>, String> {
    if let Some(dir) = cfg.tier_results.as_deref() {
        return Ok(Some(Box::new(
            fluree_doc_pdf::arbiter::FixtureBackend::new(dir.to_path_buf()),
        )));
    }
    if !live {
        return Ok(None);
    }
    let readings = crate::escalate::read_image(
        std::path::Path::new(stem),
        crop,
        image,
        mime,
        &cfg.config,
        cfg.verbose,
    )?;
    Ok((!readings.is_empty()).then(|| Box::new(readings) as Box<_>))
}

/// Splice the reading of an image read whole (`crop`: `p0_full` for a bare
/// image, `m<n>` for a placed one) over `elements`, what was read of it
/// before (a synthetic page element, or what OCR read), as a scanned PDF
/// page's reading replaces the page: unless it lost most of `lines`, the
/// words already read. Returns whether the reading was taken. A placed
/// image's headings are the screen's, not the document's: read as
/// paragraphs ([`common::screen_blocks`]), as OCR's are; a bare image's are
/// its own.
fn splice_image_reading(
    elements: &mut Vec<Element>,
    stem: &str,
    crop: &str,
    readings: &dyn fluree_doc_pdf::arbiter::TierBackend,
    lines: &[String],
) -> bool {
    /// A reading of `crop`, served as the whole page it stands for.
    struct AsPage<'a> {
        inner: &'a dyn fluree_doc_pdf::arbiter::TierBackend,
        crop: &'a str,
    }
    impl fluree_doc_pdf::arbiter::TierBackend for AsPage<'_> {
        fn read(&self, stem: &str, crop: &str) -> Option<Vec<fluree_doc_pdf::arbiter::Block>> {
            let blocks = (crop == "p0_full")
                .then(|| self.inner.read(stem, self.crop))
                .flatten()?;
            Some(if self.crop == "p0_full" {
                blocks
            } else {
                common::screen_blocks(blocks)
            })
        }
    }
    let page = AsPage {
        inner: readings,
        crop,
    };
    fluree_doc_pdf::arbiter::splice_with_page(elements, stem, &page, None, &[lines.to_vec()]);
    elements.retain(|e| !e.text.trim().is_empty());
    elements.iter().any(|e| e.evidence == "page-tier")
}

/// What came of the images a document places in its flow.
#[derive(Default)]
struct MediaRead {
    /// The text read in each image, once after each figure drawing it.
    read: Vec<(String, Vec<Element>)>,
    /// The images nothing read, and why, once for each figure drawing them.
    unread: Vec<fluree_doc_model::UnreadImage>,
    /// The OCR readings whose words are in the output, by image.
    ocr: Vec<fluree_doc_model::OcrReading>,
}

/// Read the images a document places in its flow, each once however many
/// figures draw it ([`fluree_doc_model::read_pictures`]), within what one
/// document may have read ([`fluree_doc_model::Allowance::DOCUMENT`]), each
/// by the rules a bare image is read by: OCR, with the engine or from the
/// words one wrote (`<stem>_m<n>.json` under `--ocr-words`, named by the
/// first figure drawing it); a model's reading someone already has
/// (`<stem>_m<n>.json` under `--tier-results`), which replaces what OCR read
/// as it replaces a scanned page's; else the configured model, for an image
/// OCR did not read any text in that a call is worth ([`declined_by_model`]:
/// its header read, over a floor of pixels, taken for text by the pixel
/// probe), within the document's `escalation.max_crops`: the images it
/// reads are sent together, through one reader, as a document's crops are.
/// The probe gates the model's call alone, as it is billed and sends the
/// image off the machine: what OCR reads and a reading supplied are taken
/// whatever it makes of the image. What was read is placed after each
/// figure drawing the image. An image nothing read is listed, with why
/// ([`fluree_doc_model::UnreadImage::reason`]), once for each figure drawing
/// it, for the output to say so, and counted in one note, unless no engine
/// decodes its format, it is under the floor a model is sent or it is not
/// taken for text (`UnsupportedFormat`, `Small`, `NotTexty`): the output's
/// silence is not mistaken for an empty image. `bytes` reads one image's
/// bytes by the part the file holds it under; it is called only when
/// something is there to read them, one image at a time, and again for
/// each image the model is sent, as it is sent: no image's bytes are held
/// past its reading. `head` reads its first bytes, which tell its format
/// before anything else is read.
///
/// What each reading makes is spent from `budget`, what the document left
/// of its own ([`fluree_doc_image::spend_reading`]): OCR's and one supplied
/// as each image is read, in the order they are first drawn, the
/// configured model's once it has read those sent to it. An image whose
/// reading is past what is left is listed unread past the limit
/// (`PastMemoryLimit`), and the model is not sent it; the document and the
/// images read before it are kept.
fn read_media_with(
    stem: &str,
    media: Vec<fluree_doc_model::Media>,
    head: &mut dyn FnMut(&str) -> Result<Vec<u8>, String>,
    bytes: &mut dyn FnMut(&str) -> Result<Vec<u8>, String>,
    budget: &mut fluree_doc_model::budget::Budget,
    cfg: &TierConfig,
    quiet: bool,
) -> MediaRead {
    use fluree_doc_model::{Picture, Unread};
    // Each picture's place among the document's, by the name it goes by:
    // the order its first figure is drawn in.
    let mut order: HashMap<String, usize> = HashMap::new();
    for (i, m) in media.iter().enumerate() {
        order.entry(m.tag()).or_insert(i);
    }
    let total = fluree_doc_model::pictures(media.clone()).len();
    let mut reader = MediaReader {
        stem,
        cfg,
        head,
        given: cfg
            .tier_results
            .as_deref()
            .map(|dir| fluree_doc_pdf::arbiter::FixtureBackend::new(dir.to_path_buf())),
        asked: 0,
        over_budget: 0,
        verdicts: HashMap::new(),
        budget: &mut *budget,
        quiet,
    };
    let reads = fluree_doc_model::read_pictures(
        media,
        bytes,
        &mut reader,
        fluree_doc_model::Allowance::DOCUMENT,
    );
    // Each picture as read so far, in the order it is first drawn, settled
    // once the model has read those sent to it.
    let mut placed: Vec<(Picture, Placed)> = reads.read;
    let mut past = 0usize;
    for (picture, why) in reads.unread {
        let tag = picture.tag();
        let p = match why {
            // Told before its bytes were read: its format, or that nothing
            // was there to read it.
            Unread::NotAsked => reader
                .verdicts
                .remove(&tag)
                .unwrap_or_else(|| Placed::new(tag, None)),
            // An image the file does not hold (a link to an outside file, a
            // relationship it does not define) has nothing to read.
            Unread::NotInPackage => {
                if !quiet {
                    eprintln!("warning: {stem}_{tag}: {}", picture.source);
                }
                Placed::new(tag, Some(("Missing", String::new())))
            }
            Unread::PastAllowance => {
                past += 1;
                Placed::new(tag, Some(("PastAllowance", String::new())))
            }
            // Its bytes could not be read (a part that is not there, one
            // past the cap).
            Unread::Failed(e) => {
                if !quiet {
                    eprintln!("warning: {stem}_{tag}: {e}");
                }
                Placed::new(tag, Some(("Failed", e)))
            }
        };
        placed.push((picture, p));
    }
    placed.sort_by_key(|(picture, _)| order.get(&picture.tag()).copied());
    let over_budget = reader.over_budget;
    read_by_model(stem, &mut placed, bytes, budget, cfg, quiet);
    let mut out = MediaRead::default();
    // Images whose reading was past what the document had left.
    let mut past_memory = 0usize;
    for (picture, p) in placed {
        // Of a reading listed only, taken or set aside as noise: one refused,
        // or one a model's replaced, says nothing in the output.
        if let (Some((n, words)), false) = (p.scores, p.ocr_read.is_empty()) {
            let name = format!("{stem}_{}.json", p.tag);
            common::warn_scores_out_of_range(&name, n, words, quiet);
        }
        // Taken, or set aside as noise and listed as one, with its words'
        // confidence.
        out.ocr.extend(p.ocr_read);
        match p.read {
            Some(elements) => out.read.extend(picture.after_each_figure(elements)),
            None => {
                let reason = match &p.failure {
                    Some((reason, _)) => reason,
                    None if p.unsupported => "UnsupportedFormat",
                    None => "NotRead",
                };
                if fluree_doc_model::is_past_limit(reason) {
                    past_memory += 1;
                }
                for figure in picture.figures() {
                    out.unread.push(fluree_doc_model::UnreadImage {
                        figure: figure.to_string(),
                        image: p.tag.clone(),
                        part: picture.source.name().to_string(),
                        reason: reason.into(),
                    });
                }
            }
        }
    }
    if !quiet {
        let max = cfg.config.escalation.max_crops;
        if over_budget > 0 {
            eprintln!(
                "note: {stem}: {over_budget} image(s) not sent to the model, past `escalation.max_crops` ({max})"
            );
        }
        if past > 0 {
            let a = fluree_doc_model::Allowance::DOCUMENT;
            eprintln!(
                "note: {stem}: {past} image(s) past what one document may have read \
                 ({} images, {} million pixels)",
                a.pictures,
                a.pixels / 1_000_000
            );
        }
        if past_memory > 0 {
            eprintln!(
                "note: {stem}: {past_memory} image(s) past the memory one document may take, \
                 not read (raise it with --max-memory)"
            );
        }
    }
    // An image no engine decodes (a vector drawing, a GIF or BMP logo) is
    // listed, not counted: nearly every signed e-mail carries such a logo,
    // and a note on each message would be noise. So is one under the floor
    // the model is sent (`Small`), as a PDF's region too small for a model
    // is: a PNG logo is as common as a GIF one, and was left out by choice,
    // not by a failure. `--verbose` names each, with `--ocr` to read it. So
    // is one the pixel probe does not take for text, so no model was sent
    // it, or whose OCR reading was noise (`NotTexty`), as a PDF's region the
    // probe does not take for text is: a photograph is what it is as often
    // as not. Counted by image, however many figures draw it.
    let mut unread: Vec<(&str, &str)> = out
        .unread
        .iter()
        .map(|u| (u.image.as_str(), u.reason.as_str()))
        .collect();
    unread.dedup();
    let quiet_unread = unread
        .iter()
        .filter(|(_, reason)| matches!(*reason, "UnsupportedFormat" | "Small" | "NotTexty"))
        .count();
    if unread.len() > quiet_unread && !quiet {
        let ocr = cfg.ocr.is_some() || cfg.ocr_words.is_some();
        let hint = if !ocr && cfg.tier_results.is_none() && !cfg.escalate {
            " (read them with --ocr, --ocr-words, --tier-results or a configured model)"
        } else {
            ""
        };
        eprintln!(
            "note: {stem}: {} of {} image(s) in the document were not read{hint}",
            unread.len() - quiet_unread,
            total - quiet_unread,
        );
    }
    out
}

/// What reads a document's placed images for [`read_media_with`], one
/// picture at a time.
struct MediaReader<'a> {
    stem: &'a str,
    cfg: &'a TierConfig,
    /// Reads an image's first bytes, by its part.
    head: &'a mut dyn FnMut(&str) -> Result<Vec<u8>, String>,
    /// The readings someone already has (`--tier-results`).
    given: Option<fluree_doc_pdf::arbiter::FixtureBackend>,
    /// Images sent to the configured model so far.
    asked: usize,
    /// Images not sent to it, past `escalation.max_crops`.
    over_budget: usize,
    /// What became of each picture told apart before its bytes were read,
    /// by the name it goes by.
    verdicts: HashMap<String, Placed>,
    /// What the document left of its budget, each reading spent from it.
    budget: &'a mut fluree_doc_model::budget::Budget,
    quiet: bool,
}

impl fluree_doc_model::PictureReader for MediaReader<'_> {
    type Reading = Placed;

    fn ready(&mut self, picture: &fluree_doc_model::Picture) -> bool {
        let tag = picture.tag();
        let name = format!("{}_{tag}", self.stem);
        // An image no engine decodes and no model is sent (an EMF or WMF
        // drawing, a GIF or BMP logo), told by its first bytes before
        // anything else, whatever reads the others: listed as what it is,
        // and nothing more of it is read. A part that cannot be read, or
        // that ends before its format could be told (empty, or cut short),
        // is no format at all: it failed, and is counted, whether or not
        // anything was there to read it.
        let failed = |e: String, quiet: bool| {
            if !quiet {
                eprintln!("warning: {name}: {e}");
            }
            Placed::new(tag.clone(), Some(("Failed", e)))
        };
        let verdict = match picture.source.part().map(|part| (self.head)(part)) {
            Some(Err(e)) => Some(failed(e, self.quiet)),
            Some(Ok(h)) if fluree_doc_pdf::image::Format::sniff(&h).is_none() => {
                Some(if h.len() < fluree_doc_pdf::image::Format::SNIFFED {
                    let e = format!("its part holds {} byte(s), too few for an image", h.len());
                    failed(e, self.quiet)
                } else {
                    Placed {
                        unsupported: true,
                        ..Placed::new(tag.clone(), None)
                    }
                })
            }
            _ => None,
        };
        if let Some(p) = verdict {
            self.verdicts.insert(tag, p);
            return false;
        }
        let cfg = self.cfg;
        let ocr = cfg.ocr.is_some() || cfg.ocr_words.is_some();
        ocr || cfg.tier_results.is_some() || cfg.escalate
    }

    fn pixels(&mut self, image: &[u8]) -> Result<u64, String> {
        // An image whose size cannot be read is charged a pixel for each of
        // its bytes: nothing tells what decoding it would cost, and a
        // document of many such parts (a PNG's signature, then zeros) is
        // bounded by its bytes, as one of many images is by their pixels.
        // It is not refused here: a reading supplied for it, or OCR's words,
        // may still read it; no model is sent it ([`declined_by_model`]).
        Ok(
            fluree_doc_image::header(image).map_or(image.len() as u64, |h| {
                u64::from(h.width) * u64::from(h.height)
            }),
        )
    }

    fn read(&mut self, picture: &fluree_doc_model::Picture, b: &[u8]) -> Result<Placed, String> {
        let (stem, cfg, quiet) = (self.stem, self.cfg, self.quiet);
        let tag = picture.tag();
        let name = format!("{stem}_{tag}");
        let ocr = cfg.ocr.is_some() || cfg.ocr_words.is_some();
        // Whether the configured model may be asked for it: no reading of it
        // is supplied (`--tier-results`), which would be taken instead.
        let model = cfg.escalate && cfg.tier_results.is_none();
        // Decoded once, for the engine and the pixel probe both, and only
        // for them: words written for it need only its header.
        let mut decoded = cfg.ocr.is_some().then(|| fluree_doc_image::decode(b));
        // What the probe makes of it, judged before the engine takes the
        // decoded image, where a model may be sent it.
        let probed = decoded
            .as_ref()
            .filter(|_| model)
            .map(|d| d.as_ref().ok().map(fluree_doc_image::texty));
        // Why it is not read, should nothing read it.
        let mut failure: Option<(&'static str, String)> = None;
        // What OCR read in it: `None` where OCR did not read it.
        let mut read: Option<Vec<Element>> = None;
        // The OCR reading behind `read`, for `doc:ocrReadings`.
        let mut ocr_read: Vec<fluree_doc_model::OcrReading> = Vec::new();
        // Its words' scores outside 0-1, of how many, where a sidecar gave them.
        let mut scores: Option<(usize, usize)> = None;
        // The budget before OCR's reading is spent from it, should a reading
        // supplied replace OCR's: its tables apart, which are not cloned.
        let before = self
            .given
            .is_some()
            .then(|| self.budget.apart(|b| b.clone()));
        let budget = &mut *self.budget;
        if ocr {
            // The words an engine wrote, where they are what is read.
            let words = match &cfg.ocr {
                Some(_) => Some(None),
                None => cfg
                    .ocr_words
                    .as_deref()
                    .map(|dir| std::fs::read_to_string(dir.join(format!("{name}.json"))).ok()),
            };
            let got = match (&words, &cfg.ocr) {
                // No words were written for it: OCR did not read it.
                (Some(None), None) => None,
                (Some(Some(text)), _) => Some(
                    fluree_doc_vision::WordReading::from_json(text).and_then(|reading| {
                        scores = Some((reading.scores_out_of_range(), reading.words_with_text()));
                        fluree_doc_image::elements_from_words(b, reading, budget)
                    }),
                ),
                // Read by the engine whatever the pixel probe makes of it,
                // as a DOCX's pictures were before the probe judged them:
                // OCR costs nothing per image and sends nothing off the
                // machine, and a reading that is noise is set aside below.
                (_, Some(engine)) => Some(
                    decoded
                        .take()
                        .expect("decoded for the engine")
                        .and_then(|image| {
                            fluree_doc_image::read_texty(image, engine.0.as_ref(), budget)
                        }),
                ),
                _ => unreachable!("words or an engine"),
            };
            match got {
                // What it read is past what the document has left: not read,
                // nothing of it spent, and no model is sent it, whose reading
                // would be past it as well. Counted, with the flag that
                // raises it.
                Some(Ok(fluree_doc_image::ImageElements {
                    past_budget: Some(e),
                    ..
                })) => {
                    if cfg.verbose {
                        eprintln!("ocr: {name}: past the memory limit, not read: {e}");
                    }
                    failure = Some((
                        fluree_doc_model::past_limit_reason(e.limit()),
                        e.to_string(),
                    ));
                }
                // A reading that is noise (a photograph or a chart read for
                // text it does not hold) is set aside: the image is not
                // read, and a model may still be sent it.
                Some(Ok(image)) if image.noise => {
                    if cfg.verbose {
                        eprintln!("ocr: {name}: the reading is noise, set aside");
                    }
                    failure = Some(("NotTexty", String::new()));
                    // Listed all the same, as one set aside, with the words
                    // it held and the engine's confidence in them.
                    ocr_read.extend(image.ocr.into_iter().map(|r| fluree_doc_model::OcrReading {
                        page: None,
                        image: Some(tag.clone()),
                        ..r
                    }));
                }
                // A reading set aside because its words could not be placed
                // on the image (an origin or a scale not the image's) read
                // nothing of it: a failure, warned of and counted, as a PDF
                // page it is refused on stays unread; a model may still be
                // sent it.
                Some(Ok(fluree_doc_image::ImageElements {
                    refused: Some(why), ..
                })) => {
                    if !quiet {
                        eprintln!("warning: {name}: {why}");
                    }
                    failure = Some(("Failed", why.to_string()));
                }
                // Read, whatever it held: a photo with no text was read too.
                Some(Ok(image)) => {
                    if cfg.verbose {
                        eprintln!("ocr: {name}: {} element(s)", image.elements.len());
                    }
                    if image.more_pages && !quiet {
                        eprintln!(
                            "warning: {name}: only the first page of a multi-page image is read"
                        );
                    }
                    // A picture has no page of the document's: it goes by
                    // its name, as its words do.
                    ocr_read.extend(image.ocr.into_iter().map(|r| fluree_doc_model::OcrReading {
                        page: None,
                        image: Some(tag.clone()),
                        ..r
                    }));
                    read = Some(image.elements);
                }
                // A picture that could not be read is the user's to know
                // about, as a PDF page that failed is.
                Some(Err(e)) => {
                    if !quiet {
                        eprintln!("warning: {name}: {e}");
                    }
                    failure = Some(("Failed", e));
                }
                None => {}
            }
        }
        // A model's reading: one someone already has, over what OCR read,
        // whatever the image looks like; else the configured model, for an
        // image OCR read no text in.
        let ocr_read_text = read.as_ref().is_some_and(|r| !r.is_empty());
        let past = (failure.as_ref()).is_some_and(|(why, _)| fluree_doc_model::is_past_limit(why));
        let live = model && !ocr_read_text && !past;
        let mut p = Placed {
            read,
            failure,
            ocr_read,
            scores,
            ..Placed::new(tag, None)
        };
        let max = cfg.config.escalation.max_crops;
        if let Some(given) = &self.given {
            // A reading supplied replaces OCR's, and what OCR's spent is
            // given back before it is spent in its place.
            if p.take(stem, given, cfg.verbose) {
                if let Some(before) = before {
                    self.budget.apart(|b| *b = before);
                }
                p.spend(self.budget);
            }
        } else if live {
            // Held to what a call is worth (see `declined_by_model`): its
            // header read, a floor of pixels, the pixel probe. OCR, which
            // costs nothing per image, has read it already, whatever it is.
            let probe = || probed.unwrap_or_else(|| fluree_doc_image::looks_like_text(b));
            if let Some((why, e)) = declined_by_model(b, probe) {
                if cfg.verbose {
                    let hint = if ocr || why != "Small" {
                        ""
                    } else {
                        "; --ocr reads it"
                    };
                    eprintln!("model: {name}: not sent ({why}){hint}");
                }
                if why == "Failed" && p.failure.is_none() && !quiet {
                    eprintln!("warning: {name}: {e}");
                }
                p.failure.get_or_insert((why, e));
            } else if max == 0 || self.asked < max {
                // Only an image a model is sent as it is (PNG, JPEG, TIFF):
                // a drawing kept as EMF or WMF is not. Its bytes are read
                // again when it is sent, not held until then.
                if let Some(format) = fluree_doc_pdf::image::Format::sniff(b) {
                    self.asked += 1;
                    p.send = Some(format.mime());
                }
            } else {
                self.over_budget += 1;
            }
        }
        Ok(p)
    }
}

/// Why an image a document places is not sent to the configured model, and
/// what went wrong where something did; `None` where it is sent.
///
/// - `Failed`: its header does not parse. Nothing says what it is or how
///   large, and a part that only opens like an image (a PNG's signature,
///   then zeros) would otherwise cost a billed request each.
/// - `Small`: under the floor ([`fluree_doc_model::under_model_floor`]) in its own
///   pixels, whatever holds it: an e-mail's image has no page to measure it
///   against, and a DOCX's or an XLSX's is held to the same floor, so one
///   picture is sent or not wherever it is pasted.
/// - `NotTexty`: the pixel probe (`texty`, asked last, as it decodes the
///   image) does not take it for text: a photograph, a chart, a logo, as a
///   PDF's region the probe does not take for text is not sent. A capture
///   of a few lines on a wide screen is sent. One whose header reads but
///   that does not decode (`None`) is not judged, and is sent: the model may
///   read what the decoder refuses.
fn declined_by_model(
    bytes: &[u8],
    texty: impl FnOnce() -> Option<bool>,
) -> Option<(&'static str, String)> {
    let h = match fluree_doc_image::header(bytes) {
        Ok(h) => h,
        Err(e) => return Some(("Failed", format!("its header does not parse: {e}"))),
    };
    if fluree_doc_model::under_model_floor(h.width, h.height) {
        return Some(("Small", String::new()));
    }
    (texty() == Some(false)).then(|| ("NotTexty", String::new()))
}

/// [`read_media_with`] for a source that holds its images' bytes in memory
/// (an e-mail's): an image's first bytes are a slice of them, and only an
/// image read is copied, one at a time.
fn read_media<'a>(
    stem: &str,
    media: Vec<fluree_doc_model::Media>,
    bytes: &dyn Fn(&str) -> Result<&'a [u8], String>,
    budget: &mut fluree_doc_model::budget::Budget,
    cfg: &TierConfig,
    quiet: bool,
) -> MediaRead {
    let sniffed = fluree_doc_pdf::image::Format::SNIFFED;
    read_media_with(
        stem,
        media,
        &mut |p| bytes(p).map(|b| b[..b.len().min(sniffed)].to_vec()),
        &mut |p| bytes(p).map(<[u8]>::to_vec),
        budget,
        cfg,
        quiet,
    )
}

/// An image a document places in its flow, as read so far
/// ([`read_media_with`]).
struct Placed {
    /// Its name among the document's images (`m<n>`), which its readings
    /// are keyed by.
    tag: String,
    /// What was read in it: `None` where nothing read it.
    read: Option<Vec<Element>>,
    /// Why it is not read, should nothing read it.
    failure: Option<(&'static str, String)>,
    /// The OCR reading behind `read`, for `doc:ocrReadings`.
    ocr_read: Vec<fluree_doc_model::OcrReading>,
    /// Of the words a sidecar gave it, those scored outside 0-1, of how
    /// many: warned of only once its reading is taken, not when the reading
    /// is set aside or a model's replaces it.
    scores: Option<(usize, usize)>,
    /// Its bytes are no image OCR or a model reads (EMF, WMF).
    unsupported: bool,
    /// The MIME type it is sent to the configured model as, where it is:
    /// its bytes are read again then, not held until the model reads them.
    send: Option<&'static str>,
}

impl Placed {
    /// An image nothing is to read, `failure` saying why where it is not
    /// that nothing was asked to.
    fn new(tag: String, failure: Option<(&'static str, String)>) -> Self {
        Placed {
            tag,
            read: None,
            failure,
            ocr_read: Vec::new(),
            scores: None,
            unsupported: false,
            send: None,
        }
    }

    /// Take a model's reading of it from `readings`, over what OCR read,
    /// unless it lost most of that, and say whether it was taken. A reading
    /// that holds nothing to place (the model's sentence that the image has
    /// no text, the furniture of a screen) read it all the same.
    fn take(
        &mut self,
        stem: &str,
        readings: &dyn fluree_doc_pdf::arbiter::TierBackend,
        verbose: bool,
    ) -> bool {
        if readings.read(stem, &self.tag).is_some() {
            self.read_empty();
        }
        let mut els = self.read.clone().unwrap_or_default();
        let lines: Vec<String> = els
            .iter()
            .flat_map(|e| e.text.lines().map(str::to_string).collect::<Vec<_>>())
            .collect();
        if splice_image_reading(&mut els, stem, &self.tag, readings, &lines) {
            if verbose {
                eprintln!("model: {stem}_{}: read", self.tag);
            }
            self.read = Some(els);
            self.failure = None;
            // The model's reading replaced OCR's words.
            self.ocr_read.clear();
            return true;
        }
        false
    }

    /// Spend what was read of it from `budget`, what the document left of
    /// its own: past it, it is not read, and listed past the limit.
    fn spend(&mut self, budget: &mut fluree_doc_model::budget::Budget) {
        let Some(read) = &self.read else {
            return;
        };
        if let Err(e) = fluree_doc_image::spend_reading(budget, read) {
            self.read = None;
            self.failure = Some((
                fluree_doc_model::past_limit_reason(e.limit()),
                e.to_string(),
            ));
        }
    }

    /// The model read it and found no text in it: read, as an image OCR
    /// read with no text in it is.
    fn read_empty(&mut self) {
        self.read.get_or_insert_with(Vec::new);
        self.failure = None;
    }

    /// The model could not read it: a failure, unless OCR read it.
    fn not_read_by_model(&mut self, e: String) {
        if self.read.is_none() {
            self.failure = Some(("Failed", e));
        }
    }
}

/// Send the images of `placed` that are to go to the configured model
/// together, through one reader, and take what it read of each. Each
/// image's bytes are read again by `bytes`, by its part, as it is sent
/// ([`crate::escalate::read_images`]): a document of many large pictures
/// is not held whole until the last is read.
fn read_by_model(
    stem: &str,
    placed: &mut [(fluree_doc_model::Picture, Placed)],
    bytes: &mut dyn FnMut(&str) -> Result<Vec<u8>, String>,
    budget: &mut fluree_doc_model::budget::Budget,
    cfg: &TierConfig,
    quiet: bool,
) {
    let (sent, parts): (Vec<(&str, &str)>, Vec<&str>) = placed
        .iter()
        .filter_map(|(picture, p)| Some(((p.tag.as_str(), p.send?), picture.source.part()?)))
        .unzip();
    if sent.is_empty() {
        return;
    }
    let got = crate::escalate::read_images(
        std::path::Path::new(stem),
        &sent,
        &mut |i| bytes(parts[i]).map(std::borrow::Cow::Owned),
        &cfg.config,
        cfg.verbose,
    );
    // A reader that cannot be opened is said once, for the document.
    if let (Err(e), false) = (&got, quiet) {
        eprintln!("warning: {stem}: the configured model: {e}");
    }
    settle(
        stem,
        placed.iter_mut().map(|(_, p)| p),
        &got,
        budget,
        cfg.verbose,
        quiet,
    );
}

/// Take what the model made of each image of `placed` sent to it, each
/// reading taken spent from `budget`, what the document left of its own, in
/// the order of `placed`.
fn settle<'p>(
    stem: &str,
    placed: impl IntoIterator<Item = &'p mut Placed>,
    got: &Result<crate::escalate::ImagesRead, String>,
    budget: &mut fluree_doc_model::budget::Budget,
    verbose: bool,
    quiet: bool,
) {
    for p in placed {
        if p.send.take().is_none() {
            continue;
        }
        match got {
            Ok(read) => match read.failures.iter().find(|(crop, _)| *crop == p.tag) {
                Some((_, e)) => {
                    if !quiet {
                        eprintln!("warning: {stem}_{}: the configured model: {e}", p.tag);
                    }
                    p.not_read_by_model(e.clone());
                }
                // Read, and found to hold no text: read all the same.
                None if read.empty.contains(&p.tag) => p.read_empty(),
                None => {
                    if p.take(stem, &read.readings, verbose) {
                        p.spend(budget);
                    }
                }
            },
            Err(e) => p.not_read_by_model(e.clone()),
        }
    }
}

/// Warn of a bare image's sidecar reading scored outside 0-1, once it is
/// known to be listed (`listed`, the image's readings in the output), as a
/// PDF page's is: one set aside, or one a model's reading replaced, says
/// nothing in the output.
fn warn_listed_scores(
    scores: Option<(String, usize, usize)>,
    listed: &[fluree_doc_model::OcrReading],
    quiet: bool,
) {
    if let (Some((name, n, words)), false) = (scores, listed.is_empty()) {
        common::warn_scores_out_of_range(&name, n, words, quiet);
    }
}

/// The output of a bare image as OCR read it ([`ocr_image`]), warned about
/// as a PDF's page nothing read is.
fn render_image_ocr(read: ImageOcr, src: &Source, args: &ConvertArgs, quiet: bool) -> String {
    warn_listed_scores(read.scores, &read.notes.ocr, quiet);
    if let (Some(note), false) = (read.notes.summary(), quiet) {
        eprintln!("warning: {}: {note}", src.stem);
    }
    past_limits(src.stem, &read.notes, args.format, quiet);
    render(&read.elements, src, args, read.sizes, &read.notes)
}

/// What OCR made of a bare image.
struct ImageOcr {
    elements: Vec<Element>,
    sizes: Vec<fluree_doc_model::PageSize>,
    notes: fluree_doc_model::Notes,
    /// Some word was taken, or a model's reading supplied spliced over the
    /// page.
    read_something: bool,
    /// The OCR readings of it, taken or set aside: none where no words
    /// were written for it.
    readings: usize,
    /// The sidecar reading taken, by its file's name, with those of its
    /// words' scores outside 0-1, of how many: warned of only where the
    /// reading is listed, which the caller decides.
    scores: Option<(String, usize, usize)>,
}

/// A bare image read by OCR: its words laid on a one-page document and
/// analysed like a scanned page. An engine reads the image decoded upright;
/// words already read (`--ocr-words`) need only its header for the page.
fn ocr_image(
    data: &[u8],
    header: Option<fluree_doc_image::Header>,
    src: &Source,
    cfg: &TierConfig,
    quiet: bool,
) -> Result<ImageOcr, String> {
    let stem = src.stem;
    // With the words of the sidecar reading taken, those scored outside 0-1.
    let (mut doc, flag, injected, scores) = match &cfg.ocr {
        Some(engine) => {
            let image = fluree_doc_image::decode(data)?;
            if image.truncated && !quiet {
                eprintln!(
                    "warning: {stem}: the file ends before its image does; part of it may be blank"
                );
            }
            let mut doc = fluree_doc_image::document(&image);
            let reading = engine
                .0
                .read(&image.pixels)
                .map_err(|e| format!("--ocr: {e}"))?;
            let page = fluree_doc_pdf::ocr::Crop::Page;
            let injected = fluree_doc_pdf::ocr::inject(&mut doc, &[(0, page, reading)]);
            (doc, "--ocr", injected, None)
        }
        None => {
            let h = header.ok_or("the image's header could not be read")?;
            let mut doc = fluree_doc_pdf::image::page_of(h.width, h.height, h.bitonal);
            let dir = cfg.ocr_words.as_deref().expect("words or an engine");
            // The words of its crop (`<stem>_p0_full.json`), or of the
            // image file (`<stem>.json`, as `fdoc ocr` names them): in a
            // directory shared with a PDF of the same stem, the one read on
            // this image is the one sized as it is.
            let mut readings: Vec<_> = common::ocr_readings(dir, stem, &doc, quiet)
                .into_iter()
                .map(|r| (format!("{stem}_p{}_{}.json", r.0, r.1.tag()), r))
                .collect();
            readings.extend(image_words(dir, stem, quiet).map(|r| (format!("{stem}.json"), r)));
            let sized = |r: &fluree_doc_pdf::ocr::WordReading| {
                r.coords == fluree_doc_pdf::ocr::Coords::Page
                    || (r.width, r.height) == (Some(f64::from(h.width)), Some(f64::from(h.height)))
            };
            let pages = readings
                .iter()
                .filter(|(_, r)| r.1 == fluree_doc_pdf::ocr::Crop::Page)
                .count();
            let (name, readings) = if let Some(k) = readings.iter().position(|(_, r)| sized(&r.2)) {
                let (name, taken) = readings.swap_remove(k);
                (name, vec![taken])
            } else {
                if !readings.is_empty() && !quiet {
                    eprintln!(
                        "warning: --ocr-words: {stem}: no reading is sized as the image ({} × {}): none read",
                        h.width, h.height
                    );
                }
                (String::new(), Vec::new())
            };
            if pages > 1 && !quiet {
                eprintln!(
                    "warning: --ocr-words: {stem}: both {stem}_p0_full.json and {stem}.json; read the one sized as the image"
                );
            }
            // Of the reading taken only: the other candidate is not read.
            let scores = readings
                .first()
                .map(|(_, _, r)| (name, r.scores_out_of_range(), r.words_with_text()));
            let injected = fluree_doc_pdf::ocr::inject(&mut doc, &readings);
            (doc, "--ocr-words", injected, scores)
        }
    };
    report_ocr(stem, flag, &injected, cfg.verbose, quiet);
    let mut a = fluree_doc_pdf::document::analyze_with(&mut doc, &[], &cfg.options_for(stem));
    // Readings someone already has (`--tier-results`, `--structure-results`)
    // replace what OCR made of the page, as on a PDF page.
    let page_text: Vec<Vec<String>> = doc
        .pages
        .iter()
        .map(|p| fluree_doc_pdf::fidelity::page_lines(&p.glyphs))
        .collect();
    common::apply_tiers(
        cfg.tier_results.as_deref(),
        cfg.structure_results.as_deref(),
        cfg.emit_anchors,
        stem,
        &mut a.elements,
        &page_text,
        &a.furniture,
        &Default::default(),
    );
    fluree_doc_pdf::ocr::mark_provenance(&mut a.elements, &injected);
    // Its tables' copies, as a PDF's are (`spend_table_copies`): one page,
    // so past the budget the image is refused rather than read short.
    fluree_doc_pdf::spend_table_copies(
        &mut doc,
        &mut a.elements,
        &mut fluree_doc_pdf::PageBudget::default(),
    )
    .map_err(|e| format!("image too large: {}", common::refused(&e)))?;
    let sizes = doc
        .pages
        .iter()
        .map(|p| {
            // A page OCR turned a quarter to read is still the image as shown.
            let (width, height) = p.display_size();
            fluree_doc_model::PageSize {
                index: p.index,
                width,
                height,
                folio: None,
            }
        })
        .collect();
    // Given on its own, an image is read once OCR took a word in it, however
    // few: a stamp's `PAID`, a label's one line. The floor a scanned PDF's
    // page is held to (`OcrSparse`: a page read whole under 40 characters,
    // an engine that gave two words for a dense scan) judges a page among
    // pages of text, not an image whose few words may be all it holds.
    let read_something = injected.iter().any(|r| r.refused.is_none() && r.taken > 0)
        || a.elements.iter().any(|e| e.evidence == "page-tier");
    // However unsure OCR is of them: its confidence is reported
    // (`doc:ocrReadings`), and routes nothing.
    let unread = if read_something {
        Vec::new()
    } else {
        fluree_doc_pdf::unread_pages(&doc, &a.elements)
    };
    let mut notes = fluree_doc_model::Notes {
        unread,
        ocr: fluree_doc_pdf::ocr::readings(&injected, &a.elements),
        ..Default::default()
    };
    // The pages after the first of a multi-page TIFF, never read.
    notes
        .unread
        .extend(later_pages(header.map_or(1, |h| h.pages)));
    Ok(ImageOcr {
        elements: a.elements,
        sizes,
        notes,
        read_something,
        readings: injected.len(),
        scores,
    })
}

/// Where a limit left a document's pages unread, the page it was read to
/// and the flag that raises the limit that stopped it, on stderr: the
/// budget's where it ran out first, else the page limit's.
///
/// Said under `--quiet` too where the output carries no notes (`-f json`,
/// `-f text`): nothing else would tell a document read short from a whole
/// one. The others list each page in band.
fn past_limits(stem: &str, notes: &fluree_doc_model::Notes, format: Format, quiet: bool) {
    if quiet && !matches!(format, Format::Json | Format::Text) {
        return;
    }
    let first = (notes.unread.iter())
        .filter(|u| fluree_doc_model::is_past_limit(&u.reason))
        .min_by_key(|u| u.index);
    if let Some(first) = first {
        let flag = match first.reason.as_str() {
            fluree_doc_model::PAST_MEMORY_LIMIT => "--max-memory",
            fluree_doc_model::PAST_WORK_LIMIT => "--max-work",
            _ => "--max-pages",
        };
        eprintln!(
            "note: {stem}: read to page {} (raise it with {flag})",
            first.index
        );
    }
}

/// The pages after the first of a file of `pages` (a multi-page TIFF): never
/// read, so listed unread, for the output to say so. Counted to one past
/// the page limit (`fluree_doc_image::Header::pages`), that one is listed
/// `PastPageLimit`: the file goes on, uncounted, from there.
fn later_pages(pages: usize) -> impl Iterator<Item = fluree_doc_model::UnreadPage> {
    let limit = fluree_doc_model::budget::limits().pages;
    (1..pages).map(move |index| fluree_doc_model::UnreadPage {
        index,
        reason: if index < limit {
            "LaterPage"
        } else {
            fluree_doc_model::PAST_PAGE_LIMIT
        }
        .into(),
    })
}

/// The words `fdoc ocr` read from the image file itself: `<stem>.json`.
fn image_words(
    dir: &Path,
    stem: &str,
    quiet: bool,
) -> Option<(
    usize,
    fluree_doc_pdf::ocr::Crop,
    fluree_doc_pdf::ocr::WordReading,
)> {
    let path = dir.join(format!("{stem}.json"));
    let text = std::fs::read_to_string(&path).ok()?;
    match fluree_doc_pdf::ocr::WordReading::from_json(&text) {
        Ok(r) => Some((0, fluree_doc_pdf::ocr::Crop::Page, r)),
        Err(e) => {
            if !quiet {
                eprintln!("warning: --ocr-words: {}: {e}", path.display());
            }
            None
        }
    }
}

/// Emit an element stream in the requested format. Shared by every source.
fn render(
    elements: &[Element],
    src: &Source,
    args: &ConvertArgs,
    pages: Vec<fluree_doc_model::PageSize>,
    notes: &fluree_doc_model::Notes,
) -> String {
    // No source writes a stray control character into what it emits: a
    // model's reading or another format's text can carry one as surely as a
    // PDF's font.
    let scrubbed: Vec<Element>;
    let elements = if elements.iter().any(|e| {
        e.text.chars().any(fluree_doc_model::is_stray_control)
            || e.cells
                .iter()
                .flatten()
                .flatten()
                .any(|c| c.chars().any(fluree_doc_model::is_stray_control))
    }) {
        scrubbed = elements
            .iter()
            .cloned()
            .map(|mut e| {
                e.scrub_controls();
                e
            })
            .collect();
        &scrubbed[..]
    } else {
        elements
    };
    match args.format {
        Format::Md => fluree_doc_model::to_markdown_with_options(
            elements,
            notes,
            &fluree_doc_model::MarkdownOptions {
                links: args.md_links.into(),
            },
        ),
        Format::Xhtml => fluree_doc_model::to_xhtml_with(elements, notes),
        Format::Json => serde_json::to_string_pretty(elements).unwrap(),
        Format::Doco => {
            // A caller naming the document names its nodes too: they are
            // minted under its IRI, so they cost the store no namespace the
            // document IRI does not.
            let base_iri = args
                .base_iri
                .clone()
                .or_else(|| args.doc_iri.clone())
                .unwrap_or_else(|| {
                    fluree_doc_model::doco::default_base_iri(
                        src.named.then_some(src.stem),
                        &src.sha256,
                    )
                });
            let opts = fluree_doc_pdf::doco::DocoOptions {
                base_iri,
                doc_iri: args.doc_iri.clone(),
                pages,
                unread: notes.unread.clone(),
                unread_regions: notes.unread_regions.clone(),
                warnings: notes.warnings.clone(),
                unread_images: notes.unread_images.clone(),
                ocr_readings: notes.ocr.clone(),
                running_text: notes.running_text.clone(),
                info: notes.info.clone(),
                attachments: notes.attachments.clone(),
                sha256: Some(src.sha256.clone()),
                source_name: src.name.clone(),
            };
            fluree_doc_pdf::doco::to_doco(elements, &opts)
        }
        Format::Text => fluree_doc_pdf::doco::to_text(elements),
        Format::Conform => {
            serde_json::to_string(&fluree_doc_conform::conform(elements, notes)).unwrap()
        }
    }
}

/// The conformance violations of one file as JSON: its elements read as
/// `convert` reads them, deterministically, each format written from them
/// and read back.
pub(crate) fn conform_report(path: &Path) -> Result<String, String> {
    let args = super::compat(path.to_path_buf(), Format::Conform);
    let mut cfg = TierConfig::from_env_with(None, None, None, None, false);
    cfg.resolve_escalation(false, true, true);
    let slot = slots(std::slice::from_ref(&args.inputs[0])).remove(0);
    convert_path(path, &slot, &cfg, &args, None, true)
}

/// The note when a document has more OCR crops than `ocr.max_crops`.
pub(crate) fn ocr_crowded_note(stem: &str, small: usize, crowded: usize) -> String {
    format!(
        "note: --ocr: {stem}: leaving {crowded} small region(s) unread on crowded pages (the {small} largest on a page are read) — raise `ocr.max_small_regions_per_page` (0 for no limit) to read them all"
    )
}

pub(crate) fn ocr_cap_note(stem: &str, max: usize, capped: usize) -> String {
    format!(
        "note: --ocr: {stem}: reading {max} crops and leaving {capped} unread (pages read whole first, then routed regions, then small ones) — raise `ocr.max_crops` (0 for no limit) to read them all"
    )
}

/// One stderr line per OCR reading set aside, and with `--verbose` one per
/// reading taken.
/// Name, with `-v`, each crop an engine read but left to the model.
fn report_left(stem: &str, flag: &str, left: &[(String, &'static str)], verbose: bool) {
    if verbose {
        for (crop, why) in left {
            eprintln!("ocr: {stem}_{crop}: left to the model: {why} ({flag})");
        }
    }
}

/// Name, with `-v`, each crop whose reading was set aside as artwork.
fn report_set_aside(stem: &str, flag: &str, aside: &[String], verbose: bool) {
    if verbose {
        for crop in aside {
            eprintln!("ocr: {stem}_{crop}: set aside: the engine is unsure of it ({flag})");
        }
    }
}

fn report_ocr(
    stem: &str,
    flag: &str,
    injected: &[fluree_doc_pdf::ocr::Injected],
    verbose: bool,
    quiet: bool,
) {
    for r in injected {
        let name = format!("{stem}_p{}_{}", r.page, r.crop.tag());
        match r.refused {
            Some(why) if !quiet => eprintln!("warning: {flag}: {name}: {why}"),
            None if verbose => eprintln!(
                "ocr: {name}: {} words taken, {} dropped, skew {:.1}°{}",
                r.taken,
                r.dropped,
                r.skew_deg,
                r.mean_conf
                    .map(|c| format!(", mean confidence {c:.3}"))
                    .unwrap_or_default()
            ),
            _ => {}
        }
    }
}

fn convert_bytes(
    data: Vec<u8>,
    src: &Source,
    cfg: &TierConfig,
    args: &ConvertArgs,
    pages: Option<&[usize]>,
    quiet: bool,
) -> Result<String, String> {
    let stem = src.stem;
    // Opened once, for the outline, the links and the renders as well as
    // the extraction.
    let raw = fluree_doc_pdf::open(data).map_err(|e| common::refused(&e))?;
    let ol = outline::extract(&raw);
    let opts = cfg.options_for(stem);
    // Images are probed for whatever reads their pixels' verdict: the
    // router, when it runs for escalation or for the anchors and sidecars
    // analysis routes for; OCR words, which go only where it sends a reader;
    // an OCR engine; and a caller who asks for the verdicts
    // (`--probe-images`). The images too small to route are probed only
    // for OCR, which reads the texty ones, and for that caller: without
    // them nothing would read those, and they are most of the decode. A
    // deterministic parse decodes none, and reports each image the text
    // layer does not cover as not examined
    // ([`fluree_doc_pdf::route::Declined::Unprobed`]).
    let ocr = cfg.ocr_words.is_some() || cfg.ocr.is_some();
    let mut eopts = ExtractOptions::default();
    eopts.probe_images = args.probe_images
        || cfg.escalate
        || ocr
        || opts.emit_anchors
        || opts.layout_prefix.is_some();
    eopts.probe_small_images = ocr || args.probe_images;
    // What each image shows, so an engine reads an image drawn again once
    // (`ocr::copies`): read by OCR alone, and a hash of every image's
    // stream (decrypted, in an encrypted file) a run without it never uses.
    eopts.key_images = ocr;
    // The document's budget, spent by its pages as they are read, then by
    // the copies DoCO writes of its tables once its elements are final.
    let mut budget = fluree_doc_pdf::PageBudget::default();
    let mut doc = fluree_doc_pdf::extract_pdf_within(&raw, &eopts, &mut budget)
        .map_err(|e| format!("extract: {}", common::refused(&e)))?;
    // OCR words become glyphs before anything measures the pages: the
    // analysis reads them like any text, and a region they fill no longer
    // asks a model for a reading.
    // Where a model reads too, in this run or from its readings, a reading
    // that is not enough on its own is left to it.
    let modelled = cfg.escalate || cfg.tier_results.is_some();
    // With the words of each sidecar reading, those scored outside 0-1, by
    // page and crop: warned of once the readings listed are known (below).
    let (injected, scores) = if let Some(engine) = &cfg.ocr {
        let max = cfg.config.ocr.max_crops();
        // At the stored resolution of what is under each crop (#11): an
        // engine reads small text a 2× render loses.
        let small = cfg.config.ocr.max_small_regions_per_page();
        let pass = fluree_doc_pdf::ocr::read_pages_leaving(
            &mut doc,
            &raw,
            engine.0.as_ref(),
            pages,
            fluree_doc_pdf::escalate::CropScale::Stored,
            (max, small),
            modelled,
        );
        if pass.crowded > 0 && !quiet {
            eprintln!("{}", ocr_crowded_note(stem, small, pass.crowded));
        }
        report_left(stem, "--ocr", &pass.left, cfg.verbose);
        report_set_aside(stem, "--ocr", &pass.set_aside, cfg.verbose);
        for (crop, e) in &pass.failed {
            if !quiet {
                eprintln!("warning: --ocr: {stem}_{crop}: {e}");
            }
        }
        if pass.capped > 0 && !quiet {
            eprintln!("{}", ocr_cap_note(stem, max, pass.capped));
        }
        report_ocr(stem, "--ocr", &pass.injected, cfg.verbose, quiet);
        (pass.injected, Vec::new())
    } else if let Some(dir) = cfg.ocr_words.as_deref() {
        // Only the pages asked for, as the engine reads only those: words
        // already read for the others must not change how these read.
        let mut readings = common::ocr_readings(dir, stem, &doc, quiet);
        if let Some(keep) = pages {
            readings.retain(|(page, _, _)| keep.contains(page));
        }
        // A region showing what an earlier one shows has no crop of its own
        // to read (`render-routed --ocr-regions` cuts it once): it takes the
        // earlier one's reading, as `--ocr` gives it.
        let mut readings = fluree_doc_pdf::ocr::with_copies(&doc, pages, readings);
        if modelled {
            let left = fluree_doc_pdf::ocr::leave(&doc, &mut readings);
            report_left(stem, "--ocr-words", &left, cfg.verbose);
        }
        let aside = fluree_doc_pdf::ocr::set_aside_unsure(&doc, &mut readings);
        report_set_aside(stem, "--ocr-words", &aside, cfg.verbose);
        let injected = fluree_doc_pdf::ocr::inject(&mut doc, &readings);
        let scores = readings
            .iter()
            .map(|(page, crop, r)| {
                (
                    *page,
                    crop.tag(),
                    r.scores_out_of_range(),
                    r.words_with_text(),
                )
            })
            .collect();
        report_ocr(stem, "--ocr-words", &injected, cfg.verbose, quiet);
        (injected, scores)
    } else {
        (Vec::new(), Vec::new())
    };
    // A configured reader reads the whole pages chosen from the document as
    // extracted, which the analysis below changes.
    let read_here = cfg.escalate && cfg.tier_results.is_none();
    let extracted = read_here.then(|| doc.clone());
    let mut a = fluree_doc_pdf::document::analyze_with(&mut doc, &ol, &opts);
    common::arbitrate_layout_titles(cfg.layout_boxes.as_deref(), stem, &mut a.elements);
    // The page's own text, so the arbiter can ask whether an escalated
    // reading says anything the page does not.
    let page_text: Vec<Vec<String>> = doc
        .pages
        .iter()
        .map(|p| fluree_doc_pdf::fidelity::page_lines(&p.glyphs))
        .collect();
    // The regions a screen pasted into a page of text fills: a model's
    // reading of one keeps the document's outline, as OCR's does.
    let screens = common::pasted_regions(&a.elements, &doc.pages);
    common::apply_tiers(
        cfg.tier_results.as_deref(),
        cfg.structure_results.as_deref(),
        cfg.emit_anchors,
        stem,
        &mut a.elements,
        &page_text,
        &a.furniture,
        &screens,
    );
    // A configured reader, in this same command. Sidecars win where both are
    // present: `--tier-results` names readings someone already has, and
    // paying to produce them again would be surprising.
    if let Some(extracted) = &extracted {
        read_in_run(
            &mut a,
            extracted,
            cfg,
            stem,
            &page_text,
            &screens,
            pages,
            |jobs, hints| {
                crate::escalate::read_document(
                    std::path::Path::new(stem),
                    jobs,
                    hints,
                    &doc,
                    &raw,
                    &cfg.config,
                    cfg.verbose,
                )
            },
        )?;
    }
    // After the tiers, which replace whole pages; before the links, which a
    // footer carries as often as the body does.
    let (first, last) = match pages {
        Some(keep) => (
            keep.iter().copied().min().unwrap_or(0),
            keep.iter().copied().max().unwrap_or(0),
        ),
        None => (0, doc.pages.len().saturating_sub(1)),
    };
    fluree_doc_pdf::furniture::keep_running(&mut a.elements, &a.running, first, last);
    // After the tiers, whose splices keep their own provenance, and after the
    // running lines kept once, which may be OCR's too.
    fluree_doc_pdf::ocr::mark_provenance(&mut a.elements, &injected);
    // After the tiers: an escalated reading replaces the text an anchor has to
    // be found in.
    fluree_doc_pdf::link::attach(
        &mut a.elements,
        &fluree_doc_pdf::link::extract(&raw),
        &doc.pages,
    );
    // The files it embeds, described as an e-mail's attachments are, and
    // saved with them under `--attachments`.
    let (embedded, skipped) = fluree_doc_pdf::annot::embedded_files(&raw);
    if !quiet {
        for s in &skipped {
            eprintln!(
                "warning: {stem}: embedded file {} not taken out ({})",
                s.name.as_deref().unwrap_or("(unnamed)"),
                s.reason
            );
        }
    }
    let files: Vec<(&fluree_doc_model::Attachment, &[u8])> = embedded
        .iter()
        .map(|f| (&f.info, f.bytes.as_slice()))
        .collect();
    save_attachments(&files, src, args, &[], "embedded file", quiet)?;
    if let Some(keep) = pages {
        a.elements.retain(|e: &Element| keep.contains(&e.page));
    }
    // The elements are those written from here, but for the notes: their
    // tables' copies are spent, the document read short to the most pages
    // whose glyphs and copies fit, before the notes, the pages and the
    // unread pages are told from `doc`.
    fluree_doc_pdf::spend_table_copies(&mut doc, &mut a.elements, &mut budget)
        .map_err(|e| common::refused(&fluree_doc_pdf::ExtractError::TooLarge(e)))?;
    // The notes a reader left on the pages read, when asked for
    // (`--comments`), after each page's own text, from what the document
    // left of its budget once its pages and their tables are final: past
    // it, the notes are left out with a warning, all of them, and give back
    // what they spent; the pages are kept whole, not read short for notes
    // nobody prints. A markup quoting the text it marks would say that text
    // twice, and is left out. The notes on the pages read are spent, those
    // `--pages` leaves out among them.
    if args.comments {
        let notes =
            budget.spend_whole(|b| fluree_doc_pdf::annot::annotations(&raw, doc.pages.len(), b));
        match notes {
            Ok(mut annotations) => {
                fluree_doc_pdf::annot::drop_quoting_markups(&mut annotations, &doc.pages);
                annotations.retain(|n| pages.is_none_or(|keep| keep.contains(&n.page)));
                fluree_doc_pdf::annot::place(
                    &mut a.elements,
                    fluree_doc_pdf::annot::elements(&annotations),
                );
            }
            Err(e) if !quiet => {
                eprintln!("warning: {stem}: notes left out: {}", common::refused(&e))
            }
            Err(_) => {}
        }
    }
    // Page geometry travels with the graph: a bbox cannot be placed on a
    // rendered page without the size of the page it came from.
    let sizes = doc
        .pages
        .iter()
        .filter(|p| pages.is_none_or(|keep| keep.contains(&p.index)))
        .map(|p| {
            let (width, height) = p.display_size();
            fluree_doc_model::PageSize {
                index: p.index,
                width,
                height,
                folio: a.folios.get(p.index).cloned().flatten(),
            }
        })
        .collect();
    // After the tiers: a page or a region is unread only once whatever was
    // going to read it has run. Only on the pages asked for: one left out
    // was not read because nobody asked, which the output need not say.
    let asked = |page: usize| pages.is_none_or(|keep| keep.contains(&page));
    let mut unread = fluree_doc_pdf::unread_pages(&doc, &a.elements);
    unread.retain(|u| asked(u.index));
    let mut unread_regions = fluree_doc_pdf::unread_regions(&doc, &a.elements);
    unread_regions.retain(|u| asked(u.page));
    let mut warnings = fluree_doc_pdf::parse_warnings(&doc);
    warnings.retain(|w| asked(w.page));
    let mut ocr = fluree_doc_pdf::ocr::readings(&injected, &a.elements);
    // Nothing to drop while `--ocr` reads only the pages asked for and
    // `--ocr-words` injects only their readings. A second guard behind
    // those: an empty reading of a page left out has no element to tell it
    // is not wanted, and would otherwise be listed.
    ocr.retain(|r| r.page.is_none_or(asked));
    // Of the readings listed only, as of a placed picture: one of a page not
    // asked for, one set aside, or one a model's reading replaced says
    // nothing in the output.
    for (page, tag, n, words) in scores {
        if ocr.iter().any(|r| r.page == Some(page) && r.crop == tag) {
            let name = format!("{stem}_p{page}_{tag}.json");
            common::warn_scores_out_of_range(&name, n, words, quiet);
        }
    }
    let notes = fluree_doc_model::Notes {
        unread,
        unread_regions,
        warnings,
        // A page number identifies nothing, bare or as `Page 3 of 15`; the
        // rest of the running block is the document's own name for itself.
        running_text: a
            .furniture
            .iter()
            .filter(|(text, vary)| fluree_doc_pdf::furniture::says_something(text, *vary))
            .map(|(text, _)| text.clone())
            .collect(),
        info: fluree_doc_pdf::info::read(&raw),
        ocr,
        attachments: embedded.iter().map(|f| f.info.clone()).collect(),
        ..Default::default()
    };
    if let (Some(note), false) = (notes.summary(), quiet) {
        eprintln!("warning: {stem}: {note}");
    }
    past_limits(stem, &notes, args.format, quiet);
    if !quiet {
        for line in parse_warning_lines(&notes.warnings) {
            eprintln!("warning: {stem}: {line}");
        }
    }
    let asked_pages = doc.pages.iter().filter(|p| asked(p.index)).count();
    if let Some(why) = read_nothing(doc.pages.len(), asked_pages, &a.elements, &notes) {
        if args.strict {
            return Err(why);
        }
        if !quiet {
            eprintln!("warning: {stem}: {why}");
        }
    }
    Ok(render(&a.elements, src, args, sizes, &notes))
}

/// An XPS or OpenXPS document: its pages' glyphs laid out by the PDF
/// pipeline, as a PDF's are, with their geometry.
///
/// A page that is only a picture (a screen printed to XPS) routes as a
/// scanned PDF page does, and is read the ways one is: by OCR (`--ocr`,
/// which reads the picture's own pixels; `--ocr-words`,
/// `<stem>_p<N>_full.json`), by a reading someone already has
/// (`--tier-results`, `<stem>_p<N>_full.json`), or by the configured model,
/// sent the picture's bytes. A page nothing read is listed unread.
fn convert_xps(
    data: &[u8],
    src: &Source,
    cfg: &TierConfig,
    args: &ConvertArgs,
    pages: Option<&[usize]>,
    quiet: bool,
) -> Result<String, String> {
    use fluree_doc_pdf::ocr::{Crop, WordReading};
    let stem = src.stem;
    let xps = fluree_doc_xps::parse(data).map_err(|e| common::refused(&e))?;
    let mut doc = xps.doc.clone();
    let asked = |page: usize| pages.is_none_or(|keep| keep.contains(&page));
    // The pages that are pictures nothing else on them reads: the router
    // sends them to a reader as it would a scan.
    let pictured: Vec<(usize, &fluree_doc_xps::PageImage)> = doc
        .pages
        .iter()
        .filter(|p| asked(p.index))
        .filter(|p| {
            matches!(
                fluree_doc_pdf::route::decide(p).0,
                fluree_doc_pdf::route::Route::Vlm(_)
            )
        })
        .filter_map(|p| Some((p.index, xps.page_image(p.index)?)))
        .collect();
    let image = |img: &fluree_doc_xps::PageImage| fluree_doc_xps::read_image(data, &img.part);
    let plans = common::xps_crop_plans(&xps, pages);
    let injected = if let Some(engine) = &cfg.ocr {
        // The picture's own pixels, whole for a page that is a picture, in
        // part under each raster region of a page of text, its words placed
        // where it is drawn; each crop read as it is cut, a page's pictures
        // at a time.
        let mut readings = Vec::new();
        let failed = common::xps_crops(data, &plans, &mut |c| {
            let name = format!("{stem}_p{}_{}", c.page, c.crop.tag());
            match engine.0.read(&c.pixels) {
                Ok(r) => {
                    let r = fluree_doc_xps::reading_on_page(
                        r,
                        &c.bbox,
                        c.pixels.width,
                        c.pixels.height,
                    );
                    readings.push((c.page, c.crop, r));
                }
                Err(e) => {
                    if !quiet {
                        eprintln!("warning: --ocr: {name}: {e}");
                    }
                }
            }
        });
        for e in failed {
            if !quiet {
                eprintln!("warning: --ocr: {stem}: {e}");
            }
        }
        let injected = fluree_doc_pdf::ocr::inject(&mut doc, &readings);
        report_ocr(stem, "--ocr", &injected, cfg.verbose, quiet);
        injected
    } else if let Some(dir) = cfg.ocr_words.as_deref() {
        let mut readings: Vec<(usize, Crop, WordReading)> =
            common::ocr_readings(dir, stem, &doc, quiet);
        readings.retain(|(page, _, _)| asked(*page));
        // A crop is of the picture's own pixels, not of the page: its words
        // are placed by the box it shows, as `--ocr` places them, so the
        // three steps read what `--ocr` reads. A reading of no crop this
        // document has is placed as a PDF page's is.
        let readings: Vec<(usize, Crop, WordReading)> = readings
            .into_iter()
            .map(|(page, crop, reading)| {
                let plan = plans.iter().find(|p| p.page == page && p.crop == crop);
                let (Some(plan), Some(w), Some(h)) = (plan, reading.width, reading.height) else {
                    return (page, crop, reading);
                };
                match plan.bbox(data) {
                    Ok(b) => {
                        let (w, h) = (w.round() as u32, h.round() as u32);
                        (
                            page,
                            crop,
                            fluree_doc_xps::reading_on_page(reading, &b, w, h),
                        )
                    }
                    Err(e) => {
                        if !quiet {
                            eprintln!("warning: --ocr-words: {stem}_p{page}_{}: {e}", crop.tag());
                        }
                        (page, crop, reading)
                    }
                }
            })
            .collect();
        let injected = fluree_doc_pdf::ocr::inject(&mut doc, &readings);
        report_ocr(stem, "--ocr-words", &injected, cfg.verbose, quiet);
        injected
    } else {
        Vec::new()
    };
    let opts = cfg.options_for(stem);
    let mut a = fluree_doc_pdf::document::analyze_with(&mut doc, &[], &opts);
    let page_text: Vec<Vec<String>> = doc
        .pages
        .iter()
        .map(|p| fluree_doc_pdf::fidelity::page_lines(&p.glyphs))
        .collect();
    // The regions a screen pasted into a page of text fills, as a PDF's.
    let screens = common::pasted_regions(&a.elements, &doc.pages);
    common::apply_tiers(
        cfg.tier_results.as_deref(),
        cfg.structure_results.as_deref(),
        cfg.emit_anchors,
        stem,
        &mut a.elements,
        &page_text,
        &a.furniture,
        &screens,
    );
    // The configured model, for a pictured page nothing read yet, sent the
    // picture as it is, within the document's `escalation.max_crops`.
    if cfg.escalate && cfg.tier_results.is_none() {
        let unread: Vec<usize> = fluree_doc_pdf::unread_pages(&doc, &a.elements)
            .iter()
            .map(|u| u.index)
            .collect();
        let max = cfg.config.escalation.max_crops;
        let mut sent: Vec<(String, Vec<u8>, &'static str)> = Vec::new();
        for (page, img) in pictured.iter().filter(|(p, _)| unread.contains(p)) {
            if max != 0 && sent.len() >= max {
                break;
            }
            match image(img) {
                Ok(b) => match fluree_doc_pdf::image::Format::sniff(&b) {
                    Some(f) => sent.push((format!("p{page}_full"), b, f.mime())),
                    None => {
                        if !quiet {
                            eprintln!(
                                "warning: {stem}_p{page}_full: not a PNG, JPEG or TIFF image"
                            );
                        }
                    }
                },
                Err(e) => {
                    if !quiet {
                        eprintln!("warning: {stem}_p{page}_full: {e}");
                    }
                }
            }
        }
        if !sent.is_empty() {
            let crops: Vec<(&str, &str)> = sent.iter().map(|(c, _, m)| (c.as_str(), *m)).collect();
            match crate::escalate::read_images(
                Path::new(stem),
                &crops,
                &mut |i| Ok(std::borrow::Cow::Borrowed(sent[i].1.as_slice())),
                &cfg.config,
                cfg.verbose,
            ) {
                // A picture read and found to hold no text leaves its page
                // unread, as a page of a PDF is left.
                Ok(read) => {
                    for (crop, e) in &read.failures {
                        if !quiet {
                            eprintln!("warning: {stem}_{crop}: the configured model: {e}");
                        }
                    }
                    fluree_doc_pdf::arbiter::splice_with_page(
                        &mut a.elements,
                        stem,
                        &read.readings,
                        None,
                        &page_text,
                    );
                }
                Err(e) => {
                    if !quiet {
                        eprintln!("warning: {stem}: the configured model: {e}");
                    }
                }
            }
        }
    }
    let (first, last) = match pages {
        Some(keep) => (
            keep.iter().copied().min().unwrap_or(0),
            keep.iter().copied().max().unwrap_or(0),
        ),
        None => (0, doc.pages.len().saturating_sub(1)),
    };
    fluree_doc_pdf::furniture::keep_running(&mut a.elements, &a.running, first, last);
    fluree_doc_pdf::ocr::mark_provenance(&mut a.elements, &injected);
    if pages.is_some() {
        a.elements.retain(|e: &Element| asked(e.page));
    }
    // Its tables' copies, as a PDF's are (`spend_table_copies`), from a
    // budget of their own: what the package's pages take is bounded by the
    // reader's own bounds, not spent from one.
    fluree_doc_pdf::spend_table_copies(
        &mut doc,
        &mut a.elements,
        &mut fluree_doc_pdf::PageBudget::default(),
    )
    .map_err(|e| common::refused(&fluree_doc_xps::XpsError::TooLarge(e)))?;
    let sizes = doc
        .pages
        .iter()
        .filter(|p| asked(p.index))
        .map(|p| fluree_doc_model::PageSize {
            index: p.index,
            width: p.width,
            height: p.height,
            folio: a.folios.get(p.index).cloned().flatten(),
        })
        .collect();
    let mut unread = fluree_doc_pdf::unread_pages(&doc, &a.elements);
    unread.retain(|u| asked(u.index));
    let mut unread_regions = fluree_doc_pdf::unread_regions(&doc, &a.elements);
    unread_regions.retain(|u| asked(u.page));
    let mut ocr = fluree_doc_pdf::ocr::readings(&injected, &a.elements);
    ocr.retain(|r| r.page.is_none_or(asked));
    // A page part that would not read, read as blank.
    let mut warnings = fluree_doc_pdf::parse_warnings(&doc);
    warnings.retain(|w| asked(w.page));
    let notes = fluree_doc_model::Notes {
        unread,
        unread_regions,
        running_text: a
            .furniture
            .iter()
            .filter(|(text, _)| fluree_doc_pdf::furniture::names_something(text))
            .map(|(text, _)| text.clone())
            .collect(),
        info: xps.info.clone(),
        ocr,
        warnings,
        ..Default::default()
    };
    if let (Some(note), false) = (notes.summary(), quiet) {
        eprintln!("warning: {stem}: {note}");
    }
    past_limits(stem, &notes, args.format, quiet);
    if !quiet {
        for line in parse_warning_lines(&notes.warnings) {
            eprintln!("warning: {stem}: {line}");
        }
    }
    let asked_pages = doc.pages.iter().filter(|p| asked(p.index)).count();
    if let Some(why) = read_nothing(doc.pages.len(), asked_pages, &a.elements, &notes) {
        if args.strict {
            return Err(why);
        }
        if !quiet {
            eprintln!("warning: {stem}: {why}");
        }
    }
    Ok(render(&a.elements, src, args, sizes, &notes))
}

/// How many pages a parser warning names on stderr; `doc:parseWarnings`
/// lists them all.
const LISTED_PAGES: usize = 10;

/// One line per kind of parser warning, naming its pages (1-based, the
/// first [`LISTED_PAGES`] of them) and what it costs the output.
fn parse_warning_lines(warnings: &[fluree_doc_model::ParseWarning]) -> Vec<String> {
    let mut kinds: Vec<&str> = warnings.iter().map(|w| w.kind.as_str()).collect();
    kinds.sort_unstable();
    kinds.dedup();
    kinds
        .into_iter()
        .map(|kind| {
            let all: Vec<usize> = warnings
                .iter()
                .filter(|w| w.kind == kind)
                .map(|w| w.page + 1)
                .collect();
            let mut pages: Vec<String> = all
                .iter()
                .take(LISTED_PAGES)
                .map(usize::to_string)
                .collect();
            if all.len() > LISTED_PAGES {
                pages.push(format!("and {} more", all.len() - LISTED_PAGES));
            }
            let what = match kind {
                "ImageDecodeFailure" => "an image could not be decoded; what it shows is missing",
                "MissingPagePart" => "the page's part is missing or unreadable; read as blank",
                // Any other kind the interpreter may name (it declares
                // `UnsupportedFont`, but never reports it).
                _ => "the parser reported a problem",
            };
            format!(
                "{kind} on page{} {}: {what}",
                if all.len() == 1 { "" } else { "s" },
                pages.join(", ")
            )
        })
        .collect()
}

/// Why a PDF's output holds nothing at all, when it does: no element, and
/// nothing reported unread.
///
/// Every page the router can tell carries content is reported unread when
/// nothing read it, so an output that is empty *and* reports nothing is a
/// document with no text layer and no image: blank, its text drawn as
/// vector outlines (the router does not count drawing operations), or one
/// the parser could not open properly: a page tree that loops or nests
/// past what the parser follows, or a reference one flipped bit broke, all
/// come out as a valid, empty document. They cannot be told apart from
/// here, and a batch must not take any of them for a successful parse in
/// silence.
///
/// `pages` is the document's page count, `asked` how many of them `--pages`
/// kept. A selection that keeps none of them read nothing either: page
/// numbers past the end of a document, in a batch, must not pass in silence.
fn read_nothing(
    pages: usize,
    asked: usize,
    elements: &[Element],
    notes: &fluree_doc_model::Notes,
) -> Option<String> {
    if !elements.is_empty() || !notes.unread.is_empty() || !notes.unread_regions.is_empty() {
        return None;
    }
    Some(match (pages, asked) {
        (0, _) => "nothing was read: no page was found (the file may be damaged)".into(),
        (n, 0) => format!(
            "nothing was read: --pages selects none of its {n} page{}",
            if n == 1 { "" } else { "s" }
        ),
        (_, n) => format!(
            "nothing was read: {n} page{} with no text layer and no image \
             (blank, drawn as vector outlines, or damaged)",
            if n == 1 { "" } else { "s" }
        ),
    })
}

/// Take the crops the analysis asks for, have `read` read them, and splice
/// what it returned into that analysis.
///
/// The crops are the analysis's own anchors
/// ([`fluree_doc_pdf::escalate::crops_at_anchors`]), so each reading fills
/// the anchor its crop was named after; `extracted`, the document before
/// the analysis changed it, chooses the whole pages. A table the
/// `--structure-results` veto would keep deterministic whatever the model
/// read is left out.
///
/// The splice runs even when `read` returned nothing: the analysis carries an
/// anchor for every crop it could have asked for, and a crop goes unread when
/// `--pages` or `escalation.max_crops` leaves it out or the reader finds
/// nothing printed on it. The splice is what drops an anchor no reading
/// answers, so skipping it would print the bare token where nobody asked for
/// one. With `--emit-anchors` the splice keeps it, on purpose.
///
/// `read` is the reader `convert` calls, taken as an argument so a test can
/// stand one in without the network. It is handed the crops
/// [`crate::escalate::to_read`] leaves: those on the pages `pages` names,
/// within `escalation.max_crops`, ranked by the reasons they were chosen for.
#[allow(clippy::too_many_arguments)]
fn read_in_run(
    a: &mut fluree_doc_pdf::document::Analysis,
    extracted: &fluree_doc_pdf::Document,
    cfg: &TierConfig,
    stem: &str,
    page_text: &[Vec<String>],
    screens: &std::collections::HashSet<String>,
    pages: Option<&[usize]>,
    read: impl FnOnce(
        fluree_doc_pdf::escalate::CropJobs,
        &fluree_doc_pdf::escalate::CropHints,
    ) -> Result<fluree_doc_pdf::escalate::Readings, String>,
) -> Result<(), String> {
    let hints = crate::escalate::jobs::hints(extracted, stem, cfg);
    // A table the structure readings veto keeps its grid whatever the model
    // reads, so reading it would be paid for and thrown away, and take the
    // place of a crop under `escalation.max_crops`. One backend for the
    // choice and the splice, which read the same structure files.
    let structure = common::structure_backend(cfg.structure_results.as_deref());
    let structure = structure
        .as_ref()
        .map(|s| s as &dyn fluree_doc_pdf::arbiter::TierBackend);
    let (jobs, reasons) = crate::escalate::jobs::at_anchors(extracted, a, stem, &hints, structure);
    let jobs = crate::escalate::to_read(
        std::path::Path::new(stem),
        jobs,
        &reasons,
        &cfg.config,
        pages,
    );
    let readings = read(jobs, &hints)?;
    common::splice(
        cfg.emit_anchors,
        &mut a.elements,
        stem,
        &readings,
        structure,
        page_text,
        &a.furniture,
        screens,
    );
    Ok(())
}

/// A name per input for its outputs in one run: its stem, unless another
/// input shares it, then with its source extension (`report.docx`), then
/// numbered (`report.md (2)`) where even that repeats, as `a/report.md` and
/// `b/report.md` do. Allocated before any conversion starts, so no two
/// inputs write one file or one attachment directory. Compared without
/// case, as the file systems that ignore it would.
fn slots(files: &[PathBuf]) -> Vec<String> {
    let mut stems: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for f in files {
        *stems.entry(common::stem_of(f).to_lowercase()).or_default() += 1;
    }
    let mut taken = std::collections::HashSet::new();
    files
        .iter()
        .map(|f| {
            let stem = common::stem_of(f);
            let base = match f.extension().and_then(|x| x.to_str()) {
                Some(src) if stems[&stem.to_lowercase()] > 1 => format!("{stem}.{src}"),
                _ => stem.to_string(),
            };
            let mut slot = base.clone();
            let mut n = 2;
            while !taken.insert(slot.to_lowercase()) {
                slot = format!("{base} ({n})");
                n += 1;
            }
            slot
        })
        .collect()
}

fn write_out(out: &str, dst: Option<&Path>) -> i32 {
    match dst {
        Some(p) => {
            if let Err(e) = std::fs::write(p, out) {
                eprintln!("error: writing {}: {e}", p.display());
                return 1;
            }
            0
        }
        None => {
            print!("{out}");
            0
        }
    }
}

fn ext(format: Format) -> &'static str {
    match format {
        Format::Md => "md",
        Format::Xhtml => "xhtml",
        Format::Json => "json",
        Format::Doco => "jsonld",
        Format::Text => "txt",
        Format::Conform => "conform.json",
    }
}

/// Parse a 1-based page-range list (`3`, `1-5`, `1,4,9-12`) into 0-based
/// page indices.
pub(crate) fn parse_pages(spec: &str) -> Result<Vec<usize>, String> {
    let mut out = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (lo, hi) = match part.split_once('-') {
            Some((a, b)) => (parse_page(a)?, parse_page(b)?),
            None => {
                let p = parse_page(part)?;
                (p, p)
            }
        };
        if lo > hi {
            return Err(format!("range '{part}' is descending"));
        }
        out.extend(lo - 1..hi);
    }
    if out.is_empty() {
        return Err("no pages selected".into());
    }
    Ok(out)
}

fn parse_page(s: &str) -> Result<usize, String> {
    match s.trim().parse::<usize>() {
        Ok(0) => Err("pages are 1-based".into()),
        Ok(n) => Ok(n),
        Err(_) => Err(format!("'{s}' is not a page number")),
    }
}

// The bench harness compatibility contract: `fdoc md <pdf>` must emit exactly
// what `convert <pdf>` emits, environment tiers applied, no page filter.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_batch_runs_no_more_documents_at_once_than_memory_holds() {
        use fluree_doc_model::budget::MAX_MEMORY;
        // Room for three documents: -j 8 and one per core of 12 lowered.
        let doc = document_memory(0);
        let three = Some(3 * doc + (doc >> 1));
        let (jobs, why) = batch_jobs(8, 12, three, 0, 0, None);
        assert_eq!(jobs, 3);
        assert!(why.unwrap().starts_with("-j 8 lowered to 3: "));
        let (jobs, why) = batch_jobs(0, 12, three, 0, 0, None);
        assert_eq!(jobs, 3);
        assert!(why.unwrap().starts_with("-j 0 (one per core) lowered to 3"));
        // Within it: as asked, unsaid.
        assert_eq!(batch_jobs(2, 12, three, 0, 0, None), (2, None));
        assert_eq!(batch_jobs(0, 2, three, 0, 0, None), (2, None));
        // A budget each is not room enough: what goes with it counts.
        assert_eq!(batch_jobs(4, 12, Some(3 * MAX_MEMORY), 0, 0, None).0, 2);
        // Less than one document's room: still one.
        assert_eq!(batch_jobs(4, 12, Some(1 << 30), 0, 0, None).0, 1);
        // A smaller memory limit makes room for more.
        let mut small = fluree_doc_model::budget::Limits::default();
        small.memory = MAX_MEMORY / 4;
        let more =
            fluree_doc_model::budget::limited_to(small, || batch_jobs(8, 12, three, 0, 0, None));
        assert_eq!(more.0, 7);
        // But what goes with a budget does not shrink with it: under a
        // budget of 1 MiB, 4 GiB free holds three documents, not 2,730.
        let mut tiny = fluree_doc_model::budget::Limits::default();
        tiny.memory = 1 << 20;
        let four = Some(4u64 << 30);
        let (jobs, why) =
            fluree_doc_model::budget::limited_to(tiny, || batch_jobs(64, 12, four, 0, 0, None));
        assert_eq!(jobs, 3);
        assert!(why
            .unwrap()
            .ends_with("4096 MiB of memory free, 1281 MiB a document"));
        // And a document holds its input whole: inputs of 400 MiB at the
        // least make it two.
        let (jobs, _) = fluree_doc_model::budget::limited_to(tiny, || {
            batch_jobs(64, 12, four, 400 << 20, 0, None)
        });
        assert_eq!(jobs, 2);
        assert_eq!(batch_jobs(1, 12, Some(0), 0, 0, None), (1, None));
        // Not known: one per core at most.
        assert_eq!(batch_jobs(0, 6, None, 0, 0, None), (6, None));
        assert_eq!(batch_jobs(16, 6, None, 0, 0, None).0, 6);
    }

    #[test]
    fn the_memory_free_on_macos_is_what_vm_stat_counts_free() {
        let report = "Mach Virtual Memory Statistics: (page size of 16384 bytes)\n\
            Pages free:                               10000.\n\
            Pages active:                            400000.\n\
            Pages inactive:                          200000.\n\
            Pages speculative:                         5000.\n\
            Pages throttled:                              0.\n\
            Pages wired down:                        150000.\n\
            Pages purgeable:                          20000.\n\
            \"Translation faults\":                 123456789.\n";
        assert_eq!(vm_stat_available(report), Some(215_000 * 16384));
        assert_eq!(vm_stat_available("Pages free: 10.\n"), None);
        assert_eq!(
            vm_stat_available("(page size of 4096 bytes)\nPages active: 3.\n"),
            None
        );
    }

    #[test]
    fn a_batch_sets_aside_what_its_ocr_engine_may_take() {
        // Room for three documents and a half; an engine that may take one
        // document's worth leaves room for two.
        let doc = document_memory(0);
        let free = Some(3 * doc + (doc >> 1));
        let (jobs, why) = batch_jobs(8, 12, free, 0, doc, None);
        assert_eq!(jobs, 2);
        assert!(why.unwrap().contains(&format!(
            "MiB of memory free, {} MiB of it for OCR",
            doc >> 20
        )));
        // An engine that may take more than is free still leaves one, and
        // the note does not say it takes more than there is.
        let (jobs, why) = batch_jobs(8, 12, Some(doc), 0, 2 * doc, None);
        assert_eq!(jobs, 1);
        assert!(why.unwrap().ends_with(&format!(
            "{} MiB of memory free, less than the {} MiB OCR sets aside, {} MiB a document",
            doc >> 20,
            (2 * doc) >> 20,
            doc >> 20
        )));
        // Free memory not known: one per core, whatever the engine.
        assert_eq!(batch_jobs(0, 6, None, 0, doc, None), (6, None));
    }

    #[test]
    fn a_batch_can_be_told_what_a_document_takes() {
        let gib = 1u64 << 30;
        // An 8 GB container runs one document at the default, four at 1.5
        // GiB each.
        assert_eq!(batch_jobs(8, 12, Some(7 * gib), 0, 0, None).0, 1);
        let (jobs, why) = batch_jobs(8, 12, Some(7 * gib), 0, 0, Some(3 * gib / 2));
        assert_eq!(jobs, 4);
        assert!(why.unwrap().ends_with("1536 MiB a document"));
        // Told, a document still holds its input beside it, and the OCR
        // engine's memory is still set aside.
        assert_eq!(job_need(400 << 20, Some(gib)), gib + (400 << 20));
        assert_eq!(batch_jobs(8, 12, Some(7 * gib), 0, gib, Some(gib)).0, 6);
        assert_eq!(batch_jobs(8, 12, Some(7 * gib), gib, 0, Some(gib)).0, 3,);
        // Free memory not known: told, the batch runs as many as asked.
        assert_eq!(batch_jobs(32, 8, None, 0, 0, Some(gib)), (32, None));
        // The flag over the environment, in MiB; 0 and nonsense refused.
        assert_eq!(job_memory(None, None), Ok(None));
        assert_eq!(job_memory(None, Some(" 512 ")), Ok(Some(512 << 20)));
        assert_eq!(job_memory(Some(256), Some("512")), Ok(Some(256 << 20)));
        assert!(job_memory(Some(0), None).is_err());
        assert!(job_memory(None, Some("1G")).is_err());
    }

    #[test]
    fn free_memory_is_read_from_proc() {
        let meminfo =
            "MemTotal:       65536000 kB\nMemFree:  1000 kB\nMemAvailable:   48000000 kB\n";
        assert_eq!(mem_available(meminfo), Some(48_000_000 * 1024));
        assert_eq!(mem_available("MemTotal: 1 kB\n"), None);
        let limits = "Limit                     Soft Limit           Hard Limit           Units\n\
                      Max stack size            8388608              unlimited            bytes\n\
                      Max address space         4096000000           unlimited            bytes\n";
        assert_eq!(address_space(limits), Some(4_096_000_000));
        let unlimited =
            "Max address space         unlimited            unlimited            bytes\n";
        assert_eq!(address_space(unlimited), None);
    }

    #[test]
    fn a_batch_admits_each_document_by_its_own_size() {
        // Room for three small documents: a large input waits for room,
        // runs alone if it needs it, and leaves the others their `-j`.
        let small = document_memory(0);
        let room = Admission::new(Some(3 * small + (small >> 1)));
        let a = room.try_admit(small).unwrap();
        let b = room.try_admit(small).unwrap();
        let large = document_memory(2 << 30);
        assert!(room.try_admit(large).is_none());
        let c = room.try_admit(small).unwrap();
        assert!(room.try_admit(small).is_none());
        drop((a, b, c));
        // Alone, whatever it needs: past the room, nothing runs beside it.
        let l = room.try_admit(4 * large).unwrap();
        assert!(room.try_admit(1).is_none());
        drop(l);
        assert!(room.try_admit(small).is_some());
        // The memory not known: every document at once.
        let unknown = Admission::new(None);
        let held: Vec<_> = (0..10).map(|_| unknown.try_admit(large).unwrap()).collect();
        assert_eq!(held.len(), 10);
        // A worker waiting is woken as one before it finishes.
        let room = Admission::new(Some(small));
        let first = room.admit(small);
        std::thread::scope(|s| {
            let waiting = s.spawn(|| drop(room.admit(small)));
            std::thread::sleep(std::time::Duration::from_millis(20));
            drop(first);
            waiting.join().unwrap();
        });
        // And the workers are counted at the smallest input: one 2 GiB file
        // among small ones lowers no `-j`.
        let free = Some(3 * small + (small >> 1));
        assert_eq!(batch_jobs(3, 12, free, 0, 0, None), (3, None));
    }

    #[test]
    fn a_page_limit_past_the_trees_bound_is_counted_in_a_documents_memory() {
        let mut l = fluree_doc_model::budget::Limits::default();
        l.pages = 1_250_000;
        let more = fluree_doc_model::budget::limited_to(l, || document_memory(0));
        assert_eq!(
            more - document_memory(0),
            1_000_000 * fluree_doc_pdf::page_tree::PAGE_TREE_PAGE_BYTES
        );
    }

    #[test]
    fn a_control_groups_limit_bounds_the_memory_free() {
        use std::collections::HashMap;
        let fs = |files: &[(&str, &str)]| {
            let files: HashMap<String, String> = files
                .iter()
                .map(|(p, c)| (p.to_string(), c.to_string()))
                .collect();
            move |p: &str| files.get(p).cloned()
        };
        // cgroup v2: the group's limit less its use, and a group above with
        // less left wins; the root has no limit, a group may say `max`.
        let v2 = "0::/user.slice/batch.scope\n";
        let read = fs(&[
            (
                "/sys/fs/cgroup/user.slice/batch.scope/memory.max",
                "3221225472\n",
            ),
            (
                "/sys/fs/cgroup/user.slice/batch.scope/memory.current",
                "221225472\n",
            ),
            ("/sys/fs/cgroup/user.slice/memory.max", "max\n"),
            ("/sys/fs/cgroup/user.slice/memory.current", "9000000000\n"),
        ]);
        assert_eq!(cgroup_headroom(v2, &read), Some(3_000_000_000));
        let read = fs(&[
            (
                "/sys/fs/cgroup/user.slice/batch.scope/memory.max",
                "3221225472\n",
            ),
            (
                "/sys/fs/cgroup/user.slice/batch.scope/memory.current",
                "221225472\n",
            ),
            ("/sys/fs/cgroup/user.slice/memory.max", "2000000000\n"),
            ("/sys/fs/cgroup/user.slice/memory.current", "1500000000\n"),
        ]);
        assert_eq!(cgroup_headroom(v2, &read), Some(500_000_000));
        // A container: its group at the mount's root, its path the host's.
        let container = "0::/system.slice/docker-1234.scope\n";
        let read = fs(&[
            ("/sys/fs/cgroup/memory.max", "3000000000\n"),
            ("/sys/fs/cgroup/memory.current", "1000000000\n"),
        ]);
        assert_eq!(cgroup_headroom(container, &read), Some(2_000_000_000));
        // Over its limit (pages it may reclaim): nothing left, not a wrap.
        let read = fs(&[
            ("/sys/fs/cgroup/memory.max", "1000\n"),
            ("/sys/fs/cgroup/memory.current", "2000\n"),
        ]);
        assert_eq!(cgroup_headroom("0::/\n", &read), Some(0));
        // Its file cache the kernel reclaims first is not counted as used:
        // 3.3 GB of a group's 3.4 GB is cache written and let go.
        let stat_v2 = "anon 90000000\nfile 3310000000\nkernel 4000000\n\
                       active_anon 80000000\ninactive_anon 0\n\
                       active_file 10000000\ninactive_file 3300000000\n\
                       shmem 0\n";
        let read = fs(&[
            ("/sys/fs/cgroup/batch.scope/memory.max", "9000000000\n"),
            ("/sys/fs/cgroup/batch.scope/memory.current", "3400000000\n"),
            ("/sys/fs/cgroup/batch.scope/memory.stat", stat_v2),
        ]);
        assert_eq!(
            cgroup_headroom("0::/batch.scope\n", &read),
            Some(8_900_000_000)
        );
        // More cache than use, as the counters are read apart: all of it
        // free, not a wrap.
        let read = fs(&[
            ("/sys/fs/cgroup/memory.max", "1000\n"),
            ("/sys/fs/cgroup/memory.current", "200\n"),
            ("/sys/fs/cgroup/memory.stat", "inactive_file 300\n"),
        ]);
        assert_eq!(cgroup_headroom("0::/\n", &read), Some(1000));
        // cgroup v1: the memory controller's line, its huge "no limit"
        // beside a real one.
        let v1 = "12:pids:/docker/abc\n4:cpu,cpuacct:/docker/abc\n\
                  9:memory:/docker/abc\n1:name=systemd:/docker/abc\n";
        let read = fs(&[
            (
                "/sys/fs/cgroup/memory/docker/abc/memory.limit_in_bytes",
                "3000000000\n",
            ),
            (
                "/sys/fs/cgroup/memory/docker/abc/memory.usage_in_bytes",
                "500000000\n",
            ),
            (
                "/sys/fs/cgroup/memory/memory.limit_in_bytes",
                "9223372036854771712\n",
            ),
            (
                "/sys/fs/cgroup/memory/memory.usage_in_bytes",
                "8000000000\n",
            ),
        ]);
        assert_eq!(cgroup_headroom(v1, &read), Some(2_500_000_000));
        // v1's cache is its hierarchy's, `total_inactive_file`; the group's
        // own `inactive_file` beside it is not what its use counts.
        let stat_v1 = "cache 2100000000\nrss 400000000\ninactive_file 5\n\
                       total_cache 2100000000\ntotal_rss 400000000\n\
                       total_inactive_file 2000000000\n";
        let read = fs(&[
            (
                "/sys/fs/cgroup/memory/docker/abc/memory.limit_in_bytes",
                "3000000000\n",
            ),
            (
                "/sys/fs/cgroup/memory/docker/abc/memory.usage_in_bytes",
                "2500000000\n",
            ),
            ("/sys/fs/cgroup/memory/docker/abc/memory.stat", stat_v1),
        ]);
        assert_eq!(cgroup_headroom(v1, &read), Some(2_500_000_000));
        // No limit anywhere, or no group: not known.
        let read = fs(&[
            ("/sys/fs/cgroup/memory.max", "max\n"),
            ("/sys/fs/cgroup/memory.current", "1000\n"),
        ]);
        assert_eq!(cgroup_headroom("0::/\n", &read), None);
        assert_eq!(cgroup_headroom("", &read), None);
        // A container of 3 GB on a host with 23 GB available: the
        // container's, and no more jobs than it leaves room for.
        let read = fs(&[
            ("/proc/meminfo", "MemAvailable:   23000000 kB\n"),
            ("/proc/self/cgroup", container),
            ("/sys/fs/cgroup/memory.max", "3000000000\n"),
            ("/sys/fs/cgroup/memory.current", "1000000\n"),
        ]);
        let free = free_memory_in(&read);
        assert_eq!(free, Some(2_999_000_000));
        let (jobs, why) = batch_jobs(4, 12, free, 0, 0, None);
        assert_eq!(jobs, 1);
        assert!(why.unwrap().contains("2860 MiB of memory free"));
        // Under an address space smaller still, that.
        let read = fs(&[
            ("/proc/meminfo", "MemAvailable:   23000000 kB\n"),
            ("/proc/self/cgroup", container),
            ("/sys/fs/cgroup/memory.max", "3000000000\n"),
            ("/sys/fs/cgroup/memory.current", "1000000\n"),
            (
                "/proc/self/limits",
                "Max address space         2000000000           unlimited            bytes\n",
            ),
        ]);
        assert_eq!(free_memory_in(&read), Some(2_000_000_000));
    }

    #[test]
    fn an_emails_files_share_what_the_message_leaves() {
        use fluree_doc_model::budget::{
            text_cost, ELEMENT_BYTES, MAX_MEMORY, PART_BYTES, TEXT_BYTES,
        };
        let mail = "From: Alex Sample <alex@example.com>\r\nSubject: Orders\r\nMIME-Version: 1.0\r\n\
                    Content-Type: multipart/mixed; boundary=\"b\"\r\n\r\n\
                    --b\r\nContent-Type: text/plain\r\n\r\nSee attached.\r\n\
                    --b\r\nContent-Type: text/csv\r\nContent-Disposition: attachment; filename=\"orders.csv\"\r\n\r\n\
                    Customer,Amount\r\nAlex Sample,12.50\r\n--b--\r\n";
        let email = fluree_doc_email::parse(mail.as_bytes()).unwrap();
        let files: u64 = email.attachments.iter().map(|a| a.bytes.len() as u64).sum();
        assert!(files > 0);
        let text: u64 = email.elements.iter().map(|e| text_cost(&e.text)).sum();
        let held =
            email.elements.len() as u64 * ELEMENT_BYTES + text * TEXT_BYTES + files * PART_BYTES;
        assert_eq!(attachments_share(&email), MAX_MEMORY - held);
        // A share already given bounds it too.
        assert_eq!(
            fluree_doc_model::budget::within(held, || attachments_share(&email)),
            0
        );
        // The walk reads each file within that share, and so opens an
        // attached message within it too.
        let mut left = Vec::new();
        walk_carried(&email, &mut |_: &fluree_doc_email::AttachedFile, _| {
            left.push(fluree_doc_model::budget::Budget::default().memory_left());
            Some(Ok(()))
        });
        assert_eq!(left, [MAX_MEMORY - held]);
    }

    #[test]
    fn an_attachment_cannot_take_a_conversions_name() {
        let sha = fluree_doc_model::sha256_hex(b"invoice");
        let mut taken = std::collections::HashSet::new();
        for (name, saved) in [
            (format!("{sha}.md"), format!("_{sha}.md")),
            (
                format!("{}.json", sha.to_uppercase()),
                format!("_{}.json", sha.to_uppercase()),
            ),
            (sha.clone(), format!("_{sha}")),
            (format!("x/{sha}.md"), format!("_{sha} (2).md")),
            // Not 64 digits before the dot: the sender's name, as given.
            (format!("{}.md", &sha[..63]), format!("{}.md", &sha[..63])),
            (format!("{sha}0.md"), format!("{sha}0.md")),
            ("invoice.pdf".into(), "invoice.pdf".into()),
        ] {
            assert_eq!(file_name(Some(&name), 0, &mut taken), saved, "{name}");
        }
    }

    #[test]
    fn an_attachments_name_is_one_a_file_system_takes() {
        let mut taken = std::collections::HashSet::new();
        // A NUL, which no file system takes, failed the whole input.
        assert_eq!(
            file_name(Some("orders\0.csv"), 0, &mut taken),
            "orders_.csv"
        );
        assert_eq!(file_name(Some("a\nb.txt"), 1, &mut taken), "a_b.txt");
        // A name past what a file system takes is cut, its extension kept.
        let long = format!("{}.pdf", "é".repeat(400));
        let saved = file_name(Some(&long), 2, &mut taken);
        assert!(saved.len() <= MAX_FILE_NAME_BYTES, "{}", saved.len());
        assert!(saved.ends_with("é.pdf"), "{saved}");
        let dir = std::env::temp_dir().join(format!("fdoc-names-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["orders_.csv", "a_b.txt", saved.as_str()] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn parser_warnings_read_as_one_line_a_kind() {
        let w = |page, kind: &str| fluree_doc_model::ParseWarning {
            page,
            kind: kind.into(),
            count: 1,
        };
        let lines = parse_warning_lines(&[
            w(2, "ImageDecodeFailure"),
            w(0, "UnsupportedFont"),
            w(4, "ImageDecodeFailure"),
        ]);
        assert_eq!(
            lines,
            [
                "ImageDecodeFailure on pages 3, 5: an image could not be decoded; what it shows is missing",
                "UnsupportedFont on page 1: the parser reported a problem",
            ]
        );
        // A long document names its first pages, and counts the rest.
        let many: Vec<_> = (0..13).map(|p| w(p, "ImageDecodeFailure")).collect();
        let line = &parse_warning_lines(&many)[0];
        assert!(
            line.contains("pages 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, and 3 more:"),
            "{line}"
        );
        assert!(parse_warning_lines(&[]).is_empty());
    }

    #[test]
    fn an_image_the_model_found_no_text_in_was_read() {
        let mut placed = vec![
            Placed::new("m1".into(), None),
            Placed::new("m2".into(), None),
        ];
        for p in &mut placed {
            p.send = Some("image/png");
        }
        let got = Ok(crate::escalate::ImagesRead {
            empty: vec!["m1".into()],
            failures: vec![("m2".into(), "refused".into())],
            ..Default::default()
        });
        settle(
            "report",
            placed.iter_mut(),
            &got,
            &mut fluree_doc_model::budget::Budget::default(),
            false,
            true,
        );
        assert!(placed[0].read.as_ref().is_some_and(Vec::is_empty));
        assert!(placed[0].failure.is_none());
        assert!(placed[1].read.is_none());
        assert_eq!(placed[1].failure, Some(("Failed", "refused".into())));
        // A reading that says, in prose, that the image holds no text has
        // nothing to place, and read it all the same.
        let mut placed = [Placed::new("m1".into(), None)];
        placed[0].send = Some("image/png");
        let mut readings = fluree_doc_pdf::escalate::Readings::default();
        readings.insert(
            "m1".into(),
            "The image does not contain any printed text to transcribe.".into(),
        );
        let got = Ok(crate::escalate::ImagesRead {
            readings,
            ..Default::default()
        });
        settle(
            "report",
            placed.iter_mut(),
            &got,
            &mut fluree_doc_model::budget::Budget::default(),
            false,
            true,
        );
        assert!(placed[0].read.as_ref().is_some_and(Vec::is_empty));
        assert!(placed[0].failure.is_none());
    }

    #[test]
    fn an_image_whose_header_does_not_parse_is_charged_and_never_sent() {
        use fluree_doc_model::PictureReader;
        // A PNG's signature, then zeros: it opens like an image, and no
        // header says what it holds. Charged a pixel a byte, so a document
        // of many is bounded by their bytes; declined for the model before
        // the probe decodes it.
        let fake = [&b"\x89PNG\r\n\x1a\n"[..], &[0u8; 4096]].concat();
        let cfg = TierConfig {
            escalate: true,
            ..Default::default()
        };
        let mut head = |_: &str| Ok(Vec::new());
        let mut budget = fluree_doc_model::budget::Budget::default();
        let mut reader = MediaReader {
            stem: "report",
            cfg: &cfg,
            head: &mut head,
            given: None,
            asked: 0,
            over_budget: 0,
            verdicts: HashMap::new(),
            budget: &mut budget,
            quiet: true,
        };
        assert_eq!(reader.pixels(&fake), Ok(fake.len() as u64));
        let declined = declined_by_model(&fake, || panic!("probed"));
        assert!(
            declined
                .as_ref()
                .is_some_and(|(why, e)| *why == "Failed" && e.contains("header does not parse")),
            "{declined:?}"
        );
        // Read as a picture of the document: not sent, and why.
        let picture = fluree_doc_model::Picture {
            source: fluree_doc_model::MediaSource::Part("word/media/image1.png".into()),
            uses: vec![fluree_doc_model::Media {
                figure: "figure-0-1".into(),
                source: fluree_doc_model::MediaSource::Part("word/media/image1.png".into()),
                index: 0,
            }],
        };
        let p = reader.read(&picture, &fake).unwrap();
        assert!(p.send.is_none());
        assert_eq!(p.failure.map(|f| f.0), Some("Failed"));
        assert_eq!(reader.asked, 0);
    }

    #[test]
    fn the_engine_reads_a_picture_the_probe_does_not_take_for_text_and_no_model_is_sent_it() {
        use fluree_doc_model::PictureReader;
        /// An engine that counts what it is asked to read, and reads nothing.
        struct Counting(std::sync::atomic::AtomicUsize);
        impl fluree_doc_vision::OcrEngine for Counting {
            fn name(&self) -> &str {
                "counting"
            }
            fn read(
                &self,
                image: &fluree_doc_vision::Pixels,
            ) -> Result<fluree_doc_vision::WordReading, String> {
                self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Ok(fluree_doc_vision::WordReading {
                    coords: fluree_doc_vision::Coords::Px,
                    width: Some(f64::from(image.width)),
                    height: Some(f64::from(image.height)),
                    engine: Some("counting".into()),
                    words: Vec::new(),
                    rules: Vec::new(),
                })
            }
        }
        // A flat grey picture over the model's floor: no text, to the probe.
        let grey = {
            let img = image::RgbImage::from_pixel(600, 400, image::Rgb([200, 200, 200]));
            let mut out = Vec::new();
            image::DynamicImage::ImageRgb8(img)
                .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
                .unwrap();
            out
        };
        assert_eq!(fluree_doc_image::looks_like_text(&grey), Some(false));
        let engine = std::sync::Arc::new(Counting(Default::default()));
        let cfg = TierConfig {
            escalate: true,
            ocr: Some(common::OcrHandle(engine.clone())),
            ..Default::default()
        };
        let mut head = |_: &str| Ok(Vec::new());
        let mut budget = fluree_doc_model::budget::Budget::default();
        let mut reader = MediaReader {
            stem: "report",
            cfg: &cfg,
            head: &mut head,
            given: None,
            asked: 0,
            over_budget: 0,
            verdicts: HashMap::new(),
            budget: &mut budget,
            quiet: true,
        };
        let source = fluree_doc_model::MediaSource::Part("word/media/image1.png".into());
        let picture = fluree_doc_model::Picture {
            source: source.clone(),
            uses: vec![fluree_doc_model::Media {
                figure: "figure-0-1".into(),
                source,
                index: 0,
            }],
        };
        let p = reader.read(&picture, &grey).unwrap();
        // OCR read it, as it read every picture before the probe judged
        // them; it found no text, and the probe keeps the model's call.
        assert_eq!(engine.0.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert!(p.read.as_ref().is_some_and(Vec::is_empty));
        assert!(p.send.is_none());
        assert_eq!(p.failure.map(|f| f.0), Some("NotTexty"));
    }

    #[test]
    fn colliding_stems_get_disambiguated() {
        let slots = |names: &[&str]| slots(&names.iter().map(PathBuf::from).collect::<Vec<_>>());
        // Five sources named `demo` wrote one file and silently lost four.
        assert_eq!(
            slots(&["demo.md", "demo.docx", "report.pdf"]),
            ["demo.md", "demo.docx", "report"]
        );
        // The same name in two directories, and a name differing in case,
        // which a case-insensitive file system holds as one.
        assert_eq!(
            slots(&["a/report.md", "b/report.md", "c/Report.md"]),
            ["report.md", "report.md (2)", "Report.md (3)"]
        );
        // A plain stem that a disambiguated name already took.
        assert_eq!(
            slots(&["x/report.md", "y/report.md", "report.md.txt"]),
            ["report.md", "report.md (2)", "report.md (3)"]
        );
    }

    #[test]
    fn a_flag_naming_one_document_refuses_several_inputs() {
        use clap::Parser;
        let convert = |extra: &[&str]| {
            let argv = ["fdoc", "convert", "a.md", "b.md", "--out-dir", "out"];
            let cli = crate::cli::Cli::try_parse_from(argv.iter().chain(extra)).unwrap();
            match cli.command {
                crate::cli::Commands::Convert(args) => args,
                _ => unreachable!(),
            }
        };
        for flag in ["--doc-iri", "--base-iri", "--source-name"] {
            let args = convert(&[flag, "urn:doc:a"]);
            assert_eq!(single_input_flag(&args), Some(flag));
            assert_eq!(run(&args, false, true), 2, "{flag}");
        }
        assert_eq!(single_input_flag(&convert(&[])), None);
    }

    /// A text page with a raster table the glyph layer cannot read: prose
    /// above and below, a texty image between. The router asks for that
    /// region alone, which is what a live reader is then paid to read.
    fn page_with_a_raster_region() -> fluree_doc_pdf::Document {
        let mut doc = fluree_doc_pdf::image::page_of(600, 800, false);
        let page = &mut doc.pages[0];
        page.images[0].bbox = fluree_doc_pdf::BBox {
            x0: 50.0,
            y0: 300.0,
            x1: 550.0,
            y1: 550.0,
        };
        let line = "The figures below are printed as an image of a table.";
        let baselines = (0..14)
            .map(|i| 60.0 + 14.0 * f64::from(i))
            .chain((0..14).map(|i| 580.0 + 14.0 * f64::from(i)));
        for y in baselines {
            for (i, c) in line.chars().enumerate() {
                let x = 50.0 + 5.0 * i as f64;
                let draw_index = page.glyphs.len();
                page.glyphs.push(fluree_doc_pdf::Glyph {
                    text: c.to_string(),
                    bbox: (c != ' ').then_some(fluree_doc_pdf::BBox {
                        x0: x,
                        y0: y - 8.0,
                        x1: x + 5.0,
                        y1: y,
                    }),
                    page: 0,
                    origin: (x, y),
                    rotation_deg: 0.0,
                    font_size: 10.0,
                    advance: Some(5.0),
                    weight: None,
                    italic: false,
                    draw_index,
                    ocr: None,
                });
            }
        }
        doc
    }

    /// `convert --escalate` with a configured reader and no sidecars, with a
    /// stand-in for the network: the options it analyses with, then
    /// [`read_in_run`] as `convert_bytes` calls it, with a reader answering
    /// the crop it is asked for with `answer`, or with nothing.
    fn convert_live(emit_anchors: bool, answer: Option<&str>) -> Vec<Element> {
        let cfg = TierConfig {
            escalate: true,
            emit_anchors,
            ..Default::default()
        };
        let extracted = page_with_a_raster_region();
        let mut doc = extracted.clone();
        let mut a = fluree_doc_pdf::document::analyze_with(&mut doc, &[], &cfg.options_for("doc"));
        let page_text = vec![fluree_doc_pdf::fidelity::page_lines(&doc.pages[0].glyphs)];
        read_in_run(
            &mut a,
            &extracted,
            &cfg,
            "doc",
            &page_text,
            &Default::default(),
            None,
            |jobs, _| {
                let crops: Vec<String> = jobs
                    .iter()
                    .flat_map(|(page, regions)| {
                        regions
                            .iter()
                            .flatten()
                            .map(move |(tag, _)| format!("p{page}_{tag}"))
                    })
                    .collect();
                assert_eq!(crops, ["p0_r0"], "the region alone is read");
                let mut readings = fluree_doc_pdf::escalate::Readings::default();
                if let Some(text) = answer {
                    readings.insert(crops[0].clone(), text.into());
                }
                Ok(readings)
            },
        )
        .expect("a stand-in reader does not fail");
        a.elements
    }

    /// A page of prose with two raster regions, A above B, and a label
    /// printed over A in faux bold: each of its ten letters drawn twice at
    /// one spot. As extracted, A holds twenty glyphs and the glyph layer
    /// reads it; once the analysis drops the doubles it holds ten, and A is
    /// a region the router asks a reader for, as B is.
    fn page_with_faux_bold_over_a_region() -> fluree_doc_pdf::Document {
        let mut doc = page_with_a_raster_region();
        let page = &mut doc.pages[0];
        let (a, b) = (
            fluree_doc_pdf::BBox {
                x0: 50.0,
                y0: 260.0,
                x1: 550.0,
                y1: 400.0,
            },
            fluree_doc_pdf::BBox {
                x0: 50.0,
                y0: 420.0,
                x1: 550.0,
                y1: 560.0,
            },
        );
        page.images[0].bbox = a;
        let mut second = page.images[0];
        second.bbox = b;
        page.images.push(second);
        // Prose above and below the two regions only.
        page.glyphs.retain(|g| {
            let y = g.origin.1;
            !(a.y0..=b.y1).contains(&y)
        });
        for pass in 0..2 {
            for (i, c) in "LABELTEXTS".chars().enumerate() {
                let x = 200.0 + 6.0 * i as f64;
                let draw_index = page.glyphs.len();
                page.glyphs.push(fluree_doc_pdf::Glyph {
                    text: c.to_string(),
                    bbox: Some(fluree_doc_pdf::BBox {
                        x0: x + 0.1 * f64::from(pass),
                        y0: 322.0,
                        x1: x + 5.0 + 0.1 * f64::from(pass),
                        y1: 330.0,
                    }),
                    page: 0,
                    origin: (x + 0.1 * f64::from(pass), 330.0),
                    rotation_deg: 0.0,
                    font_size: 10.0,
                    advance: Some(6.0),
                    weight: None,
                    italic: false,
                    draw_index,
                    ocr: None,
                });
            }
        }
        doc
    }

    /// A reading lands where the crop it was read from sits. The crops a
    /// live run sends and the anchors its analysis mints are one set: a
    /// region the analysis counts the router out of as extracted is still
    /// asked for, and every reading is filed under the anchor of its own
    /// crop, not under a neighbour's that took its index.
    /// An OCR reading names its region as the analysis's anchors do: a
    /// label in faux bold over a capture covers it as drawn and not once
    /// cleaned, and both count it.
    #[test]
    fn an_ocr_reading_and_an_anchor_name_a_region_alike() {
        let doc = page_with_faux_bold_over_a_region();
        let regions = match &fluree_doc_pdf::document::region_routes(&doc)[0] {
            fluree_doc_pdf::route::Route::VlmRegions(r) => r.clone(),
            other => panic!("{other:?}"),
        };
        let mut analysed = doc.clone();
        let a = fluree_doc_pdf::document::analyze_with(
            &mut analysed,
            &[],
            &fluree_doc_pdf::document::AnalyzeOptions {
                emit_anchors: true,
                ..Default::default()
            },
        );
        let anchors: Vec<(String, f64)> = a
            .elements
            .iter()
            .filter(|e| e.evidence == "route")
            .map(|e| (e.text.clone(), e.rect().y0))
            .collect();
        assert_eq!(
            anchors,
            regions
                .iter()
                .enumerate()
                .map(|(i, b)| (format!("[[VLM:p0:r{i}]]"), b.y0))
                .collect::<Vec<_>>()
        );
        assert_eq!(anchors.len(), 2, "{anchors:?}");
        // A reading of the second region is looked up under its name.
        let dir = std::env::temp_dir().join(format!(
            "fdoc-ocr-names-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let words = r#"{"coords":"px","width":100,"height":100,"words":[{"text":"B","box":[10,10,20,20]}]}"#;
        std::fs::write(dir.join("doc_p0_r1.json"), words).unwrap();
        let read = common::ocr_readings(&dir, "doc", &doc, true);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].1, fluree_doc_pdf::ocr::Crop::Region(1));
    }

    #[test]
    fn a_live_region_reading_lands_at_its_own_crop() {
        let cfg = TierConfig {
            escalate: true,
            ..Default::default()
        };
        let extracted = page_with_faux_bold_over_a_region();
        let mut doc = extracted.clone();
        let mut a = fluree_doc_pdf::document::analyze_with(&mut doc, &[], &cfg.options_for("doc"));
        let page_text = vec![fluree_doc_pdf::fidelity::page_lines(&doc.pages[0].glyphs)];
        read_in_run(
            &mut a,
            &extracted,
            &cfg,
            "doc",
            &page_text,
            &Default::default(),
            None,
            |jobs, _| {
                let mut readings = fluree_doc_pdf::escalate::Readings::default();
                for (page, regions) in &jobs {
                    for (tag, bbox) in regions.iter().flatten() {
                        readings.insert(
                            format!("p{page}_{tag}"),
                            format!("Reading of the crop at y0={}.", bbox.y0),
                        );
                    }
                }
                Ok(readings)
            },
        )
        .expect("a stand-in reader does not fail");
        let read: Vec<(f64, &str)> = a
            .elements
            .iter()
            .filter(|e| e.provenance == "vlm")
            .map(|e| (e.rect().y0, e.text.as_str()))
            .collect();
        assert_eq!(
            read,
            [
                (260.0, "Reading of the crop at y0=260."),
                (420.0, "Reading of the crop at y0=420."),
            ]
        );
    }

    /// The crops `dev render-routed` writes for an external reader
    /// (`jobs::to_render`, from an analysis with anchors on, as that command
    /// makes it) are named after the anchor of `convert --tier-results` each
    /// fills, so a sidecar reading lands where its crop sits, as a live one
    /// does. So are the crops of the library's `plan`, for a caller that
    /// has only the extracted document.
    #[test]
    fn render_routed_names_its_crops_after_the_anchors_they_fill() {
        let cfg = TierConfig {
            tier_results: Some(PathBuf::from("readings")),
            ..Default::default()
        };
        let extracted = page_with_faux_bold_over_a_region();
        let hints = fluree_doc_pdf::escalate::CropHints::default();
        let rendered = {
            let options = fluree_doc_pdf::document::AnalyzeOptions {
                emit_anchors: true,
                ..TierConfig::default().options_for("doc")
            };
            let a = fluree_doc_pdf::document::analyze_with(&mut extracted.clone(), &[], &options);
            let (jobs, _, vetoed) =
                crate::escalate::jobs::to_render(&extracted, &a, "doc", &hints, None);
            assert!(vetoed.is_empty());
            jobs
        };
        let planned =
            fluree_doc_pdf::escalate::plan(&extracted, &[], &cfg.options_for("doc"), &hints);
        let mut doc = extracted.clone();
        let a = fluree_doc_pdf::document::analyze_with(&mut doc, &[], &cfg.options_for("doc"));
        let anchors: Vec<(String, f64)> = a
            .elements
            .iter()
            .filter(|e| e.evidence == "route")
            .map(|e| (e.text.clone(), e.rect().y0))
            .collect();
        assert_eq!(
            anchors,
            [
                ("[[VLM:p0:r0]]".into(), 260.0),
                ("[[VLM:p0:r1]]".into(), 420.0)
            ]
        );
        let crops = |jobs: &fluree_doc_pdf::escalate::CropJobs| -> Vec<(String, f64)> {
            jobs.iter()
                .flat_map(|(page, regions)| {
                    regions
                        .iter()
                        .flatten()
                        .map(move |(tag, bbox)| (format!("[[VLM:p{page}:{tag}]]"), bbox.y0))
                })
                .collect()
        };
        assert_eq!(crops(&rendered), anchors);
        assert_eq!(crops(&planned), anchors);
    }

    /// `triage` prices the crops a live run is about to pay for, from the
    /// same anchors: on a page where the analysis counts a region the page
    /// as extracted does not, it counts both regions, as the run reads both.
    #[test]
    fn triage_prices_the_crops_a_live_run_reads() {
        let cfg = TierConfig {
            escalate: true,
            ..Default::default()
        };
        let extracted = page_with_faux_bold_over_a_region();
        let mut doc = extracted.clone();
        let mut a = fluree_doc_pdf::document::analyze_with(&mut doc, &[], &cfg.options_for("doc"));
        let priced = crate::commands::triage::priced(&extracted, &a, "doc", &cfg).0;
        let mut read = 0;
        read_in_run(
            &mut a,
            &extracted,
            &cfg,
            "doc",
            &[],
            &Default::default(),
            None,
            |jobs, _| {
                read = fluree_doc_pdf::escalate::crop_count(&jobs);
                Ok(fluree_doc_pdf::escalate::Readings::default())
            },
        )
        .expect("a stand-in reader does not fail");
        assert_eq!((priced, read), (2, 2));
    }

    /// Two pages read whole: a scan, and before it a page that routes a
    /// region as extracted and reads as a scan once its faux-bold label
    /// counts once, so it is read whole in place of that region.
    fn a_scan_after_a_page_read_whole_for_its_region() -> fluree_doc_pdf::Document {
        let mut doc = page_with_a_raster_region();
        let mut scan = doc.pages[0].clone();
        scan.index = 1;
        scan.glyphs.clear();
        let page = &mut doc.pages[0];
        page.images[0].bbox = fluree_doc_pdf::BBox {
            x0: 0.0,
            y0: 0.0,
            x1: 600.0,
            y1: 500.0,
        };
        page.glyphs.clear();
        for line in 0..6 {
            for (i, c) in "LABELTEXTS".chars().enumerate() {
                for pass in 0..2 {
                    let x = 50.0 + 6.0 * i as f64 + 0.1 * f64::from(pass);
                    let y = 600.0 + 14.0 * f64::from(line);
                    let draw_index = page.glyphs.len();
                    page.glyphs.push(fluree_doc_pdf::Glyph {
                        text: c.to_string(),
                        bbox: Some(fluree_doc_pdf::BBox {
                            x0: x,
                            y0: y - 8.0,
                            x1: x + 5.0,
                            y1: y,
                        }),
                        page: 0,
                        origin: (x, y),
                        rotation_deg: 0.0,
                        font_size: 10.0,
                        advance: Some(6.0),
                        weight: None,
                        italic: false,
                        draw_index,
                        ocr: None,
                    });
                }
            }
        }
        doc.pages.push(scan);
        doc
    }

    /// Under `escalation.max_crops`, a page that routes a region as drawn and
    /// reads as a scan once its faux-bold label counts once ranks as the scan
    /// every reader takes it for: two scans, the earlier first.
    #[test]
    fn under_the_ceiling_a_page_that_reads_as_a_scan_once_cleaned_ranks_as_one() {
        let mut cfg = TierConfig {
            escalate: true,
            ..Default::default()
        };
        cfg.config.escalation.max_crops = 1;
        let extracted = a_scan_after_a_page_read_whole_for_its_region();
        let mut doc = extracted.clone();
        let mut a = fluree_doc_pdf::document::analyze_with(&mut doc, &[], &cfg.options_for("doc"));
        assert!(matches!(
            fluree_doc_pdf::route::decide(&extracted.pages[0]).0,
            fluree_doc_pdf::route::Route::VlmRegions(_)
        ));
        assert!(matches!(
            fluree_doc_pdf::route::decide(&doc.pages[0]).0,
            fluree_doc_pdf::route::Route::Vlm(_)
        ));
        let mut asked = Vec::new();
        read_in_run(
            &mut a,
            &extracted,
            &cfg,
            "doc",
            &[],
            &Default::default(),
            None,
            |jobs, _| {
                asked = jobs;
                Ok(fluree_doc_pdf::escalate::Readings::default())
            },
        )
        .expect("a stand-in reader does not fail");
        assert_eq!(asked, [(0, None)], "two scans, by place");
    }

    /// `dev render-routed` writes a reason for every whole page it writes:
    /// a page that reads as a scan once its label counts once gives that.
    #[test]
    fn render_routed_gives_a_page_that_reads_as_a_scan_that_reason() {
        let cfg = TierConfig::default();
        let extracted = a_scan_after_a_page_read_whole_for_its_region();
        let mut doc = extracted.clone();
        let a = fluree_doc_pdf::document::analyze_with(
            &mut doc,
            &[],
            &fluree_doc_pdf::document::AnalyzeOptions {
                emit_anchors: true,
                ..cfg.options_for("doc")
            },
        );
        let hints = crate::escalate::jobs::hints(&extracted, "doc", &cfg);
        let (jobs, reasons, _) =
            crate::escalate::jobs::to_render(&extracted, &a, "doc", &hints, None);
        assert_eq!(jobs, [(0, None), (1, None)]);
        let named: Vec<&str> = jobs
            .iter()
            .map(|(page, _)| reasons.get(page).expect("a reason").as_str())
            .collect();
        assert_eq!(named, ["scanned", "near_blank"]);
    }

    /// A page of prose in three panels under a full-width header: a page-wide
    /// projection finds one gutter of the two, so the page is in column
    /// doubt.
    fn page_in_column_doubt() -> fluree_doc_pdf::Document {
        let mut doc = page_with_a_raster_region();
        let page = &mut doc.pages[0];
        page.images.clear();
        page.glyphs.clear();
        let mut put = |x: f64, y: f64| {
            let draw_index = page.glyphs.len();
            page.glyphs.push(fluree_doc_pdf::Glyph {
                text: "x".into(),
                bbox: Some(fluree_doc_pdf::BBox {
                    x0: x,
                    y0: y,
                    x1: x + 9.0,
                    y1: y + 10.0,
                }),
                page: 0,
                origin: (x, y + 10.0),
                rotation_deg: 0.0,
                font_size: 10.0,
                advance: None,
                weight: None,
                italic: false,
                draw_index,
                ocr: None,
            });
        };
        for row in 0..7 {
            for i in 0..30 {
                put(50.0 + f64::from(i) * 10.0, 100.0 + f64::from(row) * 12.0);
            }
        }
        for i in 0..45 {
            put(50.0 + f64::from(i) * 10.0, 184.0);
        }
        for row in 0..14 {
            for x0 in [50.0, 250.0, 460.0] {
                for i in 0..12 {
                    put(x0 + f64::from(i) * 10.0, 220.0 + f64::from(row) * 12.0);
                }
            }
        }
        doc
    }

    /// `triage` reads the configuration a live run reads: a page in column
    /// doubt is read whole when `escalation.on_column_doubt` says so, or
    /// `FDOC_ESCALATE_COLUMNS` over it, and counted then, and neither
    /// otherwise.
    #[test]
    fn triage_prices_column_doubt_as_the_configuration_has_it() {
        let counts = |on_column_doubt: bool, columns: Option<bool>| {
            let mut cfg = TierConfig {
                escalate: true,
                columns,
                ..Default::default()
            };
            cfg.config.escalation.on_column_doubt = on_column_doubt;
            let extracted = page_in_column_doubt();
            let mut doc = extracted.clone();
            let mut a =
                fluree_doc_pdf::document::analyze_with(&mut doc, &[], &cfg.options_for("doc"));
            assert!(
                fluree_doc_pdf::document::column_doubt(&extracted.pages[0], &a).is_some(),
                "the page is in column doubt"
            );
            let priced = crate::commands::triage::priced(&extracted, &a, "doc", &cfg).0;
            let mut read = 0;
            read_in_run(
                &mut a,
                &extracted,
                &cfg,
                "doc",
                &[],
                &Default::default(),
                None,
                |jobs, _| {
                    read = fluree_doc_pdf::escalate::crop_count(&jobs);
                    Ok(fluree_doc_pdf::escalate::Readings::default())
                },
            )
            .expect("a stand-in reader does not fail");
            (priced, read)
        };
        // `TierConfig` holds the run's override; the default one has none,
        // whatever this process's environment says.
        assert_eq!(counts(true, None), (1, 1));
        assert_eq!(counts(false, None), (0, 0));
        assert_eq!(counts(true, Some(false)), (0, 0));
        assert_eq!(counts(false, Some(true)), (1, 1));
    }

    /// A crop the reader found nothing on was read: asked for anchors, the
    /// run still drops its anchor, as no one is left to fill it.
    #[test]
    fn asked_for_anchors_a_crop_found_empty_leaves_none() {
        let elements = convert_live(true, Some(""));
        assert!(elements.iter().all(|e| !e.text.contains("[[VLM")));
        assert!(elements.iter().all(|e| e.provenance != "vlm"));
    }

    #[test]
    fn a_region_a_live_reader_reads_lands_in_the_output() {
        let elements = convert_live(false, Some("Revenue rose 12% to 4.2 billion."));
        let read: Vec<&Element> = elements.iter().filter(|e| e.provenance == "vlm").collect();
        assert_eq!(read.len(), 1, "the reading was paid for and must be used");
        assert_eq!(read[0].text, "Revenue rose 12% to 4.2 billion.");
        assert!(elements.iter().all(|e| !e.text.contains("[[VLM")));
    }

    #[test]
    fn a_region_no_reading_answers_leaves_no_anchor_behind() {
        let elements = convert_live(false, None);
        assert!(!elements.is_empty());
        assert!(
            elements.iter().all(|e| !e.text.contains("[[VLM")),
            "an unanswered anchor must drop, not print"
        );
    }

    /// Asked for anchors, a run that reads some of them itself leaves the
    /// rest for whoever fills anchors: what it read lands, what it did not
    /// stays marked.
    #[test]
    fn asked_for_anchors_a_live_run_leaves_the_unanswered_ones() {
        let unanswered = convert_live(true, None);
        let anchors: Vec<&str> = unanswered
            .iter()
            .filter(|e| e.text.contains("[[VLM"))
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(anchors, ["[[VLM:p0:r0]]"]);
        let answered = convert_live(true, Some("Revenue rose 12% to 4.2 billion."));
        assert!(answered.iter().all(|e| !e.text.contains("[[VLM")));
        assert!(answered.iter().any(|e| e.provenance == "vlm"));
    }

    /// The prose of [`page_with_a_raster_region`] without its image: a page
    /// nothing routes, whose crops are those of the anchors its analysis is
    /// given.
    fn page_of_prose() -> fluree_doc_pdf::Document {
        let mut doc = page_with_a_raster_region();
        doc.pages[0].images.clear();
        doc
    }

    /// A table anchor and the 2x2 grid it labels.
    fn an_anchored_grid() -> Vec<Element> {
        vec![
            Element::new(
                "doco:Figure",
                "[[VLMTAB:p0:t0]]",
                "rust",
                "table-confidence",
            ),
            Element {
                cells: Some(vec![
                    vec!["12".into(), "34".into()],
                    vec!["56".into(), "78".into()],
                ]),
                ..Element::new("doco:Table", "12 34 56 78", "rust", "grid")
            },
        ]
    }

    /// A deep reading of that table, of its own shape: unopposed, it wins.
    const DEEP_READING: &str =
        "<table><tr><td>12</td><td>34</td></tr><tr><td>56</td><td>78</td></tr></table>";

    /// A directory holding `doc_p0_t0.json`, a reading of one table block
    /// `html`, in the sidecar format; removed with the returned guard.
    struct Sidecars(PathBuf);

    impl Sidecars {
        fn holding(kind: &str, html: &str) -> Self {
            // Unique per test, not per clock tick: tests run in parallel, and
            // a clock that ticks in microseconds gave two the same name.
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("fdoc-{kind}-{}-{n}", std::process::id()));
            std::fs::create_dir(&dir).unwrap();
            let block = serde_json::json!([{"parsing_res_list": [
                {"block_label": "table", "block_content": html}
            ]}]);
            std::fs::write(dir.join("doc_p0_t0.json"), block.to_string()).unwrap();
            Self(dir)
        }
    }

    impl Drop for Sidecars {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The same two rows with one column fewer than the grid: the structure
    /// reading that corroborates it.
    fn corroborating_structure() -> Sidecars {
        Sidecars::holding(
            "structure",
            "<table><tr><td>12 34</td></tr><tr><td>56 78</td></tr></table>",
        )
    }

    /// A live reader's reading of a table answers to the structure reading
    /// as a sidecar's does: where `--structure-results` corroborates the
    /// grid, the three-way veto keeps it. And since the veto would discard
    /// whatever the model read, the model is not asked to read it. Asked for
    /// anchors, the vetoed table's anchor is answered all the same, by the
    /// veto, and drops rather than waiting for a reading the veto discards.
    #[test]
    fn a_live_table_reading_answers_to_the_structure_veto() {
        let structure = corroborating_structure();
        let read_live = |structure_results: Option<PathBuf>, emit_anchors: bool| {
            let cfg = TierConfig {
                escalate: true,
                emit_anchors,
                structure_results,
                ..Default::default()
            };
            let extracted = page_of_prose();
            let mut a = fluree_doc_pdf::document::analyze_with(
                &mut extracted.clone(),
                &[],
                &cfg.options_for("doc"),
            );
            a.elements = an_anchored_grid();
            let mut asked = Vec::new();
            let mut pages = Vec::new();
            read_in_run(
                &mut a,
                &extracted,
                &cfg,
                "doc",
                &[],
                &Default::default(),
                None,
                |jobs, _| {
                    pages = jobs.iter().map(|(page, _)| *page).collect();
                    let mut readings = fluree_doc_pdf::escalate::Readings::default();
                    for (page, regions) in &jobs {
                        for (tag, _) in regions.iter().flatten() {
                            let crop = format!("p{page}_{tag}");
                            readings.insert(crop.clone(), DEEP_READING.into());
                            asked.push(crop);
                        }
                    }
                    Ok(readings)
                },
            )
            .expect("a stand-in reader does not fail");
            (pages, asked, a.elements)
        };
        let (pages, asked, unopposed) = read_live(None, false);
        assert_eq!(pages, [0]);
        assert_eq!(asked, ["p0_t0"]);
        assert_eq!(unopposed.len(), 1);
        assert_eq!(unopposed[0].provenance, "vlm", "the reading alone wins");
        for emit_anchors in [false, true] {
            let (pages, asked, vetoed) = read_live(Some(structure.0.clone()), emit_anchors);
            assert!(asked.is_empty(), "a vetoed table is not paid for");
            assert!(
                pages.is_empty(),
                "nor is the page it leaves with nothing to read"
            );
            assert_eq!(vetoed.len(), 1, "{emit_anchors}: {vetoed:?}");
            assert_eq!(vetoed[0].provenance, "rust", "the veto keeps the grid");
        }
    }

    /// `triage` does not price a table the structure readings veto, as a
    /// live run does not read it.
    #[test]
    fn triage_does_not_price_a_vetoed_table() {
        let structure = corroborating_structure();
        let priced = |structure_results: Option<PathBuf>| {
            let cfg = TierConfig {
                escalate: true,
                structure_results,
                ..Default::default()
            };
            let extracted = page_of_prose();
            let mut a = fluree_doc_pdf::document::analyze_with(
                &mut extracted.clone(),
                &[],
                &cfg.options_for("doc"),
            );
            a.elements = an_anchored_grid();
            crate::commands::triage::priced(&extracted, &a, "doc", &cfg).0
        };
        assert_eq!(priced(None), 1);
        assert_eq!(priced(Some(structure.0.clone())), 0);
    }

    /// `dev render-routed` writes a table the structure readings veto, since
    /// the structure reader reads it too, and names it apart: less what it
    /// names, it writes the crops `triage` counts and a live run sends.
    #[test]
    fn render_routed_writes_a_vetoed_table_and_names_it() {
        let structure = corroborating_structure();
        let cfg = TierConfig {
            escalate: true,
            structure_results: Some(structure.0.clone()),
            ..Default::default()
        };
        let extracted = page_of_prose();
        let mut a = fluree_doc_pdf::document::analyze_with(
            &mut extracted.clone(),
            &[],
            &cfg.options_for("doc"),
        );
        a.elements = an_anchored_grid();
        let hints = crate::escalate::jobs::hints(&extracted, "doc", &cfg);
        let names = |jobs: &fluree_doc_pdf::escalate::CropJobs| -> Vec<String> {
            jobs.iter()
                .flat_map(|(page, regions)| {
                    regions
                        .iter()
                        .flatten()
                        .map(move |(tag, _)| format!("p{page}_{tag}"))
                })
                .collect()
        };
        let render = |structure_results| {
            crate::escalate::jobs::to_render(&extracted, &a, "doc", &hints, structure_results)
        };
        let (written, _, vetoed) = render(cfg.structure_results.as_deref());
        assert_eq!(
            names(&written),
            ["p0_t0"],
            "written for the structure reader"
        );
        assert_eq!(vetoed, ["p0_t0"]);
        let structure = common::structure_backend(cfg.structure_results.as_deref());
        let (sent, _) = crate::escalate::jobs::at_anchors(
            &extracted,
            &a,
            "doc",
            &hints,
            structure.as_ref().map(|s| s as _),
        );
        let left: Vec<String> = names(&written)
            .into_iter()
            .filter(|n| !vetoed.contains(n))
            .collect();
        assert_eq!(left, names(&sent));
        assert_eq!(
            crate::commands::triage::priced(&extracted, &a, "doc", &cfg).0,
            left.len()
        );
        let (written, _, vetoed) = render(None);
        assert_eq!(names(&written), ["p0_t0"]);
        assert!(
            vetoed.is_empty(),
            "nothing vetoes without structure readings"
        );
    }

    /// `--tier-results` with `--structure-results`: the sidecar reading of a
    /// table answers to the same veto.
    #[test]
    fn a_sidecar_table_reading_answers_to_the_structure_veto() {
        let tiers = Sidecars::holding("tiers", DEEP_READING);
        let structure = corroborating_structure();
        let splice = |structure_results: Option<&std::path::Path>| {
            let mut elements = an_anchored_grid();
            common::apply_tiers(
                Some(&tiers.0),
                structure_results,
                false,
                "doc",
                &mut elements,
                &[],
                &[],
                &Default::default(),
            );
            elements
        };
        let unopposed = splice(None);
        assert_eq!(unopposed.len(), 1);
        assert_eq!(unopposed[0].provenance, "vlm", "the reading alone wins");
        let vetoed = splice(Some(&structure.0));
        assert_eq!(vetoed.len(), 1);
        assert_eq!(vetoed[0].provenance, "rust", "the veto keeps the grid");
    }

    #[test]
    fn page_ranges_parse() {
        assert_eq!(parse_pages("3").unwrap(), vec![2]);
        assert_eq!(parse_pages("1-3").unwrap(), vec![0, 1, 2]);
        assert_eq!(parse_pages("1,4,6-8").unwrap(), vec![0, 3, 5, 6, 7]);
        assert!(parse_pages("0").is_err());
        assert!(parse_pages("5-2").is_err());
        assert!(parse_pages("x").is_err());
    }
}
