//! Geometry, re-exported from the shared document model.
//!
//! The kurbo bridge stays here: it is how the PDF renderer's rectangles enter
//! the model, and nothing outside PDF parsing needs it.

pub use fluree_doc_model::geom::BBox;

/// A renderer rectangle as a model box.
pub fn from_kurbo(r: kurbo::Rect) -> BBox {
    BBox {
        x0: r.x0,
        y0: r.y0,
        x1: r.x1,
        y1: r.y1,
    }
}
