//! Colour palette for the client's own surfaces.
//!
//! The GTK client resolved every colour through `@oc_*` tokens defined twice
//! (`tokens-light.css` / `tokens-dark.css`, dark being the base). The initial
//! COSMIC port copied the *dark* values as literals, so it rendered dark no
//! matter what the desktop theme was. This module keeps the same token names and
//! values, pairs them per mode, and picks the pair from the active COSMIC theme
//! (`cosmic::theme::is_dark()`), so light desktops get the light palette.
//!
//! Values below are copied verbatim from the GTK token files; the comment on
//! each field names its `@oc_*` source.

use cosmic::iced::Color;

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    /// `@oc_bg_window`
    pub window_bg: Color,
    /// `@oc_bg_tab_strip` — the left sidebar (the GTK client kept its session tabs there).
    pub sidebar_bg: Color,
    /// `@oc_border_tab_strip`
    pub sidebar_border: Color,
    /// `@oc_bg_session_tab_active` — the active sidebar row.
    pub sidebar_row_active_bg: Color,
    /// `@oc_bg_sidebar_nav_separator`
    pub nav_separator: Color,
    /// `@oc_bg_session_header_bar` — the strip above the transcript and agent message cards.
    pub inset_bg: Color,
    /// `@oc_border_session_header_bar`
    pub inset_border: Color,
    /// `@oc_bg_new_session_search` — cards and job rows.
    pub card_bg: Color,
    /// `@oc_border_markdown_code_header` — card, header-strip and code-header borders.
    pub panel_border: Color,
    /// `@oc_bg_form_notice`
    pub form_notice_bg: Color,
    /// `@oc_border_form_notice`
    pub form_notice_border: Color,
    /// `@oc_fg_prompt_subheading`
    pub prompt_subheading: Color,
    /// `@oc_fg_prompt_metadata`
    pub prompt_metadata: Color,
    /// `@oc_bg_new_session_palette` — GTK's modal palettes.
    pub modal_bg: Color,
    /// `@oc_border_new_session_palette`
    pub modal_border: Color,
    /// `@oc_bg_queue_tray`
    pub tray_bg: Color,
    /// `@oc_border_queue_tray`
    pub tray_border: Color,
    /// `@oc_bg_composer_frame`
    pub composer_bg: Color,
    /// `@oc_border_composer_frame`
    pub composer_border: Color,
    /// `@oc_bg_composer_frame_button_composer_action_suggested_action`
    pub accent_bg: Color,
    /// `@oc_fg_composer_frame_button_composer_action_suggested_action`
    pub accent_fg: Color,
    /// `@oc_fg_message_role` — muted labels and the message role.
    pub muted_text: Color,
    /// `@oc_fg_message_content`
    pub content_text: Color,
    /// `@oc_fg_session_header_hint` — message timestamps and header hints.
    pub time_text: Color,
    /// `@oc_fg_session_header_title` — the session title in the header strip.
    pub header_title_text: Color,
    /// `@oc_fg_user_message_message_role`
    pub user_role_text: Color,
    /// `@oc_fg_message_reasoning_message_role`
    pub reasoning_text: Color,
    /// `@oc_fg_message_content_link`
    pub link_text: Color,
    /// `@oc_fg_window` — the error card's body text.
    pub error_body_text: Color,
    /// `@oc_bg_transcript_status_compact`
    pub status_pill_bg: Color,
    /// `@oc_border_transcript_status_compact`
    pub status_pill_border: Color,
    /// `@oc_fg_transcript_status_compact`
    pub status_pill_text: Color,
    /// `@oc_fg_session_tab_unread_session_tab_title`
    pub tab_unread_text: Color,
    /// `@oc_fg_session_tab_index`
    pub tab_index_text: Color,
    /// `@oc_bg_session_tab_button_session_tab_close_hover`
    pub tab_close_hover_bg: Color,
    /// `@oc_fg_session_tab_button_session_tab_close_hover`
    pub tab_close_hover_fg: Color,
    /// `@oc_bg_sidebar_new_session_hover` — session row hover.
    pub sidebar_hover_bg: Color,
    /// `@oc_bg_composer_action_hover`
    pub action_hover_bg: Color,
    /// `@oc_border_queue_tray_row` — hairlines between tray rows.
    pub tray_row_divider: Color,
    /// `@oc_fg_queue_tray_title`
    pub tray_title_text: Color,
    /// `@oc_fg_queue_tray_group`
    pub tray_group_text: Color,
    /// `@oc_fg_queue_tray_text`
    pub tray_text: Color,
    /// `@oc_bg_queue_tray_button`
    pub tray_button_bg: Color,
    /// `@oc_border_queue_tray_button`
    pub tray_button_border: Color,
    /// `@oc_fg_queue_tray_button`
    pub tray_button_text: Color,
    /// `@oc_bg_queue_tray_paused`
    pub tray_paused: Color,
    /// `@oc_fg_button_session_id_copy_copied` — completed jobs, open sessions.
    pub status_ok: Color,
    /// `@oc_fg_session_tab_busy_session_tab_title` — busy runs, stopped jobs.
    pub status_busy: Color,
    /// `@oc_bg_queue_badge_steer`
    pub badge_steer_bg: Color,
    /// `@oc_border_queue_badge_steer`
    pub badge_steer_border: Color,
    /// `@oc_fg_queue_badge_steer`
    pub badge_steer_text: Color,
    /// `@oc_bg_queue_badge_queue`
    pub badge_queue_bg: Color,
    /// `@oc_border_queue_badge_queue`
    pub badge_queue_border: Color,
    /// `@oc_fg_queue_badge_queue`
    pub badge_queue_text: Color,
    /// `@oc_bg_session_tab_status_idle` — the inactive session dot.
    pub status_idle: Color,
    /// `@oc_bg_session_tab_status_unread` — the active session dot.
    pub status_unread: Color,
    /// `@oc_fg_message_error_header`
    pub error_text: Color,
    /// `@oc_bg_message_error_card`
    pub error_card_bg: Color,
    /// `@oc_border_message_error_card`
    pub error_card_border: Color,
    /// `@oc_bg_user_message`
    pub user_message_bg: Color,
    /// `@oc_border_message_row`
    pub message_border: Color,
    /// `@oc_border_markdown_blockquote` — reasoning boxes and blockquotes.
    pub quote_border: Color,
    /// `@oc_fg_markdown_blockquote`
    pub quote_text: Color,
    /// `@oc_bg_markdown_code_block`
    pub code_block_bg: Color,
    /// `@oc_border_markdown_code_block`
    pub code_block_border: Color,
    /// `@oc_bg_markdown_code_header`
    pub code_header_bg: Color,
    /// `@oc_fg_markdown_code_language`
    pub code_language_text: Color,
    /// `@oc_fg_markdown_code_content`
    pub code_content_text: Color,
    /// Translucent fill of reasoning boxes and blockquotes (`alpha(#ffffff, 0.02)` in dark).
    pub overlay_bg: Color,
    /// Translucent hover/agent-card border (`alpha(#ffffff, 0.04)` in dark).
    pub overlay_border: Color,
    /// Translucent tool-badge fill (`alpha(#ffffff, 0.05)` in dark).
    pub badge_overlay_bg: Color,
}

pub const LIGHT: Palette = Palette {
    window_bg: Color::from_rgb8(0xf7, 0xf5, 0xf1),
    sidebar_bg: Color::from_rgb8(0xf0, 0xee, 0xe9),
    sidebar_border: Color::from_rgb8(0xd8, 0xd4, 0xcc),
    sidebar_row_active_bg: Color::from_rgb8(0xde, 0xdb, 0xd4),
    nav_separator: Color::from_rgb8(0xd0, 0xcc, 0xc4),
    inset_bg: Color::from_rgb8(0xf0, 0xec, 0xe1),
    inset_border: Color::from_rgb8(0xd5, 0xd0, 0xc7),
    card_bg: Color::from_rgb8(0xf4, 0xf0, 0xe6),
    panel_border: Color::from_rgb8(0xd3, 0xcf, 0xc7),
    form_notice_bg: Color::from_rgb8(0xf0, 0xee, 0xe9),
    form_notice_border: Color::from_rgb8(0xcb, 0xc7, 0xbf),
    prompt_subheading: Color::from_rgb8(0x8b, 0x59, 0x18),
    prompt_metadata: Color::from_rgb8(0x62, 0x67, 0x64),
    modal_bg: Color::from_rgb8(0xfb, 0xf9, 0xf4),
    modal_border: Color::from_rgb8(0xd0, 0xcc, 0xc4),
    tray_bg: Color::from_rgb8(0xf0, 0xee, 0xe9),
    tray_border: Color::from_rgb8(0xcb, 0xc7, 0xbf),
    composer_bg: Color::from_rgb8(0xff, 0xff, 0xff),
    composer_border: Color::from_rgb8(0xcb, 0xc7, 0xbf),
    accent_bg: Color::from_rgb8(0xc4, 0x92, 0x3a),
    accent_fg: Color::from_rgb8(0x1a, 0x17, 0x13),
    muted_text: Color::from_rgb8(0x70, 0x76, 0x73),
    content_text: Color::from_rgb8(0x29, 0x27, 0x24),
    time_text: Color::from_rgb8(0x8b, 0x91, 0x8e),
    header_title_text: Color::from_rgb8(0x1f, 0x1e, 0x1c),
    user_role_text: Color::from_rgb8(0x5c, 0x4a, 0x2e),
    reasoning_text: Color::from_rgb8(0x9a, 0x9e, 0x9b),
    link_text: Color::from_rgb8(0x8f, 0x5d, 0x1c),
    error_body_text: Color::from_rgb8(0x28, 0x26, 0x23),
    status_pill_bg: Color::from_rgba8(0xff, 0xff, 0xff, 0.94),
    status_pill_border: Color::from_rgb8(0xcb, 0xc7, 0xbf),
    status_pill_text: Color::from_rgb8(0x4d, 0x51, 0x4f),
    tab_unread_text: Color::from_rgb8(0x13, 0x5d, 0x83),
    tab_index_text: Color::from_rgb8(0x92, 0x99, 0x9f),
    tab_close_hover_bg: Color::from_rgb8(0xb8, 0x4b, 0x45),
    tab_close_hover_fg: Color::from_rgb8(0xff, 0xff, 0xff),
    sidebar_hover_bg: Color::from_rgb8(0xe5, 0xe2, 0xdc),
    action_hover_bg: Color::from_rgb8(0xee, 0xeb, 0xe4),
    tray_row_divider: Color::from_rgb8(0xde, 0xda, 0xd2),
    tray_title_text: Color::from_rgb8(0x34, 0x31, 0x2d),
    tray_group_text: Color::from_rgb8(0x85, 0x7f, 0x75),
    tray_text: Color::from_rgb8(0x26, 0x24, 0x21),
    tray_button_bg: Color::from_rgb8(0xff, 0xff, 0xff),
    tray_button_border: Color::from_rgb8(0xd5, 0xd0, 0xc7),
    tray_button_text: Color::from_rgb8(0x34, 0x31, 0x2d),
    tray_paused: Color::from_rgb8(0x8f, 0x8a, 0x82),
    badge_steer_bg: Color::from_rgb8(0xf6, 0xe7, 0xcc),
    badge_steer_border: Color::from_rgb8(0xe2, 0xc4, 0x8f),
    badge_steer_text: Color::from_rgb8(0x8b, 0x59, 0x18),
    badge_queue_bg: Color::from_rgb8(0xe8, 0xe6, 0xe1),
    badge_queue_border: Color::from_rgb8(0xcf, 0xcc, 0xc5),
    badge_queue_text: Color::from_rgb8(0x5d, 0x60, 0x5e),
    status_ok: Color::from_rgb8(0x1a, 0x7f, 0x37),
    status_busy: Color::from_rgb8(0x9c, 0x64, 0x1a),
    status_idle: Color::from_rgb8(0x78, 0x82, 0x7e),
    status_unread: Color::from_rgb8(0x17, 0x78, 0xa8),
    error_text: Color::from_rgb8(0xcf, 0x22, 0x2e),
    error_card_bg: Color::from_rgba8(0xcf, 0x22, 0x2e, 0.06),
    error_card_border: Color::from_rgba8(0xcf, 0x22, 0x2e, 0.28),
    user_message_bg: Color::from_rgb8(0xe4, 0xdd, 0xd0),
    message_border: Color::from_rgba8(0x00, 0x00, 0x00, 0.08),
    quote_border: Color::from_rgb8(0x8b, 0x91, 0x8e),
    quote_text: Color::from_rgb8(0x55, 0x5a, 0x57),
    code_block_bg: Color::from_rgb8(0xf7, 0xf5, 0xf1),
    code_block_border: Color::from_rgb8(0xd0, 0xcc, 0xc4),
    code_header_bg: Color::from_rgb8(0xe9, 0xe6, 0xdf),
    code_language_text: Color::from_rgb8(0x6f, 0x74, 0x71),
    code_content_text: Color::from_rgb8(0x2f, 0x2d, 0x29),
    overlay_bg: Color::from_rgba8(0x00, 0x00, 0x00, 0.02),
    overlay_border: Color::from_rgba8(0x00, 0x00, 0x00, 0.04),
    badge_overlay_bg: Color::from_rgba8(0x00, 0x00, 0x00, 0.05),
};

pub const DARK: Palette = Palette {
    window_bg: Color::from_rgb8(0x10, 0x12, 0x14),
    sidebar_bg: Color::from_rgb8(0x0d, 0x0f, 0x11),
    sidebar_border: Color::from_rgb8(0x24, 0x28, 0x2c),
    sidebar_row_active_bg: Color::from_rgb8(0x22, 0x26, 0x2a),
    nav_separator: Color::from_rgb8(0x2b, 0x30, 0x34),
    inset_bg: Color::from_rgb8(0x13, 0x16, 0x19),
    inset_border: Color::from_rgb8(0x23, 0x27, 0x2c),
    card_bg: Color::from_rgb8(0x18, 0x1c, 0x21),
    panel_border: Color::from_rgb8(0x28, 0x2c, 0x30),
    form_notice_bg: Color::from_rgb8(0x15, 0x18, 0x1b),
    form_notice_border: Color::from_rgb8(0x2d, 0x32, 0x36),
    prompt_subheading: Color::from_rgb8(0xd8, 0xa5, 0x5f),
    prompt_metadata: Color::from_rgb8(0x9d, 0xa4, 0xaa),
    modal_bg: Color::from_rgb8(0x15, 0x18, 0x1c),
    modal_border: Color::from_rgb8(0x2a, 0x30, 0x38),
    tray_bg: Color::from_rgb8(0x15, 0x18, 0x1b),
    tray_border: Color::from_rgb8(0x2d, 0x32, 0x36),
    composer_bg: Color::from_rgb8(0x19, 0x1c, 0x1f),
    composer_border: Color::from_rgb8(0x30, 0x35, 0x3a),
    accent_bg: Color::from_rgb8(0xd2, 0x9b, 0x52),
    accent_fg: Color::from_rgb8(0x17, 0x13, 0x0e),
    muted_text: Color::from_rgb8(0x8d, 0x95, 0x9d),
    content_text: Color::from_rgb8(0xe7, 0xe3, 0xdc),
    time_text: Color::from_rgb8(0x6a, 0x72, 0x79),
    header_title_text: Color::from_rgb8(0xf0, 0xed, 0xe7),
    user_role_text: Color::from_rgb8(0xd7, 0xc4, 0xa3),
    reasoning_text: Color::from_rgb8(0x6a, 0x72, 0x79),
    link_text: Color::from_rgb8(0xe0, 0xa7, 0x5e),
    error_body_text: Color::from_rgb8(0xe8, 0xe5, 0xdf),
    status_pill_bg: Color::from_rgba8(0x17, 0x1a, 0x1d, 0.94),
    status_pill_border: Color::from_rgb8(0x34, 0x39, 0x3e),
    status_pill_text: Color::from_rgb8(0xc4, 0xc8, 0xca),
    tab_unread_text: Color::from_rgb8(0xd9, 0xef, 0xff),
    tab_index_text: Color::from_rgb8(0x92, 0x99, 0x9f),
    tab_close_hover_bg: Color::from_rgb8(0x8e, 0x37, 0x37),
    tab_close_hover_fg: Color::from_rgb8(0xff, 0xff, 0xff),
    sidebar_hover_bg: Color::from_rgb8(0x19, 0x1d, 0x20),
    action_hover_bg: Color::from_rgb8(0x22, 0x26, 0x2a),
    tray_row_divider: Color::from_rgb8(0x26, 0x2b, 0x2f),
    tray_title_text: Color::from_rgb8(0xe2, 0xdf, 0xd8),
    tray_group_text: Color::from_rgb8(0x8e, 0x93, 0x8f),
    tray_text: Color::from_rgb8(0xd6, 0xd3, 0xcc),
    tray_button_bg: Color::from_rgb8(0x1d, 0x21, 0x24),
    tray_button_border: Color::from_rgb8(0x35, 0x3b, 0x40),
    tray_button_text: Color::from_rgb8(0xe2, 0xdf, 0xd8),
    tray_paused: Color::from_rgb8(0x7d, 0x83, 0x7f),
    badge_steer_bg: Color::from_rgb8(0x3a, 0x2d, 0x19),
    badge_steer_border: Color::from_rgb8(0x5a, 0x44, 0x24),
    badge_steer_text: Color::from_rgb8(0xe0, 0xae, 0x6a),
    badge_queue_bg: Color::from_rgb8(0x23, 0x27, 0x2b),
    badge_queue_border: Color::from_rgb8(0x3a, 0x40, 0x45),
    badge_queue_text: Color::from_rgb8(0xae, 0xb3, 0xb0),
    status_ok: Color::from_rgb8(0x56, 0xd3, 0x64),
    status_busy: Color::from_rgb8(0xe5, 0xb5, 0x67),
    status_idle: Color::from_rgb8(0x68, 0x73, 0x6f),
    status_unread: Color::from_rgb8(0x62, 0xbc, 0xeb),
    error_text: Color::from_rgb8(0xf8, 0x51, 0x49),
    error_card_bg: Color::from_rgba8(0xf8, 0x51, 0x49, 0.08),
    error_card_border: Color::from_rgba8(0xf8, 0x51, 0x49, 0.32),
    user_message_bg: Color::from_rgb8(0x1c, 0x24, 0x2b),
    message_border: Color::from_rgba8(0xff, 0xff, 0xff, 0.055),
    quote_border: Color::from_rgb8(0x6f, 0x77, 0x80),
    quote_text: Color::from_rgb8(0xbc, 0xc1, 0xc4),
    code_block_bg: Color::from_rgb8(0x17, 0x1a, 0x1d),
    code_block_border: Color::from_rgb8(0x30, 0x35, 0x3a),
    code_header_bg: Color::from_rgb8(0x14, 0x17, 0x1a),
    code_language_text: Color::from_rgb8(0x89, 0x91, 0x98),
    code_content_text: Color::from_rgb8(0xe1, 0xdd, 0xd5),
    overlay_bg: Color::from_rgba8(0xff, 0xff, 0xff, 0.02),
    overlay_border: Color::from_rgba8(0xff, 0xff, 0xff, 0.04),
    badge_overlay_bg: Color::from_rgba8(0xff, 0xff, 0xff, 0.05),
};

/// The palette for the active COSMIC theme; light desktops get [`LIGHT`].
#[must_use]
pub fn current() -> &'static Palette {
    if cosmic::theme::is_dark() {
        &DARK
    } else {
        &LIGHT
    }
}
