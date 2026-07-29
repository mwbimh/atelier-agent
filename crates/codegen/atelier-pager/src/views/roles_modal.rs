//! Dedicated fixed Runtime Role manager.
//!
//! The modal consumes the redacted Role ACP response. It never stores or
//! renders request-payload values; only configured payload keys are shown.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph, Widget, Wrap};

use crate::theme::Theme;
use crate::views::modal_window::{
    self, ModalSizing, ModalWindowConfig, ModalWindowState, Shortcut,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleModalEntry {
    pub role_id: String,
    pub configured: bool,
    pub effective_model: Option<String>,
    pub effective_source: Option<String>,
    pub exact_fields: Vec<String>,
    pub payload_keys: Vec<String>,
    pub effort: Option<String>,
    pub fast_mode: bool,
    pub field_sources: Vec<(String, String)>,
    pub context_source: Option<String>,
}

impl RoleModalEntry {
    fn from_value(value: &serde_json::Value) -> Option<Self> {
        let role_id = value
            .get("roleId")
            .or_else(|| value.get("role_id"))?
            .as_str()?
            .to_owned();
        let exact = value.get("config").filter(|value| !value.is_null());
        let effective = value
            .get("effectiveConfig")
            .or_else(|| value.get("effective_config"))
            .filter(|value| !value.is_null());
        let effective_model = effective.or(exact).and_then(|config| {
            Some(format!(
                "{}/{}",
                config.get("provider")?.as_str()?,
                config.get("model")?.as_str()?
            ))
        });
        let exact_fields = exact
            .and_then(serde_json::Value::as_object)
            .map(|config| {
                ["provider", "model", "effort", "fast_mode", "payload"]
                    .into_iter()
                    .filter(|field| config.contains_key(*field))
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let mut payload_keys = exact
            .and_then(|config| config.get("payload"))
            .and_then(serde_json::Value::as_object)
            .map(|payload| payload.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        payload_keys.sort();
        let effort = effective
            .and_then(|config| config.get("effort"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let fast_mode = effective
            .and_then(|config| config.get("fast_mode"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let mut field_sources = value
            .get("fieldSources")
            .or_else(|| value.get("field_sources"))
            .and_then(serde_json::Value::as_object)
            .map(|sources| {
                sources
                    .iter()
                    .filter_map(|(field, source)| {
                        source
                            .as_str()
                            .map(|source| (field.clone(), source.to_owned()))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        field_sources.sort_by(|a, b| a.0.cmp(&b.0));
        let context_source = value
            .get("contextSource")
            .or_else(|| value.get("context_source"))
            .and_then(serde_json::Value::as_object)
            .and_then(|source| {
                let package = source.get("package")?.as_str()?;
                let role = source.get("role")?.as_str()?;
                let empty = source
                    .get("empty")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                Some(if empty {
                    format!("{package}/{role} (empty)")
                } else {
                    format!("{package}/{role}")
                })
            });
        Some(Self {
            role_id,
            configured: value
                .get("configured")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            effective_model,
            effective_source: value
                .get("effectiveSource")
                .or_else(|| value.get("effective_source"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            exact_fields,
            payload_keys,
            effort,
            fast_mode,
            field_sources,
            context_source,
        })
    }

    pub fn display_name(&self) -> &str {
        if self.role_id == "main" {
            "MAIN"
        } else {
            &self.role_id
        }
    }
}

#[derive(Debug)]
pub struct RolesModalState {
    pub window: ModalWindowState,
    pub entries: Vec<RoleModalEntry>,
    pub selected: usize,
    pub loading: bool,
    pub status: Option<String>,
}

impl RolesModalState {
    pub fn loading() -> Self {
        Self {
            window: ModalWindowState::new(),
            entries: Vec::new(),
            selected: 0,
            loading: true,
            status: Some("Loading fixed Runtime Roles…".to_owned()),
        }
    }

    pub fn selected_entry(&self) -> Option<&RoleModalEntry> {
        self.entries.get(self.selected)
    }

    pub fn select_next(&mut self) {
        if self.selected + 1 < self.entries.len() {
            self.selected += 1;
        }
    }

    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn begin_reload(&mut self, status: impl Into<String>) {
        self.loading = true;
        self.status = Some(status.into());
    }

    pub fn apply_fast_mode(&mut self, role_id: &str, enabled: bool, status: impl Into<String>) {
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.role_id == role_id)
        {
            entry.fast_mode = enabled;
        }
        self.loading = false;
        self.status = Some(status.into());
    }

    pub fn fail(&mut self, error: impl Into<String>) {
        self.loading = false;
        self.status = Some(format!("Error: {}", error.into()));
    }

    pub fn apply_response(&mut self, response: &str) -> Result<(), String> {
        let value: serde_json::Value = serde_json::from_str(response)
            .map_err(|error| format!("invalid Role response: {error}"))?;
        let result = value.get("result").unwrap_or(&value);
        let roles = result
            .get("roles")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "Role response does not contain roles".to_owned())?;
        let entries = roles
            .iter()
            .filter_map(RoleModalEntry::from_value)
            .collect::<Vec<_>>();
        if entries.len() != 12 {
            return Err(format!(
                "expected 12 fixed Runtime Roles, received {}",
                entries.len()
            ));
        }
        self.entries = entries;
        self.selected = self.selected.min(self.entries.len().saturating_sub(1));
        self.loading = false;
        if self.status.as_deref() == Some("Loading fixed Runtime Roles…") {
            self.status = None;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RolesModalOutcome {
    Close,
    Changed,
    Configure(String),
    Reset(String),
    Test(String),
    ToggleFast { role_id: String, enabled: bool },
    Unchanged,
}

pub fn handle_roles_key(state: &mut RolesModalState, key: &KeyEvent) -> RolesModalOutcome {
    if key.kind != KeyEventKind::Press {
        return RolesModalOutcome::Unchanged;
    }
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => RolesModalOutcome::Close,
        KeyCode::Down | KeyCode::Char('j') => {
            state.select_next();
            RolesModalOutcome::Changed
        }
        KeyCode::Up | KeyCode::Char('k') => {
            state.select_prev();
            RolesModalOutcome::Changed
        }
        KeyCode::Enter | KeyCode::Char('e') => state
            .selected_entry()
            .map(|entry| RolesModalOutcome::Configure(entry.role_id.clone()))
            .unwrap_or(RolesModalOutcome::Unchanged),
        KeyCode::Char('r') => match state.selected_entry() {
            Some(entry) if entry.role_id != "main" => {
                RolesModalOutcome::Reset(entry.role_id.clone())
            }
            Some(_) => {
                state.status =
                    Some("MAIN is managed by /model and cannot be reset here".to_owned());
                RolesModalOutcome::Changed
            }
            None => RolesModalOutcome::Unchanged,
        },
        KeyCode::Char('t') => state
            .selected_entry()
            .map(|entry| RolesModalOutcome::Test(entry.role_id.clone()))
            .unwrap_or(RolesModalOutcome::Unchanged),
        KeyCode::Char('f') => state
            .selected_entry()
            .map(|entry| RolesModalOutcome::ToggleFast {
                role_id: entry.role_id.clone(),
                enabled: !entry.fast_mode,
            })
            .unwrap_or(RolesModalOutcome::Unchanged),
        _ => RolesModalOutcome::Unchanged,
    }
}

pub fn render_roles_modal(
    buf: &mut Buffer,
    area: Rect,
    state: &mut RolesModalState,
    compact: bool,
    theme: &Theme,
) {
    let shortcuts = [
        Shortcut {
            label: "↑/↓ nav",
            clickable: false,
            id: 0,
        },
        Shortcut {
            label: "Enter edit",
            clickable: false,
            id: 0,
        },
        Shortcut {
            label: "f fast",
            clickable: false,
            id: 0,
        },
        Shortcut {
            label: "r reset",
            clickable: false,
            id: 0,
        },
        Shortcut {
            label: "t test",
            clickable: false,
            id: 0,
        },
        Shortcut {
            label: "Esc close",
            clickable: false,
            id: 0,
        },
    ];
    let config = ModalWindowConfig {
        title: "Runtime Roles",
        tabs: None,
        shortcuts: &shortcuts,
        sizing: ModalSizing {
            width_pct: 0.76,
            max_width: 118,
            min_width: 56,
            v_margin: 3,
            h_pad: 2,
            v_pad: 1,
            footer_lines: 2,
        }
        .with_compact(compact),
        fold_info: None,
    };
    let Some(content) =
        modal_window::render_modal_window(buf, area, &mut state.window, &config, theme)
    else {
        return;
    };
    if state.loading && state.entries.is_empty() {
        Paragraph::new(state.status.as_deref().unwrap_or("Loading…"))
            .style(Style::default().fg(theme.text_secondary))
            .render(content.content, buf);
        return;
    }

    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(36), Constraint::Percentage(64)])
        .split(content.content);
    let list_area = panes[0];
    let detail_area = Rect {
        x: panes[1].x.saturating_add(2),
        y: panes[1].y,
        width: panes[1].width.saturating_sub(2),
        height: panes[1].height,
    };
    if panes[1].x > 0 {
        for y in panes[1].y..panes[1].bottom() {
            buf.set_string(panes[1].x, y, "│", Style::default().fg(theme.gray_dim));
        }
    }

    let items = state
        .entries
        .iter()
        .map(|entry| {
            let source = if entry.role_id == "main" {
                "config.toml".to_owned()
            } else if entry.configured {
                "exact".to_owned()
            } else {
                format!("← {}", entry.effective_source.as_deref().unwrap_or("main"))
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    entry.display_name().to_owned(),
                    Style::default().fg(theme.text_primary),
                ),
                Span::styled(
                    format!("  {source}"),
                    Style::default().fg(theme.text_secondary),
                ),
            ]))
        })
        .collect::<Vec<_>>();
    let mut list_state = ListState::default();
    if !state.entries.is_empty() {
        list_state.select(Some(state.selected));
    }
    ratatui::widgets::StatefulWidget::render(
        List::new(items).highlight_style(
            Style::default()
                .fg(theme.accent_assistant)
                .add_modifier(Modifier::BOLD),
        ),
        list_area,
        buf,
        &mut list_state,
    );

    let mut lines = Vec::new();
    if let Some(entry) = state.selected_entry() {
        lines.push(Line::styled(
            entry.display_name().to_owned(),
            Style::default()
                .fg(theme.accent_assistant)
                .add_modifier(Modifier::BOLD),
        ));
        lines.push(Line::default());
        lines.push(detail_line(
            "Effective model",
            entry.effective_model.as_deref().unwrap_or("not selected"),
            theme,
        ));
        lines.push(detail_line(
            "Execution source",
            if entry.role_id == "main" {
                "config.toml"
            } else {
                entry.effective_source.as_deref().unwrap_or("unconfigured")
            },
            theme,
        ));
        lines.push(detail_line(
            "Effort",
            entry.effort.as_deref().unwrap_or("model default"),
            theme,
        ));
        lines.push(detail_line(
            "Fast mode",
            if entry.fast_mode { "on" } else { "off" },
            theme,
        ));
        lines.push(detail_line(
            "Context",
            entry.context_source.as_deref().unwrap_or("none"),
            theme,
        ));
        lines.push(Line::default());
        lines.push(detail_line(
            "Exact overrides",
            if entry.exact_fields.is_empty() {
                "none"
            } else {
                // Temporary owned line is constructed below when needed.
                ""
            },
            theme,
        ));
        if !entry.exact_fields.is_empty() {
            lines.pop();
            lines.push(detail_line_owned(
                "Exact overrides",
                entry.exact_fields.join(", "),
                theme,
            ));
        }
        lines.push(detail_line_owned(
            "Payload keys",
            if entry.payload_keys.is_empty() {
                "none".to_owned()
            } else {
                entry.payload_keys.join(", ")
            },
            theme,
        ));
        if !entry.field_sources.is_empty() {
            lines.push(Line::default());
            lines.push(Line::styled(
                "Field sources",
                Style::default()
                    .fg(theme.text_secondary)
                    .add_modifier(Modifier::BOLD),
            ));
            for (field, source) in &entry.field_sources {
                lines.push(Line::from(format!("  {field}: {source}")));
            }
        }
    }
    if let Some(status) = &state.status {
        lines.push(Line::default());
        lines.push(Line::styled(
            status.clone(),
            Style::default().fg(theme.accent_running),
        ));
    }
    Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .render(detail_area, buf);
}

fn detail_line<'a>(label: &'a str, value: &'a str, theme: &Theme) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            format!("{label}: "),
            Style::default().fg(theme.text_secondary),
        ),
        Span::styled(value, Style::default().fg(theme.text_primary)),
    ])
}

fn detail_line_owned(label: &str, value: String, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label}: "),
            Style::default().fg(theme.text_secondary),
        ),
        Span::styled(value, Style::default().fg(theme.text_primary)),
    ])
}

#[cfg(test)]
mod tests {
    use super::{RolesModalOutcome, RolesModalState, handle_roles_key};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn response() -> String {
        let roles = [
            "main", "general", "explore", "implement", "review", "test", "compact",
            "summary", "title", "planner", "strategist", "skeptic",
        ]
        .into_iter()
        .map(|role| {
            serde_json::json!({
                "roleId": role,
                "configured": role == "review" || role == "main",
                "effectiveSource": if role == "main" { serde_json::Value::Null } else { serde_json::Value::String("main".into()) },
                "config": if role == "review" {
                    serde_json::json!({"effort":"high","payload":{"secret":"[REDACTED]"}})
                } else if role == "main" {
                    serde_json::json!({"provider":"example","model":"alpha"})
                } else {
                    serde_json::Value::Null
                },
                "effectiveConfig": {"provider":"example","model":"alpha","effort":"high","fast_mode":false},
                "fieldSources": {"provider":"main","model":"main","effort":"review","fast_mode":"main","payload":"review"},
                "contextSource": {"package":"default","role":role,"empty":false}
            })
        })
        .collect::<Vec<_>>();
        serde_json::json!({"result":{"roles":roles}}).to_string()
    }

    #[test]
    fn parses_all_fixed_roles_without_payload_values() {
        let mut state = RolesModalState::loading();
        state.apply_response(&response()).unwrap();
        assert_eq!(state.entries.len(), 12);
        let review = state
            .entries
            .iter()
            .find(|entry| entry.role_id == "review")
            .unwrap();
        assert_eq!(review.payload_keys, vec!["secret"]);
        assert_eq!(review.effective_model.as_deref(), Some("example/alpha"));
        assert!(!format!("{review:?}").contains("must-not-render"));
    }

    #[test]
    fn reset_is_disabled_for_main_and_available_for_fixed_children() {
        let mut state = RolesModalState::loading();
        state.apply_response(&response()).unwrap();
        let reset_main = handle_roles_key(
            &mut state,
            &KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
        );
        assert_eq!(reset_main, RolesModalOutcome::Changed);
        assert!(state.status.as_deref().unwrap().contains("/model"));
        state.selected = 4;
        let reset_review = handle_roles_key(
            &mut state,
            &KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
        );
        assert_eq!(reset_review, RolesModalOutcome::Reset("review".into()));
    }

    #[test]
    fn fast_mode_toggle_uses_effective_value() {
        let mut state = RolesModalState::loading();
        state.apply_response(&response()).unwrap();
        state.selected = 4;
        let outcome = handle_roles_key(
            &mut state,
            &KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE),
        );
        assert_eq!(
            outcome,
            RolesModalOutcome::ToggleFast {
                role_id: "review".into(),
                enabled: true,
            }
        );
    }

    #[test]
    fn fast_mode_response_updates_session_managed_main_without_a_list_reload() {
        let mut state = RolesModalState::loading();
        state.apply_response(&response()).unwrap();

        state.apply_fast_mode("main", true, "Role fast mode updated");

        assert!(state.entries[0].fast_mode);
        assert_eq!(state.status.as_deref(), Some("Role fast mode updated"));
        assert!(!state.loading);
    }

    #[test]
    fn list_reload_preserves_action_status() {
        let mut state = RolesModalState::loading();
        state.apply_response(&response()).unwrap();
        state.begin_reload("Role override reset");

        state.apply_response(&response()).unwrap();

        assert_eq!(state.status.as_deref(), Some("Role override reset"));
        assert!(!state.loading);
    }
}
