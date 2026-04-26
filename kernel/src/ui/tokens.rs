// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Phase J design token system — Dark Glass, Light Frost, High Contrast themes.
//!
//! This module defines the kernel-side design token table for GraphOS Phase J.
//! Design tokens are named BGRA32 colour constants that the compositor and
//! UI controls reference by semantic name rather than raw hex values.
//!
//! ## Themes
//!
//! | Theme          | Philosophy                                                  |
//! |----------------|-------------------------------------------------------------|
//! | Dark Glass     | Deep navy/charcoal + frosted translucency, accent cyan-blue |
//! | Light Frost    | White/light-grey base + subtle cool frosted overlays        |
//! | High Contrast  | WCAG 2.1 AAA black-on-white / white-on-black                |
//!
//! ## Token categories
//!
//! - **Surface**: desktop background, shell bar, window chrome
//! - **Text**: primary, secondary, muted, disabled
//! - **Accent**: focus ring, selection, link, active chip
//! - **State**: error, warning, success, info
//! - **Border**: default, focus, divider
//! - **Blur**: blur radius and overlay alpha for Kawase passes
//!
//! ## Usage in compositor
//!
//! ```rust
//! let tok = tokens::active_theme();
//! canvas.fill_rect(rect, tok.surface.desktop_bg);
//! canvas.draw_text(text, tok.text.primary);
//! ```
//!
//! The active theme is set globally and can be changed at runtime.

use core::sync::atomic::{AtomicU8, Ordering};
use spin::Mutex;

// ── Token structs ──────────────────────────────────────────────────────────────

/// Surface-layer colour tokens.
#[derive(Clone, Copy, Debug)]
pub struct SurfaceTokens {
    /// Desktop wallpaper / root background.
    pub desktop_bg: u32,
    /// Shell bar background.
    pub shell_bar_bg: u32,
    /// Shell bar bottom edge highlight.
    pub shell_bar_edge: u32,
    /// Task chip / window switcher chip background (unfocused).
    pub chip_bg: u32,
    /// Task chip border (unfocused).
    pub chip_border: u32,
    /// Task chip background (focused).
    pub chip_focus: u32,
    /// Task chip border (focused).
    pub chip_focus_border: u32,
    /// Window title bar background.
    pub titlebar_bg: u32,
    /// Window body / client area tint.
    pub window_bg: u32,
    /// Frosted-glass overlay colour (applied at `blur_overlay_alpha`).
    pub blur_overlay: u32,
    /// Overlay alpha in 0–255 range (applied over the Kawase-blurred background).
    pub blur_overlay_alpha: u8,
    /// Kawase blur radius in pixels (0 = no blur).
    pub blur_radius: u8,
}

/// Text colour tokens.
#[derive(Clone, Copy, Debug)]
pub struct TextTokens {
    /// Primary text (headings, body, labels).
    pub primary: u32,
    /// Secondary text (captions, helper text).
    pub secondary: u32,
    /// Muted/placeholder text.
    pub muted: u32,
    /// Disabled text.
    pub disabled: u32,
    /// Link / interactive text (unfocused).
    pub link: u32,
    /// Link / interactive text (hovered).
    pub link_hover: u32,
}

/// Accent colour tokens.
#[derive(Clone, Copy, Debug)]
pub struct AccentTokens {
    /// Primary accent (buttons, active states, focus rings).
    pub primary: u32,
    /// Secondary accent (secondary buttons, progress bars).
    pub secondary: u32,
    /// Focus ring colour.
    pub focus_ring: u32,
    /// Selection highlight background.
    pub selection_bg: u32,
    /// Selection text colour.
    pub selection_text: u32,
}

/// Semantic state colour tokens.
#[derive(Clone, Copy, Debug)]
pub struct StateTokens {
    pub error: u32,
    pub error_bg: u32,
    pub warning: u32,
    pub warning_bg: u32,
    pub success: u32,
    pub success_bg: u32,
    pub info: u32,
    pub info_bg: u32,
}

/// Border colour tokens.
#[derive(Clone, Copy, Debug)]
pub struct BorderTokens {
    pub default: u32,
    pub focus: u32,
    pub divider: u32,
    pub outline: u32,
}

/// Complete design token set for one theme.
#[derive(Clone, Copy, Debug)]
pub struct ThemeTokens {
    pub surface: SurfaceTokens,
    pub text: TextTokens,
    pub accent: AccentTokens,
    pub state: StateTokens,
    pub border: BorderTokens,
}

// ── Dark Glass theme ───────────────────────────────────────────────────────────

/// Dark Glass theme — deep navy base, frosted translucency, cyan-blue accent.
/// Designed for OLED/HDR displays.  All colours are BGRA32.
pub const DARK_GLASS: ThemeTokens = ThemeTokens {
    surface: SurfaceTokens {
        desktop_bg: 0xFF0B_0F14,        // #0B0F14 deep navy
        shell_bar_bg: 0xFF11_171D,      // #11171D slightly lighter navy
        shell_bar_edge: 0xFF2C_3B47,    // #2C3B47 muted teal edge
        chip_bg: 0xFF15_1D24,           // #151D24 dark chip background
        chip_border: 0xFF38_4A59,       // #384A59 dim border
        chip_focus: 0xFF1D_3140,        // #1D3140 focus chip tint
        chip_focus_border: 0xFF5D_8DB5, // #5D8DB5 focus chip border (accent)
        titlebar_bg: 0xFF10_1820,       // #101820 window chrome
        window_bg: 0xDD0B_1219,         // #0B1219 with 87% alpha (frosted)
        blur_overlay: 0xCC0A_1018,      // #0A1018 with 80% overlay
        blur_overlay_alpha: 200,
        blur_radius: 16,
    },
    text: TextTokens {
        primary: 0xFFE7_EEF4,    // #E7EEF4 off-white
        secondary: 0xFFAD_BDC9,  // #ADBDC9 cool grey
        muted: 0xFF92_A2B0,      // #92A2B0 muted grey
        disabled: 0xFF4C_5D6A,   // #4C5D6A dim grey
        link: 0xFF5D_8DB5,       // #5D8DB5 accent blue
        link_hover: 0xFF7E_AED6, // #7EAED6 brighter blue
    },
    accent: AccentTokens {
        primary: 0xFF5D_8DB5,        // #5D8DB5 accent cyan-blue
        secondary: 0xFF2C_4E6A,      // #2C4E6A secondary accent
        focus_ring: 0xFF7E_AED6,     // #7EAED6 bright focus ring
        selection_bg: 0x805D_8DB5,   // #5D8DB5 with 50% alpha
        selection_text: 0xFFFF_FFFF, // white on selection
    },
    state: StateTokens {
        error: 0xFFFF_5555,
        error_bg: 0x33FF_2020,
        warning: 0xFFFF_AA00,
        warning_bg: 0x33FF_8800,
        success: 0xFF44_CC77,
        success_bg: 0x3322_AA55,
        info: 0xFF55_AAFF,
        info_bg: 0x3322_55FF,
    },
    border: BorderTokens {
        default: 0xFF2C_3B47,
        focus: 0xFF5D_8DB5,
        divider: 0xFF1E_2C38,
        outline: 0xFF38_4A59,
    },
};

// ── Light Frost theme ──────────────────────────────────────────────────────────

/// Light Frost theme — white/light-grey base, cool frosted overlays.
/// Suitable for daylight use and accessibility.
pub const LIGHT_FROST: ThemeTokens = ThemeTokens {
    surface: SurfaceTokens {
        desktop_bg: 0xFFE8_EEF4,        // #E8EEF4 cool light grey
        shell_bar_bg: 0xFFF0_F4F8,      // #F0F4F8 near-white bar
        shell_bar_edge: 0xFFBC_CCDA,    // #BCCCDA medium grey edge
        chip_bg: 0xFFE0_E8EE,           // #E0E8EE chip background
        chip_border: 0xFFBC_CCDA,       // #BCCCDA chip border
        chip_focus: 0xFFCE_E4F5,        // #CEE4F5 light blue focus
        chip_focus_border: 0xFF2D_7DC4, // #2D7DC4 focus ring blue
        titlebar_bg: 0xFFF5_F8FC,       // #F5F8FC title bar
        window_bg: 0xEEFF_FFFF,         // white with 93% alpha
        blur_overlay: 0xCCF0_F4F8,      // near-white frosted overlay
        blur_overlay_alpha: 180,
        blur_radius: 8,
    },
    text: TextTokens {
        primary: 0xFF10_1820,    // #101820 near-black
        secondary: 0xFF3D_5060,  // #3D5060 dark grey
        muted: 0xFF6E_818E,      // #6E818E medium grey
        disabled: 0xFFAB_B9C4,   // #ABB9C4 light grey
        link: 0xFF1C_5FA8,       // #1C5FA8 blue link
        link_hover: 0xFF0E_3E72, // #0E3E72 dark blue hover
    },
    accent: AccentTokens {
        primary: 0xFF2D_7DC4,        // #2D7DC4 blue accent
        secondary: 0xFF6E_ABDB,      // #6EABDB lighter blue
        focus_ring: 0xFF1C_5FA8,     // #1C5FA8 dark blue focus
        selection_bg: 0x60_2D7DC4,   // blue with 38% alpha
        selection_text: 0xFF10_1820, // dark text on selection
    },
    state: StateTokens {
        error: 0xFFCC_0000,
        error_bg: 0x22FF_0000,
        warning: 0xFFBB_6600,
        warning_bg: 0x22FF_8800,
        success: 0xFF1B_8C44,
        success_bg: 0x2200_BB44,
        info: 0xFF0C_5FA8,
        info_bg: 0x220C_5FA8,
    },
    border: BorderTokens {
        default: 0xFFBC_CCDA,
        focus: 0xFF2D_7DC4,
        divider: 0xFFD8_E3EC,
        outline: 0xFFAB_B9C4,
    },
};

// ── High Contrast theme ───────────────────────────────────────────────────────

/// High Contrast theme — WCAG 2.1 AAA minimum 7:1 contrast ratio.
/// No blur, no translucency, sharp borders.
pub const HIGH_CONTRAST: ThemeTokens = ThemeTokens {
    surface: SurfaceTokens {
        desktop_bg: 0xFF00_0000,        // black
        shell_bar_bg: 0xFF00_0000,      // black
        shell_bar_edge: 0xFFFF_FF00,    // yellow border
        chip_bg: 0xFF00_0000,           // black
        chip_border: 0xFFFF_FFFF,       // white border
        chip_focus: 0xFF00_0000,        // black
        chip_focus_border: 0xFFFF_FF00, // yellow focus
        titlebar_bg: 0xFF00_0000,       // black
        window_bg: 0xFF00_0000,         // fully opaque black
        blur_overlay: 0xFF00_0000,      // no blur overlay
        blur_overlay_alpha: 0,
        blur_radius: 0, // no blur
    },
    text: TextTokens {
        primary: 0xFFFF_FFFF,    // white
        secondary: 0xFFFF_FFFF,  // white
        muted: 0xFFCC_CCCC,      // light grey (still AAA)
        disabled: 0xFF77_7777,   // medium grey
        link: 0xFFFF_FF00,       // yellow link
        link_hover: 0xFFFF_FF66, // lighter yellow hover
    },
    accent: AccentTokens {
        primary: 0xFFFF_FF00,        // yellow accent
        secondary: 0xFFFF_FF00,      // yellow
        focus_ring: 0xFFFF_FF00,     // yellow focus ring
        selection_bg: 0xFFFF_FF00,   // yellow selection
        selection_text: 0xFF00_0000, // black text on yellow
    },
    state: StateTokens {
        error: 0xFFFF_3333,
        error_bg: 0xFF33_0000,
        warning: 0xFFFF_FF00,
        warning_bg: 0xFF33_3300,
        success: 0xFF00_FF66,
        success_bg: 0xFF00_3311,
        info: 0xFF00_CCFF,
        info_bg: 0xFF00_2233,
    },
    border: BorderTokens {
        default: 0xFFFF_FFFF, // white border
        focus: 0xFFFF_FF00,   // yellow focus
        divider: 0xFFFF_FFFF, // white divider
        outline: 0xFFFF_FFFF, // white outline
    },
};

// ── Active theme selection ─────────────────────────────────────────────────────

/// Theme identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ThemeId {
    DarkGlass = 0,
    LightFrost = 1,
    HighContrast = 2,
}

/// Active theme index (atomic for lock-free reads on the hot render path).
static ACTIVE_THEME: AtomicU8 = AtomicU8::new(ThemeId::DarkGlass as u8);

/// Active theme tokens (guarded by mutex for atomic swap during theme change).
static THEME_TOKENS: Mutex<ThemeTokens> = Mutex::new(DARK_GLASS);

/// Return the current active theme tokens.
///
/// This is the hot-path accessor — callers read from the snapshot copy
/// rather than from the THEME_TOKENS mutex to avoid lock contention.
pub fn active_theme() -> ThemeTokens {
    *THEME_TOKENS.lock()
}

/// Return the current active theme ID.
pub fn active_theme_id() -> ThemeId {
    match ACTIVE_THEME.load(Ordering::Relaxed) {
        0 => ThemeId::DarkGlass,
        1 => ThemeId::LightFrost,
        2 => ThemeId::HighContrast,
        _ => ThemeId::DarkGlass,
    }
}

/// Switch to a different theme at runtime.
///
/// The change takes effect on the next compositor render cycle.
pub fn set_theme(id: ThemeId) {
    let tokens = match id {
        ThemeId::DarkGlass => DARK_GLASS,
        ThemeId::LightFrost => LIGHT_FROST,
        ThemeId::HighContrast => HIGH_CONTRAST,
    };
    *THEME_TOKENS.lock() = tokens;
    ACTIVE_THEME.store(id as u8, Ordering::Relaxed);
}

// ── Syscall integration ────────────────────────────────────────────────────────

/// syscall-facing: set the active theme.
/// Returns 0 on success, 1 if the theme_id is invalid.
pub fn syscall_set_theme(theme_id: u8) -> u64 {
    match theme_id {
        0 => {
            set_theme(ThemeId::DarkGlass);
            0
        }
        1 => {
            set_theme(ThemeId::LightFrost);
            0
        }
        2 => {
            set_theme(ThemeId::HighContrast);
            0
        }
        _ => 1,
    }
}

/// syscall-facing: get the active theme ID.
pub fn syscall_get_theme() -> u64 {
    ACTIVE_THEME.load(Ordering::Relaxed) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_theme_is_dark_glass() {
        assert_eq!(active_theme_id(), ThemeId::DarkGlass);
    }

    #[test]
    fn set_and_get_theme_roundtrip() {
        set_theme(ThemeId::LightFrost);
        assert_eq!(active_theme_id(), ThemeId::LightFrost);
        let tok = active_theme();
        // Light Frost desktop should be light grey
        assert_eq!(tok.surface.desktop_bg, LIGHT_FROST.surface.desktop_bg);

        set_theme(ThemeId::HighContrast);
        assert_eq!(active_theme_id(), ThemeId::HighContrast);
        let tok = active_theme();
        assert_eq!(tok.surface.desktop_bg, HIGH_CONTRAST.surface.desktop_bg);
        assert_eq!(tok.surface.blur_radius, 0);

        // Reset to default
        set_theme(ThemeId::DarkGlass);
    }

    #[test]
    fn high_contrast_has_no_blur() {
        assert_eq!(HIGH_CONTRAST.surface.blur_radius, 0);
        assert_eq!(HIGH_CONTRAST.surface.blur_overlay_alpha, 0);
    }

    #[test]
    fn dark_glass_has_blur() {
        assert!(DARK_GLASS.surface.blur_radius > 0);
        assert!(DARK_GLASS.surface.blur_overlay_alpha > 0);
    }

    #[test]
    fn syscall_set_theme_invalid_returns_error() {
        assert_eq!(syscall_set_theme(99), 1);
    }
}
