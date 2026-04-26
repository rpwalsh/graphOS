// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
use crate::theme::Color;

pub const MAX_CHARTS: usize = 8;
pub const CHART_TITLE_CAP: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ChartKind {
    Empty = 0,
    NodeGraph = 1,
    Timeline = 2,
    Line = 3,
    Bar = 4,
}

impl ChartKind {
    pub const fn as_bytes(self) -> &'static [u8] {
        match self {
            Self::Empty => b"empty",
            Self::NodeGraph => b"node-graph",
            Self::Timeline => b"timeline",
            Self::Line => b"line",
            Self::Bar => b"bar",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ChartDefinition {
    pub id: u16,
    pub surface_id: u16,
    pub kind: ChartKind,
    pub series_count: u8,
    pub telemetry_slots: u8,
    pub accent: Color,
    title: [u8; CHART_TITLE_CAP],
    title_len: u8,
}

impl ChartDefinition {
    pub const fn empty() -> Self {
        Self {
            id: 0,
            surface_id: 0,
            kind: ChartKind::Empty,
            series_count: 0,
            telemetry_slots: 0,
            accent: 0,
            title: [0u8; CHART_TITLE_CAP],
            title_len: 0,
        }
    }

    pub fn new(
        id: u16,
        surface_id: u16,
        kind: ChartKind,
        title: &[u8],
        series_count: u8,
        telemetry_slots: u8,
        accent: Color,
    ) -> Self {
        let mut definition = Self {
            id,
            surface_id,
            kind,
            series_count,
            telemetry_slots,
            accent,
            title: [0u8; CHART_TITLE_CAP],
            title_len: 0,
        };
        definition.set_title(title);
        definition
    }

    pub fn title(&self) -> &[u8] {
        &self.title[..self.title_len as usize]
    }

    pub fn set_title(&mut self, title: &[u8]) {
        let len = title.len().min(CHART_TITLE_CAP);
        self.title[..len].copy_from_slice(&title[..len]);
        if len < CHART_TITLE_CAP {
            self.title[len..].fill(0);
        }
        self.title_len = len as u8;
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ChartRegistry {
    charts: [ChartDefinition; MAX_CHARTS],
    len: usize,
}

impl ChartRegistry {
    pub const fn new() -> Self {
        Self {
            charts: [ChartDefinition::empty(); MAX_CHARTS],
            len: 0,
        }
    }

    pub fn clear(&mut self) {
        self.charts = [ChartDefinition::empty(); MAX_CHARTS];
        self.len = 0;
    }

    pub fn push(&mut self, chart: ChartDefinition) -> Option<u16> {
        if self.len >= MAX_CHARTS {
            return None;
        }
        self.charts[self.len] = chart;
        self.len += 1;
        Some(chart.id)
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn iter(&self) -> core::slice::Iter<'_, ChartDefinition> {
        self.charts[..self.len].iter()
    }
}
