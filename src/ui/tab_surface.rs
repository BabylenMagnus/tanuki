use ratatui::{layout::Rect, Frame};

use super::panes::{compute_pane_infos_for_tab, render_panes, resize_tab_panes};
use crate::app::state::ViewState;
use crate::app::{AppState, Mode};
use crate::layout::{PaneInfo, SplitBorder};
use crate::protocol::CursorState;
use crate::terminal::TerminalRuntimeRegistry;

/// An explicit "what to render" descriptor — a specific workspace/tab pair —
/// rather than implicitly reading the app-wide `app.active`. Ported from
/// herdr upstream commit `6c0bb273` (TSK-47): the building block
/// `client_views.rs` (TSK-48) uses to give every connected client its own
/// independent view instead of one shared one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TabSurfaceTarget {
    pub(crate) workspace_index: usize,
    pub(crate) tab_index: usize,
}

pub(crate) struct TabSurfaceLayout {
    pub(crate) target: Option<TabSurfaceTarget>,
    pub(crate) pane_infos: Vec<PaneInfo>,
    pub(crate) split_borders: Vec<SplitBorder>,
}

#[derive(Clone, Copy)]
pub(crate) struct TabSurfaceView<'a> {
    pub(crate) target: Option<TabSurfaceTarget>,
    pub(crate) pane_infos: &'a [PaneInfo],
    pub(crate) split_borders: &'a [SplitBorder],
}

impl ViewState {
    /// `self.target` is the workspace/tab this scratch view was last
    /// computed for (set alongside `pane_infos`/`split_borders` in
    /// `compute_view_internal`/`compute_mobile_view`) — always the app-wide
    /// active target today, so every existing call site keeps working
    /// unchanged. TSK-48+ is where per-client targets start flowing through
    /// here instead of this single shared scratch field.
    pub(crate) fn tab_surface(&self) -> TabSurfaceView<'_> {
        TabSurfaceView {
            target: self.target,
            pane_infos: &self.pane_infos,
            split_borders: &self.split_borders,
        }
    }
}

/// Computes the tab surface for the app-wide active workspace/tab. Thin
/// wrapper over `compute_tab_surface_for` kept for call sites that haven't
/// been made target-aware yet (TSK-47).
pub(crate) fn compute_tab_surface(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
    resize_panes: bool,
    cell_size: crate::kitty_graphics::HostCellSize,
) -> TabSurfaceLayout {
    let target = app.active.and_then(|workspace_index| {
        let workspace = app.workspaces.get(workspace_index)?;
        Some(TabSurfaceTarget {
            workspace_index,
            tab_index: workspace.active_tab_index(),
        })
    });
    compute_tab_surface_for(
        app,
        terminal_runtimes,
        target,
        area,
        resize_panes,
        cell_size,
    )
}

/// Computes the tab surface for an explicit target rather than implicitly
/// reading `app.active` — ported from herdr upstream commit `6c0bb273`
/// (TSK-47).
pub(crate) fn compute_tab_surface_for(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    target: Option<TabSurfaceTarget>,
    area: Rect,
    resize_panes: bool,
    cell_size: crate::kitty_graphics::HostCellSize,
) -> TabSurfaceLayout {
    let tab = target.and_then(|target| {
        app.workspaces
            .get(target.workspace_index)?
            .tabs
            .get(target.tab_index)
    });
    let split_borders = tab
        .map(|tab| {
            if tab.zoomed {
                Vec::new()
            } else {
                tab.layout.splits(area)
            }
        })
        .unwrap_or_default();
    let pane_infos = target.map_or_else(Vec::new, |target| {
        compute_pane_infos_for_tab(
            app,
            terminal_runtimes,
            target.workspace_index,
            target.tab_index,
            area,
            resize_panes,
            cell_size,
        )
    });

    TabSurfaceLayout {
        target,
        pane_infos,
        split_borders,
    }
}

pub(crate) fn resize_tab_surface(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    workspace_index: usize,
    tab_index: usize,
    area: Rect,
    cell_size: crate::kitty_graphics::HostCellSize,
) {
    let Some(tab) = app
        .workspaces
        .get(workspace_index)
        .and_then(|workspace| workspace.tabs.get(tab_index))
    else {
        return;
    };
    resize_tab_panes(app, terminal_runtimes, tab, area, cell_size);
}

pub(crate) fn render_tab_surface(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    surface: TabSurfaceView<'_>,
    frame: &mut Frame,
) {
    render_panes(
        app,
        terminal_runtimes,
        frame,
        surface.target,
        surface.pane_infos,
        surface.split_borders,
    );
}

pub(crate) fn tab_surface_hyperlinks(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    surface: TabSurfaceView<'_>,
) -> Vec<((u16, u16), String, String)> {
    let Some(ws_idx) = surface.target.map(|target| target.workspace_index) else {
        return Vec::new();
    };
    if app.workspaces.get(ws_idx).is_none() {
        return Vec::new();
    }

    let mut links = Vec::new();
    for info in surface.pane_infos {
        if let Some(runtime) = app.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id)
        {
            links.extend(runtime.visible_hyperlinks(info.inner_rect));
        }
    }
    links
}

pub(crate) fn tab_surface_cursor(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    surface: TabSurfaceView<'_>,
) -> Option<CursorState> {
    // Upstream (`6c0bb273`) drops this app-wide Mode::Terminal gate in favor
    // of per-client mode tracking that doesn't exist in our fork yet — kept
    // here deliberately so this task doesn't change today's single-shared-
    // mode cursor behavior. Revisit once TSK-50/51 give each client its own
    // mode, not just its own view target.
    if app.mode != Mode::Terminal {
        return None;
    }

    let ws_idx = surface.target?.workspace_index;
    let info = surface.pane_infos.iter().find(|info| info.is_focused)?;
    if !app.pane_exposes_host_cursor(ws_idx, info.id) {
        return None;
    }
    let runtime = app.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id)?;
    if runtime.synchronized_output_active() {
        return None;
    }
    let scrolled_back = super::panes::pane_is_scrolled_back(runtime);
    let reveal = app.reveal_hidden_cursor_for_cjk_ime
        && (!app.cjk_ime_agent_filter_configured || {
            let detected = app
                .workspaces
                .get(ws_idx)
                .and_then(|ws| ws.terminal_id(info.id))
                .and_then(|terminal_id| app.terminals.get(terminal_id))
                .and_then(|terminal| terminal.detected_agent);
            detected.is_some_and(|agent| app.cjk_ime_agents.contains(&agent))
        });

    if let Some(cursor) = runtime.cursor_state(info.inner_rect, true) {
        let visible = if reveal {
            !scrolled_back
        } else {
            cursor.visible && !scrolled_back
        };
        Some(CursorState {
            x: cursor.x,
            y: cursor.y,
            visible,
            shape: if reveal && visible {
                app.cjk_ime_cursor_shape
            } else {
                cursor.shape
            },
        })
    } else if reveal && !scrolled_back {
        Some(CursorState {
            x: info.inner_rect.x,
            y: info.inner_rect.y,
            visible: true,
            shape: app.cjk_ime_cursor_shape,
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Direction;
    use ratatui::Terminal;

    #[tokio::test]
    async fn explicit_surface_layout_drives_render_cursor_and_hyperlinks() {
        let uri = "https://example.com/surface";
        let mut workspace = Workspace::test_new("shell-workspace");
        let left = workspace.tabs[0].root_pane;
        let right = workspace.test_split(Direction::Horizontal);
        workspace.insert_test_runtime(
            left,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(
                20,
                8,
                format!("\x1b]8;;{uri}\x1b\\LEFT\x1b]8;;\x1b\\").as_bytes(),
            ),
        );
        workspace.insert_test_runtime(
            right,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(20, 8, b"RIGHT"),
        );

        let mut app = AppState::test_new();
        app.workspaces = vec![workspace];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;

        let full_area = Rect::new(0, 0, 106, 20);
        crate::ui::compute_view(&mut app, full_area);
        let area = app.view.terminal_area;
        assert_eq!(area, Rect::new(26, 1, 80, 19));
        let surface = compute_tab_surface(
            &app,
            &TerminalRuntimeRegistry::new(),
            area,
            false,
            crate::kitty_graphics::HostCellSize::default(),
        );
        assert_eq!(surface.pane_infos.len(), 2);
        assert!(!surface.split_borders.is_empty());

        app.view.terminal_area = Rect::new(9, 8, 7, 6);
        app.view.pane_infos.clear();
        app.view.split_borders.clear();

        let surface_view = TabSurfaceView {
            target: surface.target,
            pane_infos: &surface.pane_infos,
            split_borders: &surface.split_borders,
        };
        let mut terminal =
            Terminal::new(TestBackend::new(full_area.width, full_area.height)).unwrap();
        terminal
            .draw(|frame| {
                render_tab_surface(&app, &TerminalRuntimeRegistry::new(), surface_view, frame)
            })
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("LEFT"), "surface: {rendered:?}");
        assert!(rendered.contains("RIGHT"), "surface: {rendered:?}");
        assert!(!rendered.contains("shell-workspace"));

        let links = tab_surface_hyperlinks(&app, &TerminalRuntimeRegistry::new(), surface_view);
        assert!(links
            .iter()
            .any(|(_, symbol, link)| { symbol == "L" && link == uri }));
        assert!(tab_surface_cursor(&app, &TerminalRuntimeRegistry::new(), surface_view,).is_some());
    }

    fn full_app_frame(app: &mut AppState, area: Rect) -> crate::protocol::FrameData {
        let (buffer, cursor) = crate::server::render_stream::render_virtual(app, area, true);
        let hyperlinks =
            crate::server::render_stream::visible_hyperlinks(app, &TerminalRuntimeRegistry::new());
        crate::protocol::FrameData::from_ratatui_buffer_with_hyperlinks(
            &buffer,
            cursor,
            &hyperlinks,
        )
    }

    fn frame_digest(frame: &crate::protocol::FrameData) -> String {
        use sha2::{Digest, Sha256};

        let encoded = bincode::serde::encode_to_vec(frame, bincode::config::standard()).unwrap();
        format!("{:x}", Sha256::digest(encoded))
    }

    fn full_app_characterization_state(uri: &str) -> AppState {
        let mut workspace = Workspace::test_new("characterization");
        workspace.identity_cwd = std::path::PathBuf::from("characterization");
        workspace.cached_git_branch = None;
        workspace.cached_git_ahead_behind = None;
        workspace.cached_git_space = None;
        workspace.test_add_tab(Some("logs"));
        workspace.switch_tab(0);
        let left = workspace.tabs[0].root_pane;
        let right = workspace.test_split(Direction::Horizontal);
        workspace.insert_test_runtime(
            left,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(
                40,
                10,
                format!("\x1b]8;;{uri}\x1b\\LINK\x1b]8;;\x1b\\").as_bytes(),
            ),
        );
        workspace.insert_test_runtime(
            right,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(40, 10, b"RIGHT\r\nPANE"),
        );

        let mut app = AppState::test_new();
        app.workspaces = vec![workspace];
        app.active = Some(0);
        app.selected = 0;
        app.mode = Mode::Terminal;
        app
    }

    #[tokio::test]
    async fn desktop_full_app_semantic_frame_is_characterized() {
        let uri = "https://example.com/full-app";
        let mut app = full_app_characterization_state(uri);
        let frame = full_app_frame(&mut app, Rect::new(0, 0, 106, 20));

        assert_eq!((frame.width, frame.height), (106, 20));
        assert_eq!(app.view.sidebar_rect, Rect::new(0, 0, 26, 20));
        assert_eq!(app.view.tab_bar_rect, Rect::new(26, 0, 80, 1));
        assert_eq!(app.view.terminal_area, Rect::new(26, 1, 80, 19));
        assert_eq!(app.view.pane_infos.len(), 2);
        assert!(!app.view.split_borders.is_empty());
        assert!(frame.cursor.is_some());
        assert_eq!(frame.hyperlinks, vec![uri.to_owned()]);
        assert_eq!(
            frame_digest(&frame),
            "04441e1125d31165d50e4102afacba13dde953ec4b4c42d1e56fcdc6d2d2bb23"
        );
    }

    #[tokio::test]
    async fn mobile_full_app_semantic_frame_is_characterized() {
        let mut app = full_app_characterization_state("https://example.com/mobile");
        app.mode = Mode::Navigate;
        let frame = full_app_frame(&mut app, Rect::new(0, 0, 44, 20));

        assert_eq!((frame.width, frame.height), (44, 20));
        assert_eq!(app.view.layout, crate::app::state::ViewLayout::Mobile);
        assert_eq!(app.view.mobile_header_rect, Rect::new(0, 0, 44, 2));
        assert_eq!(app.view.terminal_area, Rect::new(0, 2, 44, 18));
        assert_eq!(frame.cursor, None);
        assert_eq!(
            frame_digest(&frame),
            "dfdceff4ee6ecc22ebb9741aad93f93869dd594049341a63a6fb4a1942e061d8"
        );
    }
}
