// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
//! Minimal toolkit contract (GraphOS GTK-equivalent).
//!
//! This module intentionally exposes only essential primitives required by
//! GraphOS shell/apps and avoids a generic "widget zoo".

use crate::substrate::UiEvent;

/// Essential widget classes supported in the first toolkit milestone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum EssentialWidget {
    /// Box layout container.
    BoxLayout = 0,
    /// Overlay stack container.
    Overlay = 1,
    /// Split pane container.
    SplitPane = 2,
    /// Basic button control.
    Button = 3,
    /// Text label.
    Label = 4,
    /// Text input.
    TextInput = 5,
    /// Scroll container.
    Scroll = 6,
    /// Table/list view.
    TableList = 7,
    /// Window frame/chrome.
    WindowFrame = 8,
    /// Tabs/docking strip.
    TabsDock = 9,
    /// Command palette.
    CommandPalette = 10,
    /// Toast/status strip.
    StatusToast = 11,
    /// GraphOS-native graph canvas.
    GraphView = 12,
    /// GraphOS-native timeline view.
    TimelineView = 13,
    /// GraphOS-native inspector view.
    InspectorView = 14,
}

/// Toolkit profile declaring the exact enabled surface area.
#[derive(Clone, Copy, Debug)]
pub struct ToolkitProfile {
    /// Enabled widget kinds.
    pub enabled: &'static [EssentialWidget],
}

impl ToolkitProfile {
    /// Essential GraphOS profile.
    pub const fn essential() -> Self {
        Self {
            enabled: &[
                EssentialWidget::BoxLayout,
                EssentialWidget::Overlay,
                EssentialWidget::SplitPane,
                EssentialWidget::Button,
                EssentialWidget::Label,
                EssentialWidget::TextInput,
                EssentialWidget::Scroll,
                EssentialWidget::TableList,
                EssentialWidget::WindowFrame,
                EssentialWidget::TabsDock,
                EssentialWidget::CommandPalette,
                EssentialWidget::StatusToast,
                EssentialWidget::GraphView,
                EssentialWidget::TimelineView,
                EssentialWidget::InspectorView,
            ],
        }
    }
}

/// Keyboard focus model for ordered focusable controls.
#[derive(Clone, Copy, Debug)]
pub struct FocusModel {
    /// Focused index.
    pub focused: usize,
    /// Number of focusable items.
    pub total: usize,
}

impl FocusModel {
    /// Create focus model.
    pub const fn new(total: usize) -> Self {
        Self { focused: 0, total }
    }

    /// Move focus to next item.
    pub fn next(&mut self) {
        if self.total == 0 {
            return;
        }
        self.focused = (self.focused + 1) % self.total;
    }

    /// Move focus to previous item.
    pub fn prev(&mut self) {
        if self.total == 0 {
            return;
        }
        self.focused = if self.focused == 0 {
            self.total - 1
        } else {
            self.focused - 1
        };
    }

    /// Update focus with keyboard navigation events.
    ///
    /// Uses Tab and arrow-up/down as baseline controls.
    pub fn handle_event(&mut self, event: UiEvent) {
        match event {
            UiEvent::Key(key) if key.pressed && key.ascii == b'\t' => self.next(),
            UiEvent::Key(key) if key.pressed && key.hid_usage == 0x51 => self.next(),
            UiEvent::Key(key) if key.pressed && key.hid_usage == 0x52 => self.prev(),
            _ => {}
        }
    }
}
