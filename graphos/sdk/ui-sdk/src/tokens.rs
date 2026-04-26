// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Theme tokens for Phase J ring3 surfaces.

/// Supported toolkit themes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    /// Dark glass desktop theme.
    DarkGlass,
    /// Light frost desktop theme.
    LightFrost,
    /// High contrast accessibility theme.
    HighContrast,
}

/// Colour tokens consumed by widgets and charts.
#[derive(Clone, Copy, Debug)]
pub struct ThemeTokens {
    /// Window or app background.
    pub background: u32,
    /// Primary surface fill.
    pub surface: u32,
    /// Raised surface fill.
    pub surface_alt: u32,
    /// Border/stroke colour.
    pub border: u32,
    /// Strong text.
    pub text: u32,
    /// Muted text.
    pub text_muted: u32,
    /// Accent/primary colour.
    pub primary: u32,
    /// Positive state.
    pub success: u32,
    /// Warning state.
    pub warning: u32,
    /// Danger state.
    pub danger: u32,
    /// Input or command-bar surface.
    pub chrome: u32,
    /// Chart grid colour.
    pub grid: u32,
    /// Chart palette.
    pub palette: &'static [u32],
}

const DARK_PALETTE: [u32; 6] = [
    0xFF58A6FF, 0xFF39C5BB, 0xFFD29922, 0xFFF78166, 0xFFA371F7, 0xFF3FB950,
];

const LIGHT_PALETTE: [u32; 6] = [
    0xFF0969DA, 0xFF1B7F83, 0xFF9A6700, 0xFFCF222E, 0xFF8250DF, 0xFF1A7F37,
];

const HC_PALETTE: [u32; 4] = [0xFFFFFF00, 0xFF00FFFF, 0xFFFF00FF, 0xFFFFFFFF];

/// Resolve tokens for a theme.
pub const fn tokens(theme: Theme) -> ThemeTokens {
    match theme {
        Theme::DarkGlass => ThemeTokens {
            background: 0xFF0D1117,
            surface: 0xFF161B22,
            surface_alt: 0xFF1F2630,
            border: 0xFF2F3946,
            text: 0xFFE6EDF3,
            text_muted: 0xFF9AA4B2,
            primary: 0xFF58A6FF,
            success: 0xFF3FB950,
            warning: 0xFFD29922,
            danger: 0xFFF85149,
            chrome: 0xFF0B141C,
            grid: 0xFF283341,
            palette: &DARK_PALETTE,
        },
        Theme::LightFrost => ThemeTokens {
            background: 0xFFF6F8FA,
            surface: 0xFFFFFFFF,
            surface_alt: 0xFFF0F3F6,
            border: 0xFFD0D7DE,
            text: 0xFF1F2328,
            text_muted: 0xFF57606A,
            primary: 0xFF0969DA,
            success: 0xFF1A7F37,
            warning: 0xFF9A6700,
            danger: 0xFFCF222E,
            chrome: 0xFFEAEFF5,
            grid: 0xFFD8DEE4,
            palette: &LIGHT_PALETTE,
        },
        Theme::HighContrast => ThemeTokens {
            background: 0xFF000000,
            surface: 0xFF000000,
            surface_alt: 0xFF000000,
            border: 0xFFFFFFFF,
            text: 0xFFFFFFFF,
            text_muted: 0xFFFFFFFF,
            primary: 0xFFFFFF00,
            success: 0xFF00FF00,
            warning: 0xFFFFFF00,
            danger: 0xFFFF0000,
            chrome: 0xFF000000,
            grid: 0xFFFFFFFF,
            palette: &HC_PALETTE,
        },
    }
}

/// Shared spacing scale.
pub mod space {
    /// 4px.
    pub const XS: u32 = 4;
    /// 8px.
    pub const SM: u32 = 8;
    /// 12px.
    pub const MD: u32 = 12;
    /// 16px.
    pub const LG: u32 = 16;
    /// 24px.
    pub const XL: u32 = 24;
}
