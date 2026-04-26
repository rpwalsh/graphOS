// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
pub type Color = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ThemeTone {
    Dark = 0,
    Light = 1,
    HighContrast = 2,
}

impl ThemeTone {
    pub const fn as_bytes(self) -> &'static [u8] {
        match self {
            Self::Dark => b"dark",
            Self::Light => b"light",
            Self::HighContrast => b"high-contrast",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ThemeTokens {
    pub background: Color,
    pub surface: Color,
    pub surface_muted: Color,
    pub border: Color,
    pub text: Color,
    pub text_muted: Color,
    pub accent: Color,
    pub accent_soft: Color,
    pub positive: Color,
    pub warning: Color,
    pub danger: Color,
    pub nav_width: u16,
    pub inspector_width: u16,
    pub topbar_height: u16,
    pub status_height: u16,
    pub radius: u8,
    pub gap: u8,
    pub panel_padding: u8,
}

pub const DARK_THEME: ThemeTokens = ThemeTokens {
    background: 0x000a111b,
    surface: 0x00101926,
    surface_muted: 0x00162130,
    border: 0x00304158,
    text: 0x00e7eef8,
    text_muted: 0x0098abc5,
    accent: 0x0059a6ff,
    accent_soft: 0x001b3b5f,
    positive: 0x0044c08c,
    warning: 0x00efbb51,
    danger: 0x00e46b6b,
    nav_width: 248,
    inspector_width: 320,
    topbar_height: 72,
    status_height: 36,
    radius: 16,
    gap: 16,
    panel_padding: 16,
};

pub const LIGHT_THEME: ThemeTokens = ThemeTokens {
    background: 0x00edf2f8,
    surface: 0x00fefefe,
    surface_muted: 0x00e4ecf6,
    border: 0x00c5d2e1,
    text: 0x00152233,
    text_muted: 0x00596d88,
    accent: 0x003f74ff,
    accent_soft: 0x00d7e3ff,
    positive: 0x001f936a,
    warning: 0x00bf8618,
    danger: 0x00c75252,
    nav_width: 248,
    inspector_width: 320,
    topbar_height: 72,
    status_height: 36,
    radius: 16,
    gap: 16,
    panel_padding: 16,
};

pub const HIGH_CONTRAST_THEME: ThemeTokens = ThemeTokens {
    background: 0x00050a12,
    surface: 0x000d141d,
    surface_muted: 0x00121b26,
    border: 0x00a2b6d6,
    text: 0x00ffffff,
    text_muted: 0x00d8e1ee,
    accent: 0x007fbbff,
    accent_soft: 0x00213b58,
    positive: 0x0060efb0,
    warning: 0x00ffda78,
    danger: 0x00ffacac,
    nav_width: 248,
    inspector_width: 320,
    topbar_height: 72,
    status_height: 36,
    radius: 16,
    gap: 16,
    panel_padding: 16,
};

pub const fn resolve_theme(tone: ThemeTone) -> ThemeTokens {
    match tone {
        ThemeTone::Dark => DARK_THEME,
        ThemeTone::Light => LIGHT_THEME,
        ThemeTone::HighContrast => HIGH_CONTRAST_THEME,
    }
}

pub fn series_color(theme: &ThemeTokens, index: usize) -> Color {
    const OFFSETS: [u32; 8] = [
        0x00000000, 0x00181200, 0x00001812, 0x00180018, 0x00001218, 0x00181800, 0x000a0a12,
        0x00120a0a,
    ];
    theme.accent.wrapping_add(OFFSETS[index % OFFSETS.len()])
}
