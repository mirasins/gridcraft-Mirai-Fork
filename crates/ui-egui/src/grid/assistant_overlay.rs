//! The Mirai proposal preview painted over the sheet: proposed cell inputs on the sheet the
//! proposal was made for. Painting reads the workbook and the proposal only; nothing is edited
//! here. Enter applies through the engine command (stale-checked, one undo step), Esc drops the
//! proposal, so the preview can never touch the workbook or the undo history by itself.

use egui::{Align2, FontId, Painter};

use super::geo::Geo;
use super::{Sheet, Tokens};
use crate::assistant_pane::preview_cells;

/// Paints the proposed inputs over their cells, with a tint and the proposed text. Only on the
/// sheet the proposal was made for, at the revision it was made for, in the normal grid view.
pub(super) fn paint(app: &crate::SheetApp, painter: &Painter, geo: &Geo, sheet_index: usize, sheet: &Sheet, t: &Tokens) {
    let Some(preview) = preview_cells(app) else { return };
    if geo.paper.is_some() || preview.sheet_index != sheet_index || preview.cells.is_empty() {
        return;
    }
    let font = FontId::proportional(13.0 * geo.z);
    for (at, text) in preview.cells {
        let rect = geo.cell_rect(sheet, *at);
        if !rect.is_finite() || !rect.intersects(painter.clip_rect()) || rect.width() < 8.0 || rect.height() < 8.0 {
            continue;
        }
        painter.rect_filled(rect, 2.0, t.accent.gamma_multiply(0.30));
        let clipped = painter.with_clip_rect(rect);
        clipped.text(rect.left_top() + egui::vec2(3.0, rect.height() / 2.0), Align2::LEFT_CENTER, text, font.clone(), t.text);
    }
}
