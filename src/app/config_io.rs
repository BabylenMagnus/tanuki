use super::App;

impl App {
    pub(super) fn update_config_file<F>(&mut self, error_context: &str, update: F) -> bool
    where
        F: FnOnce(&str) -> String,
    {
        #[cfg(test)]
        if std::env::var_os(crate::config::CONFIG_PATH_ENV_VAR).is_none() {
            return false;
        }

        let path = crate::config::config_path();
        if let Some(parent) = path.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                crate::logging::config_write_failed(&path, error_context, &err.to_string());
                self.state.config_diagnostic =
                    Some(format!("failed to save {error_context}: {err}"));
                self.config_diagnostic_deadline =
                    Some(std::time::Instant::now() + std::time::Duration::from_secs(5));
                return false;
            }
        }

        let content = std::fs::read_to_string(&path).unwrap_or_default();
        let new_content = update(&content);
        if let Err(err) = std::fs::write(&path, new_content) {
            crate::logging::config_write_failed(&path, error_context, &err.to_string());
            self.state.config_diagnostic = Some(format!("failed to save {error_context}: {err}"));
            self.config_diagnostic_deadline =
                Some(std::time::Instant::now() + std::time::Duration::from_secs(5));
            return false;
        }

        // Record our own write's mtime so the external-change poll doesn't
        // mistake this save for a hand-edit and reload a second time with
        // a redundant "config reloaded" toast.
        self.known_config_mtime = crate::config::config_mtime();

        true
    }

    /// Check whether `config.toml` changed on disk since we last knew about it
    /// (either from loading it or from our own last write). Updates the known
    /// mtime as a side effect so repeated polls don't refire on the same change.
    pub(super) fn check_external_config_change(&mut self) -> bool {
        let current = crate::config::config_mtime();
        if current == self.known_config_mtime {
            return false;
        }
        let previously_known = self.known_config_mtime.is_some();
        self.known_config_mtime = current;
        // Only reload if we previously had a known mtime — the very first
        // poll after startup would otherwise always look like a "change"
        // when the file didn't exist yet and then got created.
        previously_known && current.is_some()
    }

    pub(super) fn mark_onboarding_complete(&mut self) {
        self.update_config_file("onboarding setting", |content| {
            crate::config::upsert_top_level_bool(content, "onboarding", false)
        });
    }

    pub(super) fn save_theme(&mut self, name: &str) {
        if self.update_config_file("theme", |content| {
            let content = crate::config::upsert_section_value(
                content,
                "theme",
                "name",
                &format!("\"{name}\""),
            );
            crate::config::upsert_section_bool(&content, "theme", "auto_switch", false)
        }) {
            self.apply_config_from_disk(false);
        }
    }

    pub(super) fn save_sound(&mut self, enabled: bool) {
        if self.update_config_file("sound setting", |content| {
            crate::config::upsert_section_bool(content, "ui.sound", "enabled", enabled)
        }) {
            self.apply_config_from_disk(false);
        }
    }

    pub(super) fn save_toast_delivery(&mut self, delivery: crate::config::ToastDelivery) {
        let value = match delivery {
            crate::config::ToastDelivery::Off => "\"off\"",
            crate::config::ToastDelivery::Tanuki => "\"tanuki\"",
            crate::config::ToastDelivery::Terminal => "\"terminal\"",
            crate::config::ToastDelivery::System => "\"system\"",
        };
        if self.update_config_file("toast setting", |content| {
            let content =
                crate::config::upsert_section_value(content, "ui.toast", "delivery", value);
            crate::config::remove_section_key(&content, "ui.toast", "enabled")
        }) {
            self.apply_config_from_disk(false);
        }
    }

    pub(super) fn save_language(&mut self, language: &str) {
        if self.update_config_file("language setting", |content| {
            crate::config::upsert_section_value(
                content,
                "ui",
                "language",
                &format!("\"{language}\""),
            )
        }) {
            self.apply_config_from_disk(false);
        }
    }

    pub(super) fn save_agent_border_labels(&mut self, enabled: bool) {
        if self.update_config_file("agent border labels", |content| {
            crate::config::upsert_section_bool(
                content,
                "ui",
                "show_agent_labels_on_pane_borders",
                enabled,
            )
        }) {
            self.apply_config_from_disk(false);
        }
    }

    pub(super) fn save_pane_history_persistence(&mut self, enabled: bool) {
        if self.update_config_file("pane screen history", |content| {
            crate::config::upsert_section_bool(content, "experimental", "pane_history", enabled)
        }) {
            self.apply_config_from_disk(false);
        }
    }

    /// Persists whether the server should register as a Tanuki Cloud host
    /// (`tanuki server --cloud-host`) on every normal `tanuki` launch. Takes
    /// effect the next time the server daemon is (re)spawned — see
    /// `server::autodetect::build_server_daemon_command`; it does not
    /// reconfigure the currently-running server in place.
    pub(super) fn save_cloud_host(&mut self, enabled: bool) {
        if self.update_config_file("cloud host", |content| {
            crate::config::upsert_section_bool(content, "remote", "cloud_host", enabled)
        }) {
            self.apply_config_from_disk(false);
        }
    }

    pub(super) fn save_switch_ascii_input_source_in_prefix(&mut self, enabled: bool) {
        if self.update_config_file("prefix ascii input source", |content| {
            crate::config::upsert_section_bool(
                content,
                "experimental",
                "switch_ascii_input_source_in_prefix",
                enabled,
            )
        }) {
            self.apply_config_from_disk(false);
        }
    }

    /// Write a single keybind action's binding into the `[keys]` table.
    /// `action_id` must match a `KeysConfig` field name (e.g. `next_agent`).
    /// `binding` is the raw binding string (e.g. `"prefix+tab"`, `"ctrl+alt+right"`).
    pub(super) fn save_keybind(&mut self, action_id: &str, binding: &str) {
        if self.update_config_file("keybind", |content| {
            crate::config::upsert_section_value(
                content,
                "keys",
                action_id,
                &format!("\"{binding}\""),
            )
        }) {
            self.apply_config_from_disk(false);
        }
    }

    pub(super) fn save_agent_panel_sort(&mut self, sort: crate::app::state::AgentPanelSort) {
        let value = match sort {
            crate::app::state::AgentPanelSort::Spaces => {
                crate::config::AgentPanelSortConfig::Spaces.as_str()
            }
            crate::app::state::AgentPanelSort::Priority => {
                crate::config::AgentPanelSortConfig::Priority.as_str()
            }
        };
        if self.update_config_file("agent panel sort", |content| {
            crate::config::upsert_section_value(
                content,
                "ui",
                "agent_panel_sort",
                &format!("\"{value}\""),
            )
        }) {
            self.apply_config_from_disk(false);
        }
    }
}
