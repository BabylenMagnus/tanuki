use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph, Tabs},
    Frame,
};

use rust_i18n::t;

use super::widgets::{
    action_button_row_rects, centered_popup_rect, modal_stack_areas, panel_contrast_fg,
    render_action_button, render_modal_choice_list, render_panel_shell, ActionButtonSpec,
};
use crate::{
    app::{
        state::{ExperimentSetting, KeybindSetting, Palette},
        AppState,
    },
    config::ToastDelivery,
};

pub(crate) const SETTINGS_POPUP_WIDTH: u16 = 76;
pub(crate) const SETTINGS_POPUP_BASE_HEIGHT: u16 = 22;

pub(crate) fn settings_popup_height(app: &AppState) -> u16 {
    if app.settings.section != crate::app::state::SettingsSection::Integrations {
        return SETTINGS_POPUP_BASE_HEIGHT;
    }
    let list_rows = app.integration_recommendations.len().max(1) as u16;
    let footer_rows = integrations_footer_height(app, SETTINGS_POPUP_WIDTH - 2);
    // borders 2 + header 3 + stack gaps 2 + modal footer 2
    // + section title 1 + description 2 + spacers 2
    (14 + list_rows + footer_rows).max(SETTINGS_POPUP_BASE_HEIGHT)
}

pub(super) fn render_settings_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    use crate::app::state::SettingsSection;

    let p = &app.palette;
    let Some(popup) = centered_popup_rect(area, SETTINGS_POPUP_WIDTH, settings_popup_height(app))
    else {
        return;
    };

    super::dim_background(frame, area);

    let Some(inner) = render_panel_shell(frame, popup, p.accent, p.panel_bg) else {
        return;
    };
    if inner.height < 4 || inner.width < 10 {
        return;
    }

    let stack = modal_stack_areas(inner, 3, 2, 0, 1);
    let header_rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas::<3>(stack.header);

    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            format!(" {}", t!("settings.title")),
            Style::default().fg(p.text).add_modifier(Modifier::BOLD),
        )])),
        header_rows[0],
    );

    let tab_labels = SettingsSection::ALL.iter().map(|section| {
        if app.settings_section_has_badge(*section) {
            Line::from(vec![
                Span::styled(
                    "● ",
                    Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
                ),
                Span::raw(section.label()),
            ])
        } else {
            Line::from(section.label())
        }
    });
    let tabs = Tabs::new(tab_labels)
        .select(
            SettingsSection::ALL
                .iter()
                .position(|section| *section == app.settings.section)
                .unwrap_or(0),
        )
        .style(Style::default().fg(p.overlay1))
        .highlight_style(
            Style::default()
                .fg(panel_contrast_fg(p))
                .bg(p.accent)
                .add_modifier(Modifier::BOLD),
        )
        .divider(" ")
        .padding(" ", " ");
    frame.render_widget(tabs, header_rows[1]);

    let sep = "─".repeat(inner.width as usize);
    frame.render_widget(
        Paragraph::new(Span::styled(&sep, Style::default().fg(p.surface0))),
        header_rows[2],
    );

    let content_area = stack.content;

    match app.settings.section {
        SettingsSection::Theme => {
            render_settings_theme(app, frame, content_area);
        }
        SettingsSection::Sound => {
            render_settings_toggle(
                frame,
                content_area,
                p,
                &t!("settings.sound.title"),
                &t!("settings.sound.description"),
                app.sound_enabled(),
                app.settings.list.selected,
            );
        }
        SettingsSection::Toast => {
            let off = t!("settings.toast.off").to_string();
            let inside_tanuki = t!("settings.toast.inside_tanuki").to_string();
            let via_terminal = t!("settings.toast.via_terminal").to_string();
            let via_system = t!("settings.toast.via_system").to_string();
            render_modal_choice_list(
                frame,
                content_area,
                &t!("settings.toast.title"),
                &t!("settings.toast.description"),
                &[
                    (off.as_str(), ToastDelivery::Off),
                    (inside_tanuki.as_str(), ToastDelivery::Tanuki),
                    (via_terminal.as_str(), ToastDelivery::Terminal),
                    (via_system.as_str(), ToastDelivery::System),
                ],
                app.toast_delivery(),
                app.settings.list.selected,
                p,
                2,
            );
        }
        SettingsSection::PaneLabels => {
            render_settings_toggle(
                frame,
                content_area,
                p,
                &t!("settings.pane_labels.title"),
                &t!("settings.pane_labels.description"),
                app.agent_border_labels_enabled(),
                app.settings.list.selected,
            );
        }
        SettingsSection::Language => {
            let auto = t!("settings.language.auto").to_string();
            let english = t!("settings.language.english").to_string();
            let russian = t!("settings.language.russian").to_string();
            render_modal_choice_list(
                frame,
                content_area,
                &t!("settings.language.title"),
                &t!("settings.language.description"),
                &[
                    (auto.as_str(), "auto"),
                    (english.as_str(), "en"),
                    (russian.as_str(), "ru"),
                ],
                app.language.as_str(),
                app.settings.list.selected,
                p,
                2,
            );
        }
        SettingsSection::Keybinds => {
            render_settings_keybinds(app, frame, content_area);
        }
        SettingsSection::Experiments => {
            render_settings_experiments(app, frame, content_area);
        }
        SettingsSection::Integrations => {
            render_settings_integrations(app, frame, content_area);
        }
    }

    if let Some(footer_area) = stack.footer {
        let footer_rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)])
            .areas::<2>(footer_area);
        let primary_label = settings_primary_button_label(app.settings.section);
        let show_primary = settings_show_primary_action(app);
        let (apply_rect, close_rect) =
            settings_button_rects(inner, app.settings.section, show_primary);
        if let Some(apply_rect) = apply_rect {
            render_action_button(
                frame,
                apply_rect,
                Some("↵"),
                &primary_label,
                Style::default()
                    .fg(panel_contrast_fg(p))
                    .bg(p.accent)
                    .add_modifier(Modifier::BOLD),
            );
        }
        render_action_button(
            frame,
            close_rect,
            Some("esc"),
            &t!("settings.close"),
            Style::default()
                .fg(p.text)
                .bg(p.surface0)
                .add_modifier(Modifier::BOLD),
        );

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" ↑↓", Style::default().fg(p.overlay0)),
                Span::styled(format!(" {}  ", t!("settings.footer.select")), Style::default().fg(p.overlay1)),
                Span::styled(t!("settings.footer.tab").to_string(), Style::default().fg(p.overlay0)),
                Span::styled(format!(" {}", t!("settings.footer.section")), Style::default().fg(p.overlay1)),
            ])),
            footer_rows[0],
        );
    }
}

pub(crate) fn settings_primary_button_label(
    section: crate::app::state::SettingsSection,
) -> String {
    match section {
        crate::app::state::SettingsSection::Integrations => t!("settings.install").to_string(),
        _ => t!("settings.apply").to_string(),
    }
}

pub(crate) fn settings_show_primary_action(app: &AppState) -> bool {
    match app.settings.section {
        crate::app::state::SettingsSection::Integrations => app
            .integration_recommendations
            .iter()
            .any(crate::integration::IntegrationRecommendation::needs_install),
        _ => true,
    }
}

pub(crate) fn settings_button_rects(
    inner: Rect,
    section: crate::app::state::SettingsSection,
    show_primary: bool,
) -> (Option<Rect>, Rect) {
    let close_label = t!("settings.close").to_string();
    if !show_primary {
        let rects = action_button_row_rects(
            inner,
            &[ActionButtonSpec {
                hint: Some("esc"),
                label: &close_label,
            }],
            2,
            inner.height.saturating_sub(1),
        );
        return (None, rects[0]);
    }

    let primary_label = settings_primary_button_label(section);
    let rects = action_button_row_rects(
        inner,
        &[
            ActionButtonSpec {
                hint: Some("↵"),
                label: &primary_label,
            },
            ActionButtonSpec {
                hint: Some("esc"),
                label: &close_label,
            },
        ],
        2,
        inner.height.saturating_sub(1),
    );
    (Some(rects[0]), rects[1])
}

fn integrations_footer_paragraph(app: &AppState) -> Paragraph<'static> {
    let p = &app.palette;
    let mut footer_lines = Vec::new();
    if !app.integration_install_messages.is_empty() {
        for message in &app.integration_install_messages {
            footer_lines.push(Line::from(Span::styled(
                format!(" {message}"),
                Style::default().fg(p.overlay1),
            )));
        }
    } else {
        let found_any = app.integration_recommendations.iter().any(|item| {
            item.available || item.state != crate::integration::IntegrationStatusKind::NotInstalled
        });
        let hint = if app
            .integration_recommendations
            .iter()
            .any(crate::integration::IntegrationRecommendation::needs_install)
        {
            t!("settings.integrations.hint_install")
        } else if found_any {
            t!("settings.integrations.hint_all_installed")
        } else {
            t!("settings.integrations.hint_none_found")
        };
        footer_lines.push(Line::from(Span::styled(
            format!(" {hint}"),
            Style::default().fg(p.overlay1),
        )));
    }
    Paragraph::new(footer_lines).wrap(ratatui::widgets::Wrap { trim: false })
}

fn integrations_footer_height(app: &AppState, width: u16) -> u16 {
    (integrations_footer_paragraph(app).line_count(width) as u16).min(6)
}

fn render_settings_integrations(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;

    let footer = integrations_footer_paragraph(app);
    let footer_height = integrations_footer_height(app, area.width);

    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(footer_height),
    ])
    .areas::<6>(area);

    frame.render_widget(
        Paragraph::new(t!("settings.integrations.title").to_string())
            .style(Style::default().fg(p.text).add_modifier(Modifier::BOLD)),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(t!("settings.integrations.description").to_string())
            .style(Style::default().fg(p.overlay1))
            .wrap(ratatui::widgets::Wrap { trim: false }),
        rows[1],
    );

    let mut lines = Vec::new();
    for item in &app.integration_recommendations {
        let marker = match item.state {
            crate::integration::IntegrationStatusKind::Current => "✓",
            crate::integration::IntegrationStatusKind::Outdated => "↻",
            crate::integration::IntegrationStatusKind::NotInstalled if item.available => "+",
            crate::integration::IntegrationStatusKind::NotInstalled => "–",
        };
        let marker_style = match item.state {
            crate::integration::IntegrationStatusKind::Current => Style::default().fg(p.green),
            crate::integration::IntegrationStatusKind::Outdated => Style::default().fg(p.yellow),
            crate::integration::IntegrationStatusKind::NotInstalled if item.available => {
                Style::default().fg(p.accent)
            }
            crate::integration::IntegrationStatusKind::NotInstalled => {
                Style::default().fg(p.overlay0)
            }
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {marker} "), marker_style),
            Span::styled(
                format!("{:<9}", item.label),
                Style::default().fg(p.subtext0),
            ),
            Span::styled(item.status_label(), Style::default().fg(p.overlay1)),
        ]));
    }

    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            format!(" {}", t!("settings.integrations.none_available")),
            Style::default().fg(p.overlay1),
        )));
    }

    frame.render_widget(Paragraph::new(lines), rows[3]);
    frame.render_widget(footer, rows[5]);
}

fn render_settings_theme(app: &AppState, frame: &mut Frame, area: Rect) {
    use crate::app::state::THEME_NAMES;

    let p = &app.palette;
    let items: Vec<ListItem> = THEME_NAMES
        .iter()
        .map(|name| {
            let is_current = name.to_lowercase().replace([' ', '_'], "-")
                == app.theme_name.to_lowercase().replace([' ', '_'], "-");
            let marker = if is_current { " ✓" } else { "" };
            ListItem::new(Line::from(vec![
                Span::styled(*name, Style::default().fg(p.subtext0)),
                Span::styled(marker, Style::default().fg(p.green)),
            ]))
        })
        .collect();

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(p.surface0)
                .fg(p.text)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(" ▸ ")
        .style(Style::default().fg(p.subtext0));

    let mut state = ListState::default().with_selected(Some(app.settings.list.selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_settings_toggle(
    frame: &mut Frame,
    area: Rect,
    p: &Palette,
    title: &str,
    description: &str,
    current_value: bool,
    selected_idx: usize,
) {
    render_modal_choice_list(
        frame,
        area,
        title,
        description,
        &[("on", true), ("off", false)],
        current_value,
        selected_idx,
        p,
        1,
    );
}

fn render_settings_experiments(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;
    let [desc_area, _, list_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas::<3>(area);

    super::widgets::render_modal_description(
        frame,
        desc_area,
        &t!("settings.experiments.description"),
        Style::default().fg(p.overlay1),
    );

    for (idx, setting) in ExperimentSetting::ALL.iter().copied().enumerate() {
        let marker = if setting.enabled(app) { "[✓]" } else { "[ ]" };
        let style = if app.settings.list.selected == idx {
            Style::default()
                .bg(p.surface0)
                .fg(p.text)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.subtext0)
        };
        let row = Rect::new(list_area.x, list_area.y + idx as u16, list_area.width, 1);
        frame.render_widget(
            Paragraph::new(format!(" {} {marker}", setting.label())).style(style),
            row,
        );
    }
}

fn render_settings_keybinds(app: &AppState, frame: &mut Frame, area: Rect) {
    use crate::app::state::KeybindCaptureKind;

    let p = &app.palette;
    let [desc_area, search_area, list_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas::<3>(area);

    let description = match app.settings.keybind_capture {
        Some(KeybindCaptureKind::Direct) => t!("settings.keybinds.desc_direct"),
        Some(KeybindCaptureKind::Prefix) => t!("settings.keybinds.desc_prefix"),
        None if app.settings.keybind_search_active => t!("settings.keybinds.desc_search_active"),
        None => t!("settings.keybinds.desc_default"),
    };
    super::widgets::render_modal_description(
        frame,
        desc_area,
        &description,
        Style::default().fg(p.overlay1),
    );

    let search_line = if app.settings.keybind_search_active {
        Line::from(vec![
            Span::styled(
                " / ",
                Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                app.settings.keybind_search.clone(),
                Style::default().fg(p.text),
            ),
            Span::styled("▏", Style::default().fg(p.accent)),
        ])
    } else if !app.settings.keybind_search.is_empty() {
        Line::from(vec![
            Span::styled(" / ", Style::default().fg(p.overlay1)),
            Span::styled(
                app.settings.keybind_search.clone(),
                Style::default().fg(p.text),
            ),
            Span::styled(
                format!(" ({})", t!("settings.keybinds.press_to_edit")),
                Style::default().fg(p.overlay1),
            ),
        ])
    } else {
        Line::from(Span::styled(
            format!(" {}", t!("settings.keybinds.press_to_search")),
            Style::default().fg(p.overlay1),
        ))
    };
    frame.render_widget(Paragraph::new(search_line), search_area);

    let filtered = KeybindSetting::filtered(&app.settings.keybind_search);

    if filtered.is_empty() && app.keybinds.custom_commands.is_empty() {
        frame.render_widget(
            Paragraph::new(format!(" {}", t!("settings.keybinds.no_matches")))
                .style(Style::default().fg(p.overlay1)),
            list_area,
        );
        return;
    }

    let header_style = Style::default().fg(p.accent).add_modifier(Modifier::BOLD);

    let mut items: Vec<ListItem> = Vec::new();
    let mut render_selected = 0;
    let mut current_group = None;
    for (idx, setting) in filtered.iter().copied().enumerate() {
        let group = setting.group();
        if current_group != Some(group) {
            items.push(ListItem::new(Line::from(Span::styled(
                format!(" {}", group.label()),
                header_style,
            ))));
            current_group = Some(group);
        }
        if idx == app.settings.list.selected {
            render_selected = items.len();
        }
        let binding_label = setting
            .current_binding(&app.keybinds)
            .unwrap_or_else(|| t!("settings.keybinds.unset").to_string());
        let value = if app.settings.list.selected == idx && app.settings.keybind_capture.is_some() {
            t!("settings.keybinds.press_a_key").to_string()
        } else {
            binding_label
        };
        items.push(ListItem::new(Line::from(vec![
            Span::raw(format!("   {:<22}", setting.label())),
            Span::styled(value, Style::default().fg(p.accent)),
        ])));
    }

    // Indexed 1..9 shortcuts and custom commands use a different editing
    // model (a range binding, or their own dedicated capture flow) than the
    // single-key-capture list above, so they're shown as read-only reference
    // rows rather than folded into the selectable/editable set -- matching
    // how the old standalone keybind-help overlay displayed them before it
    // was unified into this tab. Only shown with no active search filter.
    if app.settings.keybind_search.is_empty() {
        items.push(ListItem::new(Line::from(Span::styled(
            format!(" {}", t!("settings.keybinds.workspaces_tabs_readonly")),
            header_style,
        ))));
        items.push(ListItem::new(Line::from(vec![
            Span::raw(format!(
                "   {:<22}",
                t!("settings.keybinds.switch_workspace").to_string()
            )),
            Span::styled(
                indexed_keybind_label(&app.keybinds.switch_workspace),
                Style::default().fg(p.overlay1),
            ),
        ])));
        items.push(ListItem::new(Line::from(vec![
            Span::raw(format!(
                "   {:<22}",
                t!("settings.keybinds.focus_agent").to_string()
            )),
            Span::styled(
                indexed_keybind_label(&app.keybinds.focus_agent),
                Style::default().fg(p.overlay1),
            ),
        ])));
        items.push(ListItem::new(Line::from(vec![
            Span::raw(format!(
                "   {:<22}",
                t!("settings.keybinds.switch_tab").to_string()
            )),
            Span::styled(
                indexed_keybind_label(&app.keybinds.switch_tab),
                Style::default().fg(p.overlay1),
            ),
        ])));

        if !app.keybinds.custom_commands.is_empty() {
            items.push(ListItem::new(Line::from(Span::styled(
                format!(" {}", t!("settings.keybinds.custom_readonly")),
                header_style,
            ))));
            for command in &app.keybinds.custom_commands {
                let description = command
                    .description
                    .clone()
                    .unwrap_or_else(|| command.command.clone());
                items.push(ListItem::new(Line::from(vec![
                    Span::raw(format!("   {:<22}", description)),
                    Span::styled(command.label.clone(), Style::default().fg(p.overlay1)),
                ])));
            }
        }
    }

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(p.surface0)
                .fg(p.text)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(" ▸ ")
        .style(Style::default().fg(p.subtext0));

    let mut list_state = ListState::default();
    if !filtered.is_empty() {
        list_state = list_state.with_selected(Some(render_selected));
    }
    frame.render_stateful_widget(list, list_area, &mut list_state);
}

/// Renders an indexed `1..9` keybind range (e.g. `switch_workspace`) the same
/// way the old keybind-help overlay did: collapse a full `prefix+shift+1..9`
/// run into `prefix+shift+1..9`, or fall back to listing each configured
/// binding's label when the run isn't a clean `1..9` sequence.
fn indexed_keybind_label(bindings: &[crate::config::IndexedKeybind]) -> String {
    if bindings.is_empty() {
        return t!("settings.keybinds.unset").to_string();
    }

    let mut parts = Vec::new();
    let mut index = 0;
    while index < bindings.len() {
        if let Some(prefix) = indexed_range_prefix(&bindings[index..]) {
            parts.push(format!("{prefix}1..9"));
            index += 9;
        } else {
            parts.push(bindings[index].label.clone());
            index += 1;
        }
    }
    parts.join(" / ")
}

fn indexed_range_prefix(bindings: &[crate::config::IndexedKeybind]) -> Option<&str> {
    let run = bindings.get(..9)?;
    let prefix = run[0].label.strip_suffix('1')?;
    for (offset, binding) in run.iter().enumerate() {
        let digit = char::from(b'1' + offset as u8);
        if binding.label.strip_suffix(digit) != Some(prefix) {
            return None;
        }
    }
    Some(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{state::SettingsSection, Mode};
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn experiments_pane_history_uses_settings_checkmark_marker() {
        let mut app = AppState::test_new();
        app.pane_history_persistence = true;
        app.settings.section = SettingsSection::Experiments;
        app.settings.list.selected = 0;
        app.mode = Mode::Settings;

        let mut terminal =
            Terminal::new(TestBackend::new(80, 24)).expect("test terminal should initialize");
        terminal
            .draw(|frame| render_settings_overlay(&app, frame, Rect::new(0, 0, 80, 24)))
            .expect("settings overlay should render");

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("pane screen history [✓]"));
        assert!(!rendered.contains("[x]"));
    }

    #[test]
    fn experiments_pane_history_keeps_empty_checkbox_marker_when_disabled() {
        let mut app = AppState::test_new();
        app.pane_history_persistence = false;
        app.settings.section = SettingsSection::Experiments;
        app.settings.list.selected = 0;
        app.mode = Mode::Settings;

        let mut terminal =
            Terminal::new(TestBackend::new(80, 24)).expect("test terminal should initialize");
        terminal
            .draw(|frame| render_settings_overlay(&app, frame, Rect::new(0, 0, 80, 24)))
            .expect("settings overlay should render");

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("pane screen history [ ]"));
    }

    #[test]
    fn experiments_renders_switch_ascii_input_source_row() {
        let mut app = AppState::test_new();
        app.switch_ascii_input_source_in_prefix = true;
        app.settings.section = SettingsSection::Experiments;
        app.settings.list.selected = 1;
        app.mode = Mode::Settings;

        let mut terminal =
            Terminal::new(TestBackend::new(80, 24)).expect("test terminal should initialize");
        terminal
            .draw(|frame| render_settings_overlay(&app, frame, Rect::new(0, 0, 80, 24)))
            .expect("settings overlay should render");

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("switch to ascii input source in prefix (macOS) [✓]"));
    }
}
