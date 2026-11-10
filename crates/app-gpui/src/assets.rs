//! Lucide icons, rendered straight from the egui app's SVG files.
//!
//! `Svg::data` takes raw bytes, so there is no asset-path mapping and no second
//! copy of the icons in this crate. The paint path tints the SVG with
//! `text_color`, so a single file covers every colour.

use gpui_kit::{Rgba, Styled, Svg, px, svg};

/// Icons used by the gpui shell. Add entries as views need them.
#[derive(Clone, Copy, Debug)]
pub enum Icon {
    Calendar,
    Check,
    FileText,
    Folder,
    FolderKanban,
    Globe,
    House,
    StickyNote,
}

impl Icon {
    fn bytes(self) -> &'static [u8] {
        match self {
            Icon::Calendar => include_bytes!("../../app/assets/icons/calendar.svg"),
            Icon::Check => include_bytes!("../../app/assets/icons/check.svg"),
            Icon::FileText => include_bytes!("../../app/assets/icons/file-text.svg"),
            Icon::Folder => include_bytes!("../../app/assets/icons/folder.svg"),
            Icon::FolderKanban => include_bytes!("../../app/assets/icons/folder-kanban.svg"),
            Icon::Globe => include_bytes!("../../app/assets/icons/globe.svg"),
            Icon::House => include_bytes!("../../app/assets/icons/house.svg"),
            Icon::StickyNote => include_bytes!("../../app/assets/icons/sticky-note.svg"),
        }
    }
}

/// A tinted icon at a fixed size.
pub fn icon(icon: Icon, size: f32, color: Rgba) -> Svg {
    svg()
        .data(icon.bytes())
        .text_color(color)
        .size(px(size))
        .flex_shrink_0()
}
