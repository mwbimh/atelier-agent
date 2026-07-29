//! Subagent catalog pane — browseable list of compile-time built-in types.
//!
//! Runtime discovery is intentionally absent: external catalog metadata never
//! contributes entries to this pane.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crossterm::event::{KeyEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::StatefulWidget;

use crate::app::bundle::BundleState;
use crate::appearance::LayoutConfig;
use crate::scrollback::layout::HorizontalLayout;
use crate::theme::Theme;

use super::list_pane::{
    ListItem, ListPane, ListPaneConfig, ListPaneState, ListPaneStyle, WrapMode,
};
use super::overlay::OverlayState;

// ---------------------------------------------------------------------------
// CatalogEntry
// ---------------------------------------------------------------------------

struct CatalogEntry {
    id: u64,
    label: String,
    styled: Line<'static>,
    is_header: bool,
    kind: Option<&'static str>,
}

impl ListItem for CatalogEntry {
    fn content(&self) -> &Line<'_> {
        &self.styled
    }

    fn stable_id(&self) -> u64 {
        self.id
    }

    fn is_selectable(&self) -> bool {
        !self.is_header
    }

    fn search_text(&self) -> &str {
        &self.label
    }
}

// ---------------------------------------------------------------------------
// SubagentCatalogPane
// ---------------------------------------------------------------------------

const MAX_CATALOG_HEIGHT: u16 = 8;
const MAX_CATALOG_FRACTION: f32 = 0.15;

pub struct SubagentCatalogPane {
    entries: Vec<CatalogEntry>,
    pub list_state: ListPaneState,
    list_style: ListPaneStyle,
    pub overlay: OverlayState,
}

impl Default for SubagentCatalogPane {
    fn default() -> Self {
        Self::new()
    }
}

impl SubagentCatalogPane {
    pub fn new() -> Self {
        let config = ListPaneConfig {
            follow_enabled: false,
            wrap_toggle_enabled: false,
            search_enabled: true,
            copy_enabled: false,
            show_selection_when_unfocused: false,
            visual_select_enabled: false,
            filter_enabled: true,
            goto_line_enabled: false,
        };
        let list_state = ListPaneState::new_with_config(WrapMode::NoWrap, false, config);
        Self {
            entries: Vec::new(),
            list_state,
            list_style: ListPaneStyle::default(),
            overlay: OverlayState::hidden(),
        }
    }

    // -- Data sync -----------------------------------------------------------

    pub fn sync_from_bundle(&mut self, _state: &BundleState) {
        self.entries.clear();

        let theme = Theme::current();
        let header_style = Style::default()
            .fg(theme.gray_bright)
            .add_modifier(Modifier::BOLD);
        let item_style = Style::default().fg(theme.text_primary);
        let desc_style = Style::default().fg(theme.gray_bright);
        let header = "Built-in Subagents";
        let items = [
            (
                "general-purpose",
                "General-purpose implementation and multi-step work",
            ),
            ("explore", "Fast read-only codebase exploration"),
            ("plan", "Read-only implementation planning"),
        ];

        let mut hasher = DefaultHasher::new();
        header.hash(&mut hasher);
        self.entries.push(CatalogEntry {
            id: hasher.finish(),
            styled: Line::from(Span::styled(header, header_style)),
            label: header.to_owned(),
            is_header: true,
            kind: None,
        });
        for (item, description) in items {
            let mut hasher = DefaultHasher::new();
            header.hash(&mut hasher);
            item.hash(&mut hasher);
            self.entries.push(CatalogEntry {
                id: hasher.finish(),
                label: item.to_owned(),
                styled: Line::from(vec![
                    Span::styled(format!("  {item}"), item_style),
                    Span::styled(format!(" \u{2014} {description}"), desc_style),
                ]),
                is_header: false,
                kind: Some("subagent"),
            });
        }
    }

    // -- Visibility ----------------------------------------------------------

    pub fn is_visible(&self) -> bool {
        self.overlay.visible
    }

    pub fn on_state_change(&mut self) {
        if !self.overlay.visible {
            self.list_state.close_input_bar();
        }
    }

    pub fn desired_height(&self, view_height: u16) -> u16 {
        if !self.overlay.visible {
            return 0;
        }
        if view_height < 12 {
            return 0;
        }
        let count = self.entries.len();
        if count == 0 {
            return 1;
        }
        let fraction_cap = (view_height as f32 * MAX_CATALOG_FRACTION).floor() as u16;
        let max = MAX_CATALOG_HEIGHT.min(fraction_cap).max(1);
        (count as u16).min(max).max(1)
    }

    /// Returns `(kind, name)` of the currently selected non-header entry.
    ///
    /// `kind` is always `"subagent"` for selectable entries.
    pub fn selected_entry(&self) -> Option<(&str, &str)> {
        let selected_id = self.list_state.selected_id()?;
        let entry = self.entries.iter().find(|e| e.id == selected_id)?;
        if entry.is_header {
            return None;
        }
        Some((entry.kind?, &entry.label))
    }

    // -- Input handling ------------------------------------------------------

    pub fn handle_key(&mut self, key: &KeyEvent) -> bool {
        if self.entries.is_empty() {
            return false;
        }
        self.list_state.handle_key_event(key, &self.entries)
    }

    pub fn handle_scroll(&mut self, lines: i32, col: u16, row: u16) {
        let max = match self.list_state.viewport_height() {
            0..=5 => 1,
            6..=10 => 2,
            _ => lines.unsigned_abs() as i32,
        };
        let capped = lines.signum() * lines.abs().min(max);
        self.list_state
            .handle_scroll_event(capped, col, row, &self.entries);
    }

    pub fn handle_mouse(&mut self, kind: MouseEventKind, col: u16, row: u16, area: Rect) -> bool {
        if self.entries.is_empty() {
            return false;
        }
        self.list_state
            .handle_mouse_event(kind, col, row, area, &self.entries)
    }

    // -- Rendering -----------------------------------------------------------

    fn content_area(area: Rect, layout_cfg: &LayoutConfig) -> Rect {
        let pad_left = HorizontalLayout::ACCENT + layout_cfg.block_pad_left;
        let pad_right = layout_cfg.block_pad_right;
        Rect {
            x: area.x + pad_left,
            y: area.y,
            width: area.width.saturating_sub(pad_left + pad_right),
            height: area.height,
        }
    }

    pub fn render(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        focused: bool,
        layout_cfg: &LayoutConfig,
    ) {
        let inner = Self::content_area(area, layout_cfg);
        if self.entries.is_empty() {
            if inner.height > 0 && inner.width > 0 {
                let theme = Theme::current();
                let span =
                    Span::styled("No bundled items.", Style::default().fg(theme.gray_bright));
                buf.set_span(inner.x, inner.y, &span, inner.width);
            }
            return;
        }
        self.list_state
            .prepare_layout(&self.entries, inner.width, inner.height);
        ListPane::new(&self.entries)
            .focused(focused)
            .style(self.list_style)
            .render(inner, buf, &mut self.list_state);
    }
}

#[cfg(test)]
mod tests {
    use super::SubagentCatalogPane;
    use crate::app::bundle::BundleState;

    fn synced() -> SubagentCatalogPane {
        let mut pane = SubagentCatalogPane::new();
        pane.sync_from_bundle(&BundleState::default());
        pane
    }

    #[test]
    fn catalog_contains_only_the_fixed_builtin_subagent_types() {
        let pane = synced();
        let labels = pane
            .entries
            .iter()
            .filter(|entry| !entry.is_header)
            .map(|entry| entry.label.as_str())
            .collect::<Vec<_>>();

        assert_eq!(labels, vec!["general-purpose", "explore", "plan"]);
        assert!(
            pane.entries
                .iter()
                .all(|entry| { entry.is_header || entry.kind == Some("subagent") })
        );
    }

    #[test]
    fn catalog_does_not_depend_on_bundle_cache_or_legacy_metadata() {
        let mut pane = SubagentCatalogPane::new();
        pane.sync_from_bundle(&BundleState {
            has_cache: false,
            version: "ignored".into(),
            skills: vec!["also-ignored".into()],
        });

        assert_eq!(pane.entries.len(), 4);
    }

    #[test]
    fn catalog_is_hidden_until_its_overlay_is_opened() {
        let pane = synced();
        assert!(!pane.is_visible());
        assert_eq!(pane.desired_height(40), 0);
    }

    #[test]
    fn selected_entry_reports_the_fixed_subagent_kind() {
        let mut pane = synced();
        pane.list_state.select_by_id(pane.entries[1].id);

        assert_eq!(pane.selected_entry(), Some(("subagent", "general-purpose")));
    }
}
