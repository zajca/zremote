//! Inline review comment cards rendered underneath a diff line.
//!
//! The RFC (§9.1, §9.3) says a draft comment should appear as a card
//! attached to the line it anchors to, with Edit + Delete actions. The
//! actual rendering lives inside `diff_pane.rs` today (the card is just
//! one of the rows it emits); this module holds the pure side-inference
//! helper used by the click handler plus the types shared with the diff
//! pane.

use zremote_protocol::project::ReviewSide;

/// Side inference helper — RFC §9.1, last paragraph: "unified: removed →
/// left, added / context → right". Side-by-side callers pass the gutter
/// they clicked directly and skip this.
#[must_use]
pub fn infer_side_from_kind(kind: zremote_protocol::project::DiffLineKind) -> ReviewSide {
    match kind {
        zremote_protocol::project::DiffLineKind::Removed => ReviewSide::Left,
        zremote_protocol::project::DiffLineKind::Added
        | zremote_protocol::project::DiffLineKind::Context
        | zremote_protocol::project::DiffLineKind::NoNewlineMarker => ReviewSide::Right,
    }
}
