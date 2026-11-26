//! What a grid's cells read of a page, worked out once per grid.
//!
//! A grid is judged many times over before it is kept: whether it reads as
//! prose, whether it has a typeset header, what to trim, what its title is,
//! how sure it is, and then again as it is written out. Each judgement asks
//! which cell every glyph falls in ([`crate::table::Grid::glyph_cells`]),
//! and most ask for the cells' text ([`crate::table::Grid::cell_texts`]).
//! Both sort and walk all of the page's glyphs. The same grid over the same
//! glyphs was asked five or six times on average.
//!
//! Both answers depend only on the glyphs and the grid's cuts (`xs`, `ys`).
//! While a page's glyphs are pinned ([`pin`]), each answer is kept by those
//! cuts and handed back on the next ask. A grid whose cuts change (trimmed,
//! split, given its column heads) asks with new cuts and is worked out
//! afresh.
//!
//! A pin borrows the glyphs, so nothing can change or free them while it
//! lives. An ask is answered from the memo only when it is about that same
//! slice, the same memory and length. Any other slice is worked out as
//! before. This is a memo, not state: without a pin, or on another
//! thread, every ask is worked out as it always was, and the answer is the
//! same.

use crate::glyph::Glyph;
use std::cell::RefCell;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::rc::Rc;

/// A grid's cuts, bit for bit.
type Cuts = (Vec<u64>, Vec<u64>);

/// The answers kept for one pinned slice.
struct Pinned {
    id: u64,
    at: usize,
    len: usize,
    homes: HashMap<Cuts, Rc<[Option<usize>]>>,
    texts: HashMap<Cuts, Rc<[String]>>,
}

thread_local! {
    static PINNED: RefCell<Vec<Pinned>> = const { RefCell::new(Vec::new()) };
    static NEXT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Keeps the cells read from `glyphs` while it lives; see the module.
pub(crate) struct Pin<'g> {
    id: u64,
    _glyphs: PhantomData<&'g [Glyph]>,
}

/// Pin `glyphs` for as long as the returned guard lives.
pub(crate) fn pin(glyphs: &[Glyph]) -> Pin<'_> {
    let id = NEXT.with(|n| {
        let id = n.get();
        n.set(id + 1);
        id
    });
    PINNED.with(|p| {
        p.borrow_mut().push(Pinned {
            id,
            at: glyphs.as_ptr() as usize,
            len: glyphs.len(),
            homes: HashMap::new(),
            texts: HashMap::new(),
        })
    });
    Pin {
        id,
        _glyphs: PhantomData,
    }
}

impl Drop for Pin<'_> {
    fn drop(&mut self) {
        PINNED.with(|p| p.borrow_mut().retain(|x| x.id != self.id));
    }
}

fn cuts(xs: &[f64], ys: &[f64]) -> Cuts {
    (
        xs.iter().map(|x| x.to_bits()).collect(),
        ys.iter().map(|y| y.to_bits()).collect(),
    )
}

/// The memo kept for `glyphs`, if they are pinned.
fn with_pinned<R>(glyphs: &[Glyph], f: impl FnOnce(&mut Pinned) -> R) -> Option<R> {
    let (at, len) = (glyphs.as_ptr() as usize, glyphs.len());
    PINNED.with(|p| {
        p.borrow_mut()
            .iter_mut()
            .rev()
            .find(|x| x.at == at && x.len == len)
            .map(f)
    })
}

/// The cell each glyph is read into, from the memo or from `read`.
pub(crate) fn homes(
    glyphs: &[Glyph],
    xs: &[f64],
    ys: &[f64],
    read: impl FnOnce() -> Vec<Option<usize>>,
) -> Vec<Option<usize>> {
    let key = cuts(xs, ys);
    match with_pinned(glyphs, |m| m.homes.get(&key).cloned()) {
        None => read(),
        Some(Some(kept)) => kept.to_vec(),
        Some(None) => {
            let got = read();
            with_pinned(glyphs, |m| m.homes.insert(key, got.as_slice().into()));
            got
        }
    }
}

/// Each cell's text, from the memo or from `read`.
pub(crate) fn texts(
    glyphs: &[Glyph],
    xs: &[f64],
    ys: &[f64],
    read: impl FnOnce() -> Vec<String>,
) -> Vec<String> {
    let key = cuts(xs, ys);
    match with_pinned(glyphs, |m| m.texts.get(&key).cloned()) {
        None => read(),
        Some(Some(kept)) => kept.to_vec(),
        Some(None) => {
            let got = read();
            with_pinned(glyphs, |m| m.texts.insert(key, got.as_slice().into()));
            got
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glyph(x: f64) -> Glyph {
        Glyph {
            text: "a".into(),
            bbox: None,
            page: 0,
            origin: (x, 10.0),
            rotation_deg: 0.0,
            font_size: 10.0,
            weight: None,
            italic: false,
            advance: None,
            draw_index: 0,
        }
    }

    #[test]
    fn a_pinned_slice_is_read_once_per_grid_and_no_other_slice_is_kept() {
        let page = vec![glyph(1.0), glyph(2.0)];
        let other = vec![glyph(3.0), glyph(4.0)];
        let reads = std::cell::Cell::new(0);
        let read = || {
            reads.set(reads.get() + 1);
            vec![Some(0), None]
        };
        let _pin = pin(&page);
        homes(&page, &[0.0, 1.0], &[0.0, 1.0], read);
        homes(&page, &[0.0, 1.0], &[0.0, 1.0], read);
        assert_eq!(reads.get(), 1);
        // New cuts are a new grid.
        homes(&page, &[0.0, 2.0], &[0.0, 1.0], read);
        assert_eq!(reads.get(), 2);
        // A slice not pinned is read every time.
        homes(&other, &[0.0, 1.0], &[0.0, 1.0], read);
        homes(&other, &[0.0, 1.0], &[0.0, 1.0], read);
        assert_eq!(reads.get(), 4);
        // Part of the page is another slice.
        homes(&page[..1], &[0.0, 1.0], &[0.0, 1.0], read);
        assert_eq!(reads.get(), 5);
    }

    #[test]
    fn nothing_is_kept_once_the_pin_is_gone() {
        let page = vec![glyph(1.0)];
        let reads = std::cell::Cell::new(0);
        let read = || {
            reads.set(reads.get() + 1);
            vec!["x".to_string()]
        };
        {
            let _pin = pin(&page);
            texts(&page, &[0.0], &[0.0], read);
            texts(&page, &[0.0], &[0.0], read);
        }
        texts(&page, &[0.0], &[0.0], read);
        assert_eq!(reads.get(), 2);
    }
}
