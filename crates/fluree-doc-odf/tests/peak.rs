//! What reading a document holds at most, as the allocator counts it: a run
//! of text the budget has no room for is refused before it is copied out of
//! the part. One test alone in its binary, so no other thread allocates
//! meanwhile.

use fluree_doc_model::budget::{converting_to, within, Output};
use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

/// The system allocator, counting the bytes held and the most held.
struct Counting;

static HELD: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grew(n: usize) {
    PEAK.fetch_max(HELD.fetch_add(n, Relaxed) + n, Relaxed);
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            grew(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        unsafe { System.dealloc(p, layout) };
        HELD.fetch_sub(layout.size(), Relaxed);
    }

    unsafe fn realloc(&self, p: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, layout, size) };
        if !q.is_null() {
            if size > layout.size() {
                grew(size - layout.size());
            } else {
                HELD.fetch_sub(layout.size() - size, Relaxed);
            }
        }
        q
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

#[test]
fn a_run_of_text_past_the_budget_is_refused_before_it_is_copied() {
    // One paragraph of 8 MiB of text, less a little so that the part's
    // buffer is no larger.
    let run = (8 << 20) - 1024;
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut z = zip::ZipWriter::new(&mut buf);
    let deflate = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    z.start_file("mimetype", deflate).unwrap();
    z.write_all(b"application/vnd.oasis.opendocument.text")
        .unwrap();
    z.start_file("content.xml", deflate).unwrap();
    write!(
        z,
        "<office:document-content xmlns:office=\"urn:oasis:names:tc:opendocument:xmlns:office:1.0\" \
         xmlns:text=\"urn:oasis:names:tc:opendocument:xmlns:text:1.0\"><office:body><office:text>\
         <text:p>{}</text:p></office:text></office:body></office:document-content>",
        "y".repeat(run)
    )
    .unwrap();
    z.finish().unwrap();
    let pkg = buf.into_inner();
    // What reading it within `memory` holds at most, and why it is refused.
    let read = |memory: u64| {
        let base = HELD.load(Relaxed);
        PEAK.store(base, Relaxed);
        let read = within(memory, || {
            converting_to(Output::Flat, || fluree_doc_odf::parse(&pkg))
        });
        let peak = PEAK.load(Relaxed) - base;
        (read.unwrap_err().to_string(), peak)
    };
    // Room for the part, at two bytes a byte, and its tree, about a byte a
    // byte of text: not for the paragraph's text, at twelve. The part and
    // the tree's copy of the run are held, not the paragraph's two (its
    // runs joined, then tidied).
    let (refused, peak) = read(40 << 20);
    assert!(refused.contains("text past the 40 MiB"), "{refused}");
    assert!(peak < 3 * run, "{} MiB held at most", peak >> 20);
    // Room for the part alone: the part, not the tree's copy of the run.
    let (refused, peak) = read(20 << 20);
    assert!(refused.contains("cells past the 20 MiB"), "{refused}");
    assert!(peak < 3 * run / 2, "{} MiB held at most", peak >> 20);
}
