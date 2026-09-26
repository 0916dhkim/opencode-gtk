//! Shared type metrics.
//!
//! The GTK client sized everything in `em` against the system GTK font
//! (`gtk-font-name`, default "Noto Sans, 10" ≈ 13px) and implemented zoom by
//! dividing that base, so every `em` value scaled with it. iced has no runtime
//! scale factor for a window, so the same thing is done by hand: every text
//! size goes through [`em`] (and the GTK spacing scale through [`space`]) with
//! the app's zoom.

/// 1em, in logical pixels.
pub const BASE_FONT_PX: f32 = 13.0;

/// `factor` em at `zoom`, rounded to whole pixels.
#[must_use]
pub fn em(factor: f32, zoom: f32) -> u32 {
    (factor * BASE_FONT_PX * zoom).round().max(1.0) as u32
}

/// `factor` em at `zoom`, for paddings and spacings.
#[must_use]
pub fn space(factor: f32, zoom: f32) -> f32 {
    factor * BASE_FONT_PX * zoom
}

/// A 1em em-space value at zoom 1.0, for tests and default sizing.
#[must_use]
pub const fn em_base(factor: f32) -> u32 {
    (factor * BASE_FONT_PX) as u32
}
