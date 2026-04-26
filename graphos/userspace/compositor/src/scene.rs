// Copyright (c) 2024-2026 Ryan P. Walsh. All rights reserved.
use crate::charts::{ChartDefinition, ChartKind, ChartRegistry};
use crate::geom::Rect;
use crate::material::{GlassMaterial, GradientMaterial, Material};
use crate::render_node::{NodeKind, RenderScene, Transform3D};
use crate::surface::{SurfaceKind, SurfaceRecord, SurfaceRegistry};
use crate::theme::{ThemeTokens, ThemeTone, resolve_theme, series_color};

const SNAPSHOT_MAGIC: &[u8] = b"graphos-compositor-v1\n";
const WORKSPACE_FOCUS_START: usize = 2;

#[derive(Clone, Copy, Debug, Default)]
pub struct SceneTelemetry {
    pub surfaces: u16,
    pub visible_surfaces: u16,
    pub dirty_surfaces: u16,
    pub focusable_surfaces: u16,
    pub charts: u16,
    pub focused_surface: u16,
    pub workspace_pixels: u32,
    pub scene_epoch: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct CompositorState {
    pub width: u16,
    pub height: u16,
    pub tone: ThemeTone,
    pub theme: ThemeTokens,
    pub surfaces: SurfaceRegistry,
    pub charts: ChartRegistry,
    scene_epoch: u32,
    focus_cursor: usize,
    inspector_surface_id: u16,
}

impl CompositorState {
    pub fn new(width: u16, height: u16, tone: ThemeTone) -> Self {
        let mut state = Self {
            width,
            height,
            tone,
            theme: resolve_theme(tone),
            surfaces: SurfaceRegistry::new(),
            charts: ChartRegistry::new(),
            scene_epoch: 0,
            focus_cursor: 0,
            inspector_surface_id: 0,
        };
        state.seed_graph_desktop();
        state
    }

    pub fn seed_graph_desktop(&mut self) {
        self.theme = resolve_theme(self.tone);
        self.surfaces.clear();
        self.charts.clear();
        self.scene_epoch = 1;
        self.focus_cursor = 0;
        self.inspector_surface_id = 0;

        let gap = self.theme.gap as u16;
        let nav_width = self.theme.nav_width;
        let inspector_width = self.theme.inspector_width;
        let topbar_height = self.theme.topbar_height;
        let status_height = self.theme.status_height;

        let root_w = self.width;
        let root_h = self.height;
        let body_y = gap;
        let body_h = root_h.saturating_sub(status_height).saturating_sub(gap * 2);
        let main_x = gap.saturating_add(nav_width).saturating_add(gap);
        let main_w = root_w
            .saturating_sub(main_x)
            .saturating_sub(inspector_width)
            .saturating_sub(gap * 2);
        let inspector_x = root_w.saturating_sub(inspector_width).saturating_sub(gap);
        let content_y = body_y.saturating_add(topbar_height).saturating_add(gap);
        let content_h = body_h.saturating_sub(topbar_height).saturating_sub(gap);
        let lower_h = content_h / 3;
        let topology_h = content_h.saturating_sub(lower_h).saturating_sub(gap);
        let lower_y = content_y.saturating_add(topology_h).saturating_add(gap);
        let lower_w = main_w;
        let console_w = lower_w.saturating_mul(3) / 5;
        let timeline_w = lower_w.saturating_sub(console_w).saturating_sub(gap);
        let topology_w = main_w.saturating_mul(2) / 3;
        let health_w = main_w.saturating_sub(topology_w).saturating_sub(gap);
        let timeline_h = lower_h.saturating_sub(gap) / 2;
        let release_h = lower_h.saturating_sub(timeline_h).saturating_sub(gap);
        let timeline_y = lower_y;
        let release_y = timeline_y.saturating_add(timeline_h).saturating_add(gap);

        let nav_id = self.push_surface(
            SurfaceKind::Navigation,
            Rect::new(gap, body_y, nav_width, body_h),
            b"GraphOS",
            self.theme.accent_soft,
            true,
        );
        let topbar_id = self.push_surface(
            SurfaceKind::Topbar,
            Rect::new(
                main_x,
                body_y,
                root_w.saturating_sub(main_x).saturating_sub(gap),
                topbar_height,
            ),
            b"GraphOS operations center",
            self.theme.accent,
            false,
        );
        let topology_id = self.push_surface(
            SurfaceKind::Chart,
            Rect::new(main_x, content_y, topology_w, topology_h),
            b"Service topology",
            self.theme.accent,
            true,
        );
        let health_id = self.push_surface(
            SurfaceKind::Chart,
            Rect::new(
                main_x.saturating_add(topology_w).saturating_add(gap),
                content_y,
                health_w,
                topology_h,
            ),
            b"SLO health",
            self.theme.positive,
            true,
        );
        let _console_id = self.push_surface(
            SurfaceKind::Console,
            Rect::new(main_x, lower_y, console_w, lower_h),
            b"Live console",
            self.theme.warning,
            true,
        );
        let timeline_id = self.push_surface(
            SurfaceKind::Chart,
            Rect::new(
                main_x.saturating_add(console_w).saturating_add(gap),
                timeline_y,
                timeline_w,
                timeline_h,
            ),
            b"Activation timeline",
            self.theme.positive,
            true,
        );
        let release_id = self.push_surface(
            SurfaceKind::Workspace,
            Rect::new(
                main_x.saturating_add(console_w).saturating_add(gap),
                release_y,
                timeline_w,
                release_h,
            ),
            b"Release queue",
            series_color(&self.theme, 4),
            true,
        );
        let inspector_id = self.push_surface(
            SurfaceKind::Inspector,
            Rect::new(inspector_x, content_y, inspector_width, content_h),
            b"Control posture",
            series_color(&self.theme, 1),
            true,
        );
        let status_id = self.push_surface(
            SurfaceKind::StatusBar,
            Rect::new(
                gap,
                root_h.saturating_sub(status_height).saturating_sub(gap),
                root_w.saturating_sub(gap * 2),
                status_height,
            ),
            b"SLO target 99.95% | protected services online",
            self.theme.text_muted,
            false,
        );

        self.inspector_surface_id = inspector_id;

        self.push_chart(
            topology_id,
            ChartKind::NodeGraph,
            b"Fabric dependency graph",
            6,
            4,
            self.theme.accent,
        );
        self.push_chart(
            health_id,
            ChartKind::Bar,
            b"Service SLO burn-down",
            4,
            3,
            self.theme.positive,
        );
        self.push_chart(
            timeline_id,
            ChartKind::Timeline,
            b"Boot activation milestones",
            3,
            3,
            self.theme.positive,
        );
        self.push_chart(
            release_id,
            ChartKind::Line,
            b"Deployment queue latency",
            3,
            2,
            series_color(&self.theme, 4),
        );
        self.push_chart(
            inspector_id,
            ChartKind::Line,
            b"Control telemetry",
            2,
            2,
            series_color(&self.theme, 2),
        );

        let _ = topbar_id;
        let _ = status_id;
        self.focus_cursor = WORKSPACE_FOCUS_START.min(self.surfaces.len().saturating_sub(1));
        self.set_focus(topology_id);
        if let Some(nav) = self.surfaces.get_mut(nav_id) {
            nav.dirty = true;
        }
    }

    pub fn focus_next(&mut self) -> Option<u16> {
        if self.surfaces.focusable_visible_count() == 0 {
            return None;
        }

        let total = self.surfaces.len();
        if total == 0 {
            return None;
        }

        let mut cursor = self.focus_cursor;
        for _ in 0..total {
            cursor = (cursor + 1) % total;
            let id = match self.surfaces.get_index_mut(cursor) {
                Some(surface) if surface.visible && surface.focusable => surface.id,
                _ => continue,
            };
            self.focus_cursor = cursor;
            self.set_focus(id);
            return Some(id);
        }

        None
    }

    pub fn focus_surface(&mut self, id: u16) -> bool {
        let mut target_index = None;
        for (index, surface) in self.surfaces.iter().enumerate() {
            if surface.id == id && surface.visible && surface.focusable {
                target_index = Some(index);
                break;
            }
        }

        let Some(index) = target_index else {
            return false;
        };

        self.focus_cursor = index;
        self.set_focus(id);
        true
    }

    pub fn surface_at(&self, x: i32, y: i32) -> Option<u16> {
        for surface in self.surfaces.iter().rev() {
            if !surface.visible {
                continue;
            }
            let rect = surface.bounds;
            let hit = x >= rect.x as i32
                && y >= rect.y as i32
                && x < rect.right() as i32
                && y < rect.bottom() as i32;
            if hit {
                return Some(surface.id);
            }
        }
        None
    }

    pub fn surface_kind(&self, id: u16) -> Option<SurfaceKind> {
        self.surfaces.get(id).map(|surface| surface.kind)
    }

    pub fn toggle_inspector(&mut self) -> bool {
        let inspector_id = self.inspector_surface_id;
        let Some(visible_now) = self
            .surfaces
            .get(inspector_id)
            .map(|surface| surface.visible)
        else {
            return false;
        };

        let visible_after = !visible_now;
        if let Some(surface) = self.surfaces.get_mut(inspector_id) {
            surface.visible = visible_after;
            surface.dirty = true;
            if !visible_after {
                surface.focused = false;
            }
        }

        if visible_after {
            self.set_focus(inspector_id);
        } else {
            let _ = self.focus_next();
        }
        self.scene_epoch = self.scene_epoch.saturating_add(1);
        visible_after
    }

    pub fn mark_dirty(&mut self, id: u16) -> bool {
        let Some(surface) = self.surfaces.get_mut(id) else {
            return false;
        };
        surface.dirty = true;
        self.scene_epoch = self.scene_epoch.saturating_add(1);
        true
    }

    pub fn telemetry(&self) -> SceneTelemetry {
        let visible_surfaces = self.surfaces.visible_count() as u16;
        let dirty_surfaces = self.surfaces.dirty_count() as u16;
        let focusable_surfaces = self.surfaces.focusable_visible_count() as u16;
        let focused_surface = self
            .surfaces
            .focused_surface()
            .map(|surface| surface.id)
            .unwrap_or(0);
        let workspace_pixels = self
            .surfaces
            .iter()
            .filter(|surface| {
                surface.visible
                    && matches!(
                        surface.kind,
                        SurfaceKind::Workspace
                            | SurfaceKind::Chart
                            | SurfaceKind::Console
                            | SurfaceKind::Inspector
                    )
            })
            .fold(0u32, |pixels, surface| {
                pixels.saturating_add(surface.area())
            });

        SceneTelemetry {
            surfaces: self.surfaces.len() as u16,
            visible_surfaces,
            dirty_surfaces,
            focusable_surfaces,
            charts: self.charts.len() as u16,
            focused_surface,
            workspace_pixels,
            scene_epoch: self.scene_epoch,
        }
    }

    pub fn snapshot_bytes(&self, out: &mut [u8]) -> usize {
        let mut cursor = ByteCursor::new(out);
        cursor.push_bytes(SNAPSHOT_MAGIC);
        cursor.push_bytes(b"desktop graph-shell ");
        cursor.push_bytes(b"size=");
        cursor.push_u16(self.width);
        cursor.push_byte(b'x');
        cursor.push_u16(self.height);
        cursor.push_bytes(b" theme=");
        cursor.push_bytes(self.tone.as_bytes());

        let telemetry = self.telemetry();
        cursor.push_bytes(b" epoch=");
        cursor.push_u32(telemetry.scene_epoch);
        cursor.push_bytes(b" surfaces=");
        cursor.push_u16(telemetry.surfaces);
        cursor.push_bytes(b" visible=");
        cursor.push_u16(telemetry.visible_surfaces);
        cursor.push_bytes(b" dirty=");
        cursor.push_u16(telemetry.dirty_surfaces);
        cursor.push_bytes(b" charts=");
        cursor.push_u16(telemetry.charts);
        cursor.push_bytes(b" interactive=");
        cursor.push_u16(telemetry.focusable_surfaces);
        cursor.push_bytes(b" focus=");
        cursor.push_u16(telemetry.focused_surface);
        cursor.push_bytes(b" workspace=");
        cursor.push_u32(telemetry.workspace_pixels);
        cursor.push_byte(b'\n');

        for surface in self.surfaces.iter() {
            cursor.push_bytes(b"surface ");
            cursor.push_u16(surface.id);
            cursor.push_bytes(b" kind=");
            cursor.push_bytes(surface.kind.as_bytes());
            cursor.push_bytes(b" visible=");
            cursor.push_bool(surface.visible);
            cursor.push_bytes(b" dirty=");
            cursor.push_bool(surface.dirty);
            cursor.push_bytes(b" focused=");
            cursor.push_bool(surface.focused);
            cursor.push_bytes(b" bounds=");
            cursor.push_rect(surface.bounds);
            cursor.push_bytes(b" accent=");
            cursor.push_hex(surface.accent);
            cursor.push_bytes(b" title=");
            cursor.push_bytes(surface.title());
            cursor.push_byte(b'\n');
        }

        for chart in self.charts.iter() {
            cursor.push_bytes(b"chart ");
            cursor.push_u16(chart.id);
            cursor.push_bytes(b" kind=");
            cursor.push_bytes(chart.kind.as_bytes());
            cursor.push_bytes(b" surface=");
            cursor.push_u16(chart.surface_id);
            cursor.push_bytes(b" series=");
            cursor.push_u8(chart.series_count);
            cursor.push_bytes(b" telemetry=");
            cursor.push_u8(chart.telemetry_slots);
            cursor.push_bytes(b" accent=");
            cursor.push_hex(chart.accent);
            cursor.push_bytes(b" title=");
            cursor.push_bytes(chart.title());
            cursor.push_byte(b'\n');
        }

        cursor.len()
    }

    pub fn summary_bytes(&self, out: &mut [u8]) -> usize {
        let mut cursor = ByteCursor::new(out);
        let telemetry = self.telemetry();

        cursor.push_bytes(b"scene ");
        cursor.push_u16(self.width);
        cursor.push_byte(b'x');
        cursor.push_u16(self.height);
        cursor.push_bytes(b" theme=");
        cursor.push_bytes(self.tone.as_bytes());
        cursor.push_bytes(b" epoch=");
        cursor.push_u32(telemetry.scene_epoch);
        cursor.push_bytes(b" focus=");
        cursor.push_u16(telemetry.focused_surface);
        cursor.push_byte(b'\n');

        cursor.push_bytes(b"surfaces=");
        cursor.push_u16(telemetry.surfaces);
        cursor.push_bytes(b" visible=");
        cursor.push_u16(telemetry.visible_surfaces);
        cursor.push_bytes(b" dirty=");
        cursor.push_u16(telemetry.dirty_surfaces);
        cursor.push_bytes(b" charts=");
        cursor.push_u16(telemetry.charts);
        cursor.push_byte(b'\n');

        let mut emitted = 0usize;
        for surface in self.surfaces.iter() {
            if !surface.visible || !surface.focusable {
                continue;
            }
            cursor.push_byte(b's');
            cursor.push_u16(surface.id);
            cursor.push_bytes(b" ");
            cursor.push_bytes(surface.kind.as_bytes());
            cursor.push_bytes(b" ");
            cursor.push_rect(surface.bounds);
            cursor.push_bytes(b" ");
            if surface.focused {
                cursor.push_bytes(b"* ");
            }
            cursor.push_bytes(surface.title());
            cursor.push_byte(b'\n');
            emitted += 1;
            if emitted >= 4 {
                break;
            }
        }

        cursor.len()
    }

    fn push_surface(
        &mut self,
        kind: SurfaceKind,
        bounds: Rect,
        title: &[u8],
        accent: u32,
        focusable: bool,
    ) -> u16 {
        let id = (self.surfaces.len() + 1) as u16;
        let _ = self.surfaces.push(SurfaceRecord::new(
            id, kind, bounds, title, accent, focusable,
        ));
        id
    }

    fn push_chart(
        &mut self,
        surface_id: u16,
        kind: ChartKind,
        title: &[u8],
        series_count: u8,
        telemetry_slots: u8,
        accent: u32,
    ) -> u16 {
        let id = (self.charts.len() + 1) as u16;
        let _ = self.charts.push(ChartDefinition::new(
            id,
            surface_id,
            kind,
            title,
            series_count,
            telemetry_slots,
            accent,
        ));
        id
    }

    fn set_focus(&mut self, id: u16) {
        for surface in self.surfaces.iter_mut() {
            let focused = surface.id == id && surface.visible && surface.focusable;
            if surface.focused != focused {
                surface.focused = focused;
                surface.dirty = true;
            }
        }
        self.scene_epoch = self.scene_epoch.saturating_add(1);
    }
}

pub fn build_gpu_shell_scene(state: &CompositorState, cursor_x: i32, cursor_y: i32) -> RenderScene {
    let mut scene = RenderScene::new(state.width as u32, state.height as u32);
    let theme = state.theme;
    scene.add(
        NodeKind::Background,
        Transform3D::at_z(0, 0, 0),
        Material::Gradient(GradientMaterial::linear_v(
            0xFF00_0000 | theme.background,
            0xFF00_0000 | theme.surface,
        )),
    );

    for (index, surface) in state.surfaces.iter().enumerate() {
        if !surface.visible {
            continue;
        }
        let z = match surface.kind {
            SurfaceKind::Navigation => 10,
            SurfaceKind::Topbar => 20,
            SurfaceKind::StatusBar => 30,
            SurfaceKind::Workspace => 40,
            SurfaceKind::Chart => 45 + index as i32,
            SurfaceKind::Console => 55,
            SurfaceKind::Inspector => 60,
            SurfaceKind::Empty => 5,
        } + if surface.focused { 8 } else { 0 };
        let transform = Transform3D::at_z(surface.bounds.x as i32, surface.bounds.y as i32, z);
        let mut glass = match state.tone {
            ThemeTone::Light => GlassMaterial::LIGHT_FROST,
            ThemeTone::Dark | ThemeTone::HighContrast => GlassMaterial::DARK_PANEL,
        };
        if matches!(
            surface.kind,
            SurfaceKind::Workspace
                | SurfaceKind::Chart
                | SurfaceKind::Console
                | SurfaceKind::Inspector
        ) {
            glass = if surface.focused {
                GlassMaterial::ACRYLIC
            } else {
                glass
            };
        }
        let material = Material::Glass(glass);
        scene.add(
            NodeKind::Panel {
                w: surface.bounds.w as u32,
                h: surface.bounds.h as u32,
            },
            transform,
            material,
        );
    }

    scene.add(
        NodeKind::Cursor { w: 12, h: 20 },
        Transform3D::at_z(cursor_x, cursor_y, 100),
        match state.tone {
            ThemeTone::Light => Material::Solid(0xFF000000),
            ThemeTone::Dark | ThemeTone::HighContrast => Material::Solid(0xFFFFFFFF),
        },
    );

    scene
}

struct ByteCursor<'a> {
    buf: &'a mut [u8],
    len: usize,
}

impl<'a> ByteCursor<'a> {
    fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, len: 0 }
    }

    fn len(&self) -> usize {
        self.len.min(self.buf.len())
    }

    fn push_byte(&mut self, byte: u8) {
        if self.len < self.buf.len() {
            self.buf[self.len] = byte;
        }
        self.len = self.len.saturating_add(1);
    }

    fn push_bytes(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.push_byte(byte);
        }
    }

    fn push_bool(&mut self, value: bool) {
        self.push_byte(if value { b'1' } else { b'0' });
    }

    fn push_rect(&mut self, rect: Rect) {
        self.push_u16(rect.x);
        self.push_byte(b',');
        self.push_u16(rect.y);
        self.push_byte(b',');
        self.push_u16(rect.w);
        self.push_byte(b'x');
        self.push_u16(rect.h);
    }

    fn push_u8(&mut self, value: u8) {
        self.push_u32(value as u32);
    }

    fn push_u16(&mut self, value: u16) {
        self.push_u32(value as u32);
    }

    fn push_u32(&mut self, mut value: u32) {
        if value == 0 {
            self.push_byte(b'0');
            return;
        }

        let mut digits = [0u8; 10];
        let mut len = 0usize;
        while value > 0 {
            digits[len] = b'0' + (value % 10) as u8;
            value /= 10;
            len += 1;
        }
        while len > 0 {
            len -= 1;
            self.push_byte(digits[len]);
        }
    }

    fn push_hex(&mut self, value: u32) {
        self.push_bytes(b"0x");
        for shift in (0..8).rev() {
            let nibble = ((value >> (shift * 4)) & 0xF) as u8;
            self.push_byte(match nibble {
                0..=9 => b'0' + nibble,
                _ => b'a' + (nibble - 10),
            });
        }
    }
}
