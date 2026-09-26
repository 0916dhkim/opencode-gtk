//! The client's own symbolic icons, embedded from `icons/`.
//!
//! These are the GTK client's icon set (it installed them as a GResource icon
//! theme). libcosmic's `widget::icon::from_name` looks icons up in an icon
//! theme on disk — its bundled fallback compiles to nothing on Linux — so the
//! port had fallen back to font glyphs (`☰`, `⚙`, `✕`, …), which rendered in
//! whatever font happened to be installed. Embedding the same SVGs keeps the
//! icons identical everywhere; `symbolic = true` lets iced paint them with the
//! theme's icon colour, so they follow light and dark like the text does.

use cosmic::widget::icon::{self, Handle};

macro_rules! icons {
    ($($fn_name:ident => $file:literal),* $(,)?) => {
        $(
            #[doc = concat!("`icons/", $file, "`")]
            pub fn $fn_name() -> Handle {
                symbolic(include_bytes!(concat!("../icons/", $file)))
            }
        )*
    };
}

icons! {
    add => "add.svg",
    attach => "attach.svg",
    close => "close.svg",
    connection => "connection.svg",
    edit => "edit.svg",
    search => "search.svg",
    send => "send.svg",
    sessions => "sessions.svg",
    settings => "settings.svg",
    stop => "stop.svg",
}

fn symbolic(bytes: &'static [u8]) -> Handle {
    let mut handle = icon::from_svg_bytes(bytes);
    handle.symbolic = true;
    handle
}
