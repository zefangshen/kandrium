//! Lucide icons, rendered straight from the egui app's SVG files.
//!
//! `Svg::data` takes raw bytes, so there is no asset-path mapping and no second
//! copy of the icons in this crate. The paint path tints the SVG with
//! `text_color`, so a single file covers every colour.

use gpui_kit::{Rgba, Styled, Svg, px, svg};

/// Icons used by the gpui shell. Add entries as views need them.
#[derive(Clone, Copy, Debug)]
pub enum Icon {
    Archive,
    Calendar,
    Check,
    ChevronDown,
    ChevronLeft,
    ChevronRight,
    ChevronUp,
    Ellipsis,
    Eye,
    File,
    FilePlus,
    FileText,
    Folder,
    FolderColumns,
    FolderKanban,
    FolderPlus,
    Globe,
    Highlighter,
    House,
    Minus,
    Pencil,
    Plane,
    Plus,
    RotateCcw,
    Save,
    Search,
    Square,
    SquareKanban,
    StickyNote,
    Trash2,
    Upload,
    Users,
    X,
}

impl Icon {
    fn bytes(self) -> &'static [u8] {
        match self {
            Icon::Archive => include_bytes!("../../app/assets/icons/archive.svg"),
            Icon::Calendar => include_bytes!("../../app/assets/icons/calendar.svg"),
            Icon::Check => include_bytes!("../../app/assets/icons/check.svg"),
            Icon::ChevronDown => include_bytes!("../../app/assets/icons/chevron-down.svg"),
            Icon::ChevronLeft => include_bytes!("../../app/assets/icons/chevron-left.svg"),
            Icon::ChevronRight => include_bytes!("../../app/assets/icons/chevron-right.svg"),
            Icon::ChevronUp => include_bytes!("../../app/assets/icons/chevron-up.svg"),
            Icon::Ellipsis => include_bytes!("../../app/assets/icons/ellipsis.svg"),
            Icon::Eye => include_bytes!("../../app/assets/icons/eye.svg"),
            Icon::File => include_bytes!("../../app/assets/icons/file.svg"),
            Icon::FilePlus => include_bytes!("../../app/assets/icons/file-plus.svg"),
            Icon::FileText => include_bytes!("../../app/assets/icons/file-text.svg"),
            Icon::Folder => include_bytes!("../../app/assets/icons/folder.svg"),
            Icon::FolderColumns => include_bytes!("../../app/assets/icons/folder-columns.svg"),
            Icon::FolderKanban => include_bytes!("../../app/assets/icons/folder-kanban.svg"),
            Icon::FolderPlus => include_bytes!("../../app/assets/icons/folder-plus.svg"),
            Icon::Globe => include_bytes!("../../app/assets/icons/globe.svg"),
            Icon::Highlighter => include_bytes!("../../app/assets/icons/highlighter.svg"),
            Icon::House => include_bytes!("../../app/assets/icons/house.svg"),
            Icon::Minus => include_bytes!("../../app/assets/icons/minus.svg"),
            Icon::Pencil => include_bytes!("../../app/assets/icons/pencil.svg"),
            Icon::Plane => include_bytes!("../../app/assets/icons/plane.svg"),
            Icon::Plus => include_bytes!("../../app/assets/icons/plus.svg"),
            Icon::RotateCcw => include_bytes!("../../app/assets/icons/rotate-ccw.svg"),
            Icon::Save => include_bytes!("../../app/assets/icons/save.svg"),
            Icon::Search => include_bytes!("../../app/assets/icons/search.svg"),
            Icon::Square => include_bytes!("../../app/assets/icons/square.svg"),
            Icon::SquareKanban => include_bytes!("../../app/assets/icons/square-kanban.svg"),
            Icon::StickyNote => include_bytes!("../../app/assets/icons/sticky-note.svg"),
            Icon::Trash2 => include_bytes!("../../app/assets/icons/trash-2.svg"),
            Icon::Upload => include_bytes!("../../app/assets/icons/upload.svg"),
            Icon::Users => include_bytes!("../../app/assets/icons/users.svg"),
            Icon::X => include_bytes!("../../app/assets/icons/x.svg"),
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
