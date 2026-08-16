//! Virtual rendering helpers for headless client frame streaming.

use ratatui::backend::{Backend, ClearType, TestBackend, WindowSize};
use ratatui::layout::{Position, Rect, Size};

use crate::app::state::AppState;
use crate::app::Mode;
use crate::protocol::render_ansi::{BlitEncoder, EncodedBlit};
use crate::protocol::{CursorState, FrameData, RenderEncoding, ServerMessage, TerminalFrame};
use crate::terminal::TerminalRuntimeRegistry;

/// Per-client render baseline for the negotiated render encoding.
pub(crate) enum ClientRenderState {
    /// Semantic clients compare full frame data and skip identical frames.
    Semantic { last_frame: Option<FrameData> },
    /// Terminal-ANSI clients keep a terminal diff encoder and sequence number.
    TerminalAnsi { blit_encoder: BlitEncoder, seq: u64 },
}

impl ClientRenderState {
    pub(crate) fn new(render_encoding: RenderEncoding) -> Self {
        match render_encoding {
            RenderEncoding::SemanticFrame => Self::Semantic { last_frame: None },
            RenderEncoding::TerminalAnsi => Self::TerminalAnsi {
                blit_encoder: BlitEncoder::new(),
                seq: 0,
            },
        }
    }

    pub(crate) fn reset_baseline(&mut self) {
        match self {
            Self::Semantic { last_frame } => *last_frame = None,
            Self::TerminalAnsi { blit_encoder, .. } => *blit_encoder = BlitEncoder::new(),
        }
    }

    pub(crate) fn reset_semantic_input_baseline(&mut self) {
        if let Self::Semantic { last_frame } = self {
            *last_frame = None;
        }
    }

    pub(crate) fn prepare_frame(&mut self, frame: FrameData) -> Option<PreparedRender> {
        self.prepare_frame_with_patch(frame, None)
    }

    /// Like [`Self::prepare_frame`], but for `Semantic` clients may send a
    /// compact `FramePatch` instead of the full frame when `patch` carries
    /// segments computed against the client's existing baseline (same size,
    /// baseline present). `frame` is always the complete, correctly patched
    /// composite regardless of what gets sent -- it becomes the new
    /// baseline either way, exactly as a full `Frame` send would.
    ///
    /// `TerminalAnsi` clients ignore `patch`: they always derive their own
    /// diff via `BlitEncoder`, which needs the full frame anyway.
    pub(crate) fn prepare_frame_with_patch(
        &mut self,
        frame: FrameData,
        patch: Option<(Vec<crate::protocol::FramePatchSegment>, bool)>,
    ) -> Option<PreparedRender> {
        match self {
            Self::Semantic { last_frame } => {
                if last_frame
                    .as_ref()
                    .is_some_and(|prev| prev.content_eq(&frame))
                {
                    crate::render_prof::event("prepare_frame.semantic.skip_current");
                    return None;
                }
                crate::render_prof::event("prepare_frame.semantic.changed");
                let message = match (patch, last_frame.as_ref()) {
                    (Some((segments, cursor_changed)), Some(prev))
                        if prev.width == frame.width && prev.height == frame.height =>
                    {
                        crate::render_prof::event("prepare_frame.semantic.patch");
                        crate::render_prof::counter(
                            "prepare_frame.semantic.patch_segments",
                            segments.len() as u64,
                        );
                        ServerMessage::FramePatch {
                            segments,
                            cursor: frame.cursor.clone(),
                            cursor_changed,
                        }
                    }
                    _ => {
                        crate::render_prof::event("prepare_frame.semantic.full");
                        ServerMessage::Frame(frame.clone())
                    }
                };
                Some(PreparedRender::Semantic { message, frame })
            }
            Self::TerminalAnsi { blit_encoder, seq } => {
                if blit_encoder.is_current(&frame) {
                    crate::render_prof::event("prepare_frame.ansi.skip_current");
                    return None;
                }
                let mut encoded = blit_encoder.encode(&frame, frame.is_full);
                crate::render_prof::event("prepare_frame.ansi.changed");
                crate::render_prof::counter("prepare_frame.ansi.bytes", encoded.bytes.len() as u64);
                if encoded.full {
                    crate::render_prof::event("prepare_frame.ansi.full");
                } else {
                    crate::render_prof::event("prepare_frame.ansi.partial");
                }
                insert_graphics_before_sync_end(&mut encoded.bytes, &frame.graphics);
                crate::render_prof::counter(
                    "prepare_frame.graphics.bytes",
                    frame.graphics.len() as u64,
                );
                Some(PreparedRender::TerminalAnsi {
                    message: ServerMessage::Terminal(TerminalFrame {
                        seq: *seq + 1,
                        width: frame.width,
                        height: frame.height,
                        full: encoded.full,
                        bytes: encoded.bytes.clone(),
                    }),
                    frame,
                    encoded: Some(encoded),
                })
            }
        }
    }

    pub(crate) fn last_frame(&self) -> Option<&FrameData> {
        match self {
            Self::Semantic { last_frame } => last_frame.as_ref(),
            Self::TerminalAnsi { blit_encoder, .. } => blit_encoder.last_frame(),
        }
    }

    pub(crate) fn commit_sent_frame(&mut self, prepared: PreparedRender) {
        match (self, prepared) {
            (Self::Semantic { last_frame }, PreparedRender::Semantic { frame, .. }) => {
                *last_frame = Some(frame)
            }
            (
                Self::TerminalAnsi { blit_encoder, seq },
                PreparedRender::TerminalAnsi {
                    frame,
                    encoded: Some(encoded),
                    ..
                },
            ) => {
                blit_encoder.commit(frame, encoded);
                *seq += 1;
            }
            _ => {}
        }
    }

    #[cfg(test)]
    pub(crate) fn terminal_seq(&self) -> Option<u64> {
        match self {
            Self::Semantic { .. } => None,
            Self::TerminalAnsi { seq, .. } => Some(*seq),
        }
    }
}

const SYNC_OUTPUT_END: &[u8] = b"\x1b[?2026l";

fn insert_graphics_before_sync_end(encoded: &mut Vec<u8>, graphics: &[u8]) {
    if graphics.is_empty() {
        return;
    }

    if let Some(sync_end) = rfind_subslice(encoded, SYNC_OUTPUT_END) {
        encoded.splice(sync_end..sync_end, graphics.iter().copied());
    } else {
        encoded.extend_from_slice(graphics);
    }
}

fn rfind_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }

    haystack
        .windows(needle.len())
        .rposition(|window| window == needle)
}

/// A prepared client render message plus any baseline state needed after send.
pub(crate) enum PreparedRender {
    Semantic {
        message: ServerMessage,
        /// The complete, correctly patched composite frame -- this is the
        /// new baseline regardless of whether `message` is a full `Frame`
        /// or a compact `FramePatch`.
        frame: FrameData,
    },
    TerminalAnsi {
        message: ServerMessage,
        frame: FrameData,
        encoded: Option<EncodedBlit>,
    },
}

impl PreparedRender {
    pub(crate) fn message(&self) -> &ServerMessage {
        match self {
            Self::Semantic { message, .. } | Self::TerminalAnsi { message, .. } => message,
        }
    }

    pub(crate) fn into_frame(self) -> Option<FrameData> {
        match self {
            Self::Semantic { frame, .. } => Some(frame),
            Self::TerminalAnsi { frame, .. } => Some(frame),
        }
    }
}

struct CursorTrackingBackend {
    inner: TestBackend,
    rendered_cursor: Option<Position>,
}

impl CursorTrackingBackend {
    fn new(width: u16, height: u16) -> Self {
        Self {
            inner: TestBackend::new(width, height),
            rendered_cursor: None,
        }
    }

    fn buffer(&self) -> &ratatui::buffer::Buffer {
        self.inner.buffer()
    }

    fn rendered_cursor(&self) -> Option<CursorState> {
        self.rendered_cursor.map(|pos| CursorState {
            x: pos.x,
            y: pos.y,
            visible: true,
            shape: 0,
        })
    }
}

impl Backend for CursorTrackingBackend {
    type Error = std::convert::Infallible;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)>,
    {
        self.inner.draw(content)
    }

    fn append_lines(&mut self, n: u16) -> Result<(), Self::Error> {
        self.inner.append_lines(n)
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.hide_cursor()?;
        self.rendered_cursor = None;
        Ok(())
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        self.inner.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        let position = position.into();
        self.inner.set_cursor_position(position)?;
        self.rendered_cursor = Some(position);
        Ok(())
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> Result<Size, Self::Error> {
        self.inner.size()
    }

    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.flush()
    }
}

/// Renders the AppState to an in-memory ratatui Buffer.
///
/// This produces the same output as the monolithic binary's terminal draw,
/// but writes to a `Buffer` instead of stdout. Cursor visibility is captured
/// from explicit frame cursor intent rather than incidental backend state.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn render_virtual(
    app_state: &mut AppState,
    area: Rect,
    resize_panes: bool,
) -> (ratatui::buffer::Buffer, Option<CursorState>) {
    let terminal_runtimes = TerminalRuntimeRegistry::new();
    render_virtual_with_runtime_registry(
        app_state,
        &terminal_runtimes,
        area,
        resize_panes,
        crate::kitty_graphics::HostCellSize::default(),
    )
}

pub(crate) fn render_virtual_with_runtime_registry(
    app_state: &mut AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
    resize_panes: bool,
    cell_size: crate::kitty_graphics::HostCellSize,
) -> (ratatui::buffer::Buffer, Option<CursorState>) {
    let popup_visible = app_state.popup_pane.is_some();
    let pre_compute_suppresses_focused_terminal_cursor =
        !popup_visible && focused_terminal_suppresses_host_cursor(app_state, terminal_runtimes);
    let layout_started = crate::render_prof::timer();
    if resize_panes {
        crate::ui::compute_view_with_cell_size(app_state, terminal_runtimes, area, cell_size);
    } else {
        crate::ui::compute_view_without_resizing_panes(app_state, terminal_runtimes, area);
    }
    crate::render_prof::duration_since("full_render.render_virtual.layout", layout_started);
    let suppress_focused_terminal_cursor = pre_compute_suppresses_focused_terminal_cursor
        || (!popup_visible
            && focused_terminal_suppresses_host_cursor(app_state, terminal_runtimes));

    let backend = CursorTrackingBackend::new(area.width, area.height);
    let mut terminal = ratatui::Terminal::new(backend).expect("TestBackend::new should never fail");

    let draw_started = crate::render_prof::timer();
    terminal
        .draw(|frame| {
            crate::ui::render_with_runtime_registry(app_state, terminal_runtimes, frame);
        })
        .expect("render to TestBackend should never fail");
    crate::render_prof::duration_since("full_render.render_virtual.draw", draw_started);

    let clone_started = crate::render_prof::timer();
    let buffer = terminal.backend().buffer().clone();
    crate::render_prof::duration_since("full_render.render_virtual.clone", clone_started);
    let cursor = if popup_visible {
        popup_terminal_cursor(app_state, terminal_runtimes)
    } else if suppress_focused_terminal_cursor {
        None
    } else {
        focused_terminal_cursor(app_state, terminal_runtimes).or_else(|| {
            (!focused_terminal_owns_host_cursor(app_state, terminal_runtimes))
                .then(|| terminal.backend().rendered_cursor())
                .flatten()
        })
    };

    (buffer, cursor)
}

fn popup_terminal_cursor(
    app_state: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> Option<CursorState> {
    let popup = app_state.popup_pane.as_ref()?;
    let runtime = terminal_runtimes.get(&popup.terminal_id)?;
    if runtime.synchronized_output_active() {
        return None;
    }
    let (_, inner) = crate::ui::popup_pane_rects(app_state, app_state.view.terminal_area)?;
    let cursor = runtime.cursor_state(inner, true)?;
    Some(CursorState {
        x: cursor.x,
        y: cursor.y,
        visible: cursor.visible && !crate::ui::pane_is_scrolled_back(runtime),
        shape: cursor.shape,
    })
}

/// Renders one server-owned terminal directly for `terminal attach` clients.
pub(crate) fn render_terminal_virtual(
    runtime: &crate::terminal::TerminalRuntime,
    area: Rect,
) -> (ratatui::buffer::Buffer, Option<CursorState>) {
    let suppress_cursor = runtime.synchronized_output_active();
    let backend = CursorTrackingBackend::new(area.width, area.height);
    let mut terminal = ratatui::Terminal::new(backend).expect("TestBackend::new should never fail");

    terminal
        .draw(|frame| {
            runtime.render(frame, area, true);
        })
        .expect("render to TestBackend should never fail");

    let buffer = terminal.backend().buffer().clone();
    let cursor = (!suppress_cursor)
        .then(|| runtime.cursor_state(area, true))
        .flatten()
        .map(|cursor| CursorState {
            x: cursor.x,
            y: cursor.y,
            visible: cursor.visible && !crate::ui::pane_is_scrolled_back(runtime),
            shape: cursor.shape,
        })
        .or_else(|| {
            (!suppress_cursor)
                .then(|| terminal.backend().rendered_cursor())
                .flatten()
        });

    (buffer, cursor)
}

pub(crate) fn visible_hyperlinks(
    app_state: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> Vec<((u16, u16), String, String)> {
    crate::ui::tab_surface_hyperlinks(app_state, terminal_runtimes, app_state.view.tab_surface())
}

pub(crate) fn focused_terminal_cursor(
    app_state: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> Option<CursorState> {
    crate::ui::tab_surface_cursor(app_state, terminal_runtimes, app_state.view.tab_surface())
}

fn focused_terminal_owns_host_cursor(
    app_state: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> bool {
    if app_state.mode != Mode::Terminal {
        return false;
    }

    let Some(ws_idx) = app_state.active else {
        return false;
    };
    let Some(info) = app_state
        .view
        .pane_infos
        .iter()
        .find(|info| info.is_focused)
    else {
        return false;
    };
    if !app_state.pane_exposes_host_cursor(ws_idx, info.id) {
        return false;
    }

    app_state
        .runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id)
        .is_some()
}

fn focused_terminal_suppresses_host_cursor(
    app_state: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> bool {
    if app_state.mode != Mode::Terminal {
        return false;
    }

    let Some(ws_idx) = app_state.active else {
        return false;
    };
    let Some(info) = app_state
        .view
        .pane_infos
        .iter()
        .find(|info| info.is_focused)
    else {
        return false;
    };
    if !app_state.pane_exposes_host_cursor(ws_idx, info.id) {
        return false;
    }

    app_state
        .runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id)
        .is_some_and(crate::terminal::TerminalRuntime::synchronized_output_active)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{CellData, FramePatchSegment};
    use std::time::Instant;

    fn test_cell(symbol: &str) -> CellData {
        CellData {
            symbol: symbol.into(),
            fg: 0,
            bg: 0,
            modifier: 0,
            skip: false,
            hyperlink: None,
        }
    }

    fn filled_frame(width: u16, height: u16, symbol: &str) -> FrameData {
        FrameData {
            is_full: false,
            cells: vec![test_cell(symbol); usize::from(width) * usize::from(height)],
            width,
            height,
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        }
    }

    fn one_row_patch(
        frame: &FrameData,
        row: u16,
        symbol: &str,
    ) -> (FrameData, Vec<FramePatchSegment>) {
        let width = usize::from(frame.width);
        let mut target = frame.clone();
        for x in 0..width {
            target.cells[usize::from(row) * width + x] = test_cell(symbol);
        }
        let start = usize::from(row) * width;
        let segment = FramePatchSegment {
            y: row,
            x: 0,
            cells: target.cells[start..start + width].to_vec(),
        };
        (target, vec![segment])
    }

    #[test]
    fn prepare_frame_with_patch_sends_patch_when_baseline_matches() {
        let mut state = ClientRenderState::new(RenderEncoding::SemanticFrame);
        let baseline = filled_frame(10, 4, ".");
        // Prime the baseline via a plain full-frame send, as the first
        // frame to any client always is.
        let prepared = state
            .prepare_frame(baseline.clone())
            .expect("first frame always changed");
        state.commit_sent_frame(prepared);

        let (target, segments) = one_row_patch(&baseline, 1, "x");
        let prepared = state
            .prepare_frame_with_patch(target.clone(), Some((segments.clone(), false)))
            .expect("changed frame should produce a prepared render");
        assert!(
            matches!(prepared.message(), ServerMessage::FramePatch { .. }),
            "expected a compact FramePatch once a matching baseline exists"
        );

        // Baseline must converge to the full target frame either way --
        // this is the invariant the whole optimization depends on.
        state.commit_sent_frame(prepared);
        assert_eq!(state.last_frame(), Some(&target));
    }

    #[test]
    fn prepare_frame_with_patch_falls_back_to_full_frame_without_baseline() {
        let mut state = ClientRenderState::new(RenderEncoding::SemanticFrame);
        let baseline = filled_frame(10, 4, ".");
        let (target, segments) = one_row_patch(&baseline, 1, "x");

        // No prior frame committed -- must never claim to patch nothing.
        let prepared = state
            .prepare_frame_with_patch(target.clone(), Some((segments, false)))
            .expect("first frame always changed");
        assert!(
            matches!(prepared.message(), ServerMessage::Frame(_)),
            "must send a full frame when there is no baseline to patch"
        );
        state.commit_sent_frame(prepared);
        assert_eq!(state.last_frame(), Some(&target));
    }

    #[test]
    fn prepare_frame_with_patch_falls_back_to_full_frame_on_size_mismatch() {
        let mut state = ClientRenderState::new(RenderEncoding::SemanticFrame);
        let baseline = filled_frame(10, 4, ".");
        let prepared = state.prepare_frame(baseline.clone()).unwrap();
        state.commit_sent_frame(prepared);

        // A resized frame with segments computed against the old size must
        // never be sent as a patch against the new baseline.
        let resized = filled_frame(12, 4, "y");
        let bogus_segments = vec![FramePatchSegment {
            y: 0,
            x: 0,
            cells: vec![test_cell("y"); 12],
        }];
        let prepared = state
            .prepare_frame_with_patch(resized.clone(), Some((bogus_segments, false)))
            .expect("changed frame should produce a prepared render");
        assert!(
            matches!(prepared.message(), ServerMessage::Frame(_)),
            "must not send a patch when frame size differs from the baseline"
        );
        state.commit_sent_frame(prepared);
        assert_eq!(state.last_frame(), Some(&resized));
    }

    #[test]
    fn frame_patch_encode_is_much_cheaper_than_full_frame_encode() {
        // Wall-clock companion to the byte-size regression test in
        // protocol::wire -- directly validates the reported symptom
        // (per-keystroke serialize cost in the retained render path), not
        // just payload size. Uses a large relative margin and averages
        // several iterations to stay stable on slow/shared machines.
        let baseline = filled_frame(200, 60, ".");
        let (target, segments) = one_row_patch(&baseline, 30, "x");

        const ITERS: u32 = 200;

        let full_started = Instant::now();
        for _ in 0..ITERS {
            let msg = ServerMessage::Frame(target.clone());
            let _ = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        }
        let full_elapsed = full_started.elapsed();

        let patch_started = Instant::now();
        for _ in 0..ITERS {
            let msg = ServerMessage::FramePatch {
                segments: segments.clone(),
                cursor: None,
                cursor_changed: false,
            };
            let _ = bincode::serde::encode_to_vec(&msg, bincode::config::standard()).unwrap();
        }
        let patch_elapsed = patch_started.elapsed();

        assert!(
            patch_elapsed * 5 < full_elapsed,
            "expected patch encoding to be at least 5x faster than full-frame encoding \
             (full: {full_elapsed:?}, patch: {patch_elapsed:?}) -- \
             regression in the FramePatch fast path"
        );
    }
}
