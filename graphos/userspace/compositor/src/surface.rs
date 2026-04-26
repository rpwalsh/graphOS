// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
use crate::geom::Rect;
use crate::theme::Color;

pub const MAX_SURFACES: usize = 12;
pub const SURFACE_TITLE_CAP: usize = 40;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum SurfaceKind {
    Empty = 0,
    Navigation = 1,
    Topbar = 2,
    Workspace = 3,
    Chart = 4,
    Console = 5,
    Inspector = 6,
    StatusBar = 7,
}

impl SurfaceKind {
    pub const fn as_bytes(self) -> &'static [u8] {
        match self {
            Self::Empty => b"empty",
            Self::Navigation => b"navigation",
            Self::Topbar => b"topbar",
            Self::Workspace => b"workspace",
            Self::Chart => b"chart",
            Self::Console => b"console",
            Self::Inspector => b"inspector",
            Self::StatusBar => b"status-bar",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SurfaceRecord {
    pub id: u16,
    pub kind: SurfaceKind,
    pub bounds: Rect,
    pub visible: bool,
    pub dirty: bool,
    pub focusable: bool,
    pub focused: bool,
    pub accent: Color,
    title: [u8; SURFACE_TITLE_CAP],
    title_len: u8,
}

impl SurfaceRecord {
    pub const fn empty() -> Self {
        Self {
            id: 0,
            kind: SurfaceKind::Empty,
            bounds: Rect::new(0, 0, 0, 0),
            visible: false,
            dirty: false,
            focusable: false,
            focused: false,
            accent: 0,
            title: [0u8; SURFACE_TITLE_CAP],
            title_len: 0,
        }
    }

    pub fn new(
        id: u16,
        kind: SurfaceKind,
        bounds: Rect,
        title: &[u8],
        accent: Color,
        focusable: bool,
    ) -> Self {
        let mut record = Self {
            id,
            kind,
            bounds,
            visible: true,
            dirty: true,
            focusable,
            focused: false,
            accent,
            title: [0u8; SURFACE_TITLE_CAP],
            title_len: 0,
        };
        record.set_title(title);
        record
    }

    pub fn title(&self) -> &[u8] {
        &self.title[..self.title_len as usize]
    }

    pub fn set_title(&mut self, title: &[u8]) {
        let len = title.len().min(SURFACE_TITLE_CAP);
        self.title[..len].copy_from_slice(&title[..len]);
        if len < SURFACE_TITLE_CAP {
            self.title[len..].fill(0);
        }
        self.title_len = len as u8;
    }

    pub const fn area(&self) -> u32 {
        self.bounds.w as u32 * self.bounds.h as u32
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SurfaceRegistry {
    surfaces: [SurfaceRecord; MAX_SURFACES],
    len: usize,
}

impl SurfaceRegistry {
    pub const fn new() -> Self {
        Self {
            surfaces: [SurfaceRecord::empty(); MAX_SURFACES],
            len: 0,
        }
    }

    pub fn clear(&mut self) {
        self.surfaces = [SurfaceRecord::empty(); MAX_SURFACES];
        self.len = 0;
    }

    pub fn push(&mut self, surface: SurfaceRecord) -> Option<u16> {
        if self.len >= MAX_SURFACES {
            return None;
        }
        self.surfaces[self.len] = surface;
        self.len += 1;
        Some(surface.id)
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn iter(&self) -> core::slice::Iter<'_, SurfaceRecord> {
        self.surfaces[..self.len].iter()
    }

    pub fn iter_mut(&mut self) -> core::slice::IterMut<'_, SurfaceRecord> {
        self.surfaces[..self.len].iter_mut()
    }

    pub fn get(&self, id: u16) -> Option<&SurfaceRecord> {
        self.iter().find(|surface| surface.id == id)
    }

    pub fn get_mut(&mut self, id: u16) -> Option<&mut SurfaceRecord> {
        self.iter_mut().find(|surface| surface.id == id)
    }

    pub fn get_index_mut(&mut self, index: usize) -> Option<&mut SurfaceRecord> {
        self.surfaces[..self.len].get_mut(index)
    }

    pub fn visible_count(&self) -> usize {
        self.iter().filter(|surface| surface.visible).count()
    }

    pub fn dirty_count(&self) -> usize {
        self.iter().filter(|surface| surface.dirty).count()
    }

    pub fn focusable_visible_count(&self) -> usize {
        self.iter()
            .filter(|surface| surface.visible && surface.focusable)
            .count()
    }

    pub fn focused_surface(&self) -> Option<&SurfaceRecord> {
        self.iter().find(|surface| surface.focused)
    }
}
