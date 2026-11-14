//! Monospace fonts with a bold variant.
//!
//! egui's bundled monospace font (Hack) has no bold face. If a system font
//! pair (regular + bold of the same family, so column widths match) is found,
//! it becomes the monospace font and its bold face is registered as
//! [`BOLD_FAMILY`]. The bundled fonts stay as fallbacks for missing glyphs.

use std::sync::Arc;

use eframe::egui::{self, FontData, FontDefinitions, FontFamily};

pub const BOLD_FAMILY: &str = "mono-bold";

/// (regular, bold) paths, tried in order.
const CANDIDATES: &[(&str, &str)] = &[
    (
        "C:/Windows/Fonts/consola.ttf",
        "C:/Windows/Fonts/consolab.ttf",
    ),
    (
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf",
    ),
    (
        "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
        "/usr/share/fonts/TTF/DejaVuSansMono-Bold.ttf",
    ),
];

/// Installs the fonts; returns the bold family if one is available.
pub fn install(ctx: &egui::Context) -> Option<FontFamily> {
    let (regular, bold) = CANDIDATES.iter().find_map(|(r, b)| {
        let regular = std::fs::read(r).ok()?;
        let bold = std::fs::read(b).ok()?;
        Some((regular, bold))
    })?;

    let mut fonts = FontDefinitions::default();
    fonts
        .font_data
        .insert("mono".into(), Arc::new(FontData::from_owned(regular)));
    fonts
        .font_data
        .insert(BOLD_FAMILY.into(), Arc::new(FontData::from_owned(bold)));
    let fallback = fonts
        .families
        .get(&FontFamily::Monospace)
        .cloned()
        .unwrap_or_default();
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .insert(0, "mono".into());
    let mut bold_list = vec![BOLD_FAMILY.to_owned()];
    bold_list.extend(fallback);
    let family = FontFamily::Name(BOLD_FAMILY.into());
    fonts.families.insert(family.clone(), bold_list);
    ctx.set_fonts(fonts);
    Some(family)
}
