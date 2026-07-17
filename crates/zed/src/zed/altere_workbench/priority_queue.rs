use altere_ui::{PriorityQueueRow, PriorityQueueRowData};
use anyhow::Context as _;
use db::kvp::KeyValueStore;
use gpui::{
    AnyElement, App, AppContext as _, AsyncWindowContext, Context, Empty, Entity, EventEmitter,
    FocusHandle, Focusable, IntoElement, KeyBinding, ListAlignment, ListState, Pixels, Render,
    Role, Task, TaskExt, WeakEntity, Window, actions, div, list, px,
};
use menu::{SelectFirst, SelectLast, SelectNext, SelectPrevious};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use ui::{Button, IconName, LabelSize, SpinnerLabel, prelude::*};
use util::ResultExt as _;
use workspace::{
    Workspace,
    dock::{DockPosition, Panel, PanelEvent},
    notifications::DetachAndPromptErr,
};

actions!(
    altere_queue,
    [
        ToggleFocus,
        OpenSelected,
        ScrollUp,
        ScrollDown,
        MoreImportant,
        LessImportant
    ]
);

const PANEL_POSITION_KEY: &str = "altere_priority_queue_panel_position";

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct QueueProjection {
    version: u8,
    book_size: usize,
    protected_percentile: u8,
    rows: Vec<QueueProjectionRow>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueueProjectionRow {
    id: String,
    path: PathBuf,
    fingerprint: String,
    name: String,
    rank: usize,
    percentile: f32,
    claim_cells: usize,
    lane: String,
    priority: f32,
    reads: usize,
    protected: bool,
    rule: String,
}

pub(super) fn queue_invocation(runtime: &Path, collection: &Path) -> [String; 5] {
    [
        runtime.to_string_lossy().into_owned(),
        "rotation".into(),
        "queue".into(),
        "--collection".into(),
        collection.to_string_lossy().into_owned(),
    ]
}

fn priority_invocation(
    runtime: &Path,
    collection: &Path,
    source: &str,
    priority: u8,
    expected_fingerprint: &str,
) -> [String; 11] {
    [
        runtime.to_string_lossy().into_owned(),
        "priority".into(),
        "set".into(),
        "--collection".into(),
        collection.to_string_lossy().into_owned(),
        "--source".into(),
        source.into(),
        "--value".into(),
        priority.to_string(),
        "--expected-fingerprint".into(),
        expected_fingerprint.into(),
    ]
}

pub(super) async fn load_projection() -> anyhow::Result<QueueProjection> {
    let (bun, runtime, collection) = super::runtime_configuration()?;
    let output = smol::process::Command::new(bun)
        .args(queue_invocation(&runtime, &collection))
        .output()
        .await?;
    anyhow::ensure!(
        output.status.success(),
        "Altere queue runtime failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let projection: QueueProjection =
        serde_json::from_slice(&output.stdout).context("decoding the Altere queue projection")?;
    anyhow::ensure!(
        projection.version == 1,
        "unsupported Altere queue projection"
    );
    Ok(projection)
}

async fn set_priority(
    source: &str,
    priority: u8,
    expected_fingerprint: &str,
) -> anyhow::Result<QueueProjection> {
    let (bun, runtime, collection) = super::runtime_configuration()?;
    let output = smol::process::Command::new(bun)
        .args(priority_invocation(
            &runtime,
            &collection,
            source,
            priority,
            expected_fingerprint,
        ))
        .output()
        .await?;
    anyhow::ensure!(
        output.status.success(),
        "Altere priority mutation failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    load_projection().await
}

pub(crate) struct AlterePriorityQueuePanel {
    workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
    position: DockPosition,
    projection: Option<QueueProjection>,
    load_state: QueueLoadState,
    list_state: ListState,
    selected_index: usize,
    _refresh_task: Task<()>,
}

enum QueueLoadState {
    Loading,
    Ready,
    Error(String),
}

impl QueueLoadState {
    fn error(error: anyhow::Error) -> Self {
        Self::Error(error.to_string())
    }
}

fn clamp_selected_index(selected_index: usize, row_count: usize) -> usize {
    selected_index.min(row_count.saturating_sub(1))
}

fn selected_index_after_refresh(
    selected_id: Option<&str>,
    previous_index: usize,
    rows: &[QueueProjectionRow],
) -> usize {
    selected_id
        .and_then(|selected_id| rows.iter().position(|row| row.id == selected_id))
        .unwrap_or_else(|| clamp_selected_index(previous_index, rows.len()))
}

fn stepped_priority(priority: f32, delta: i32) -> u8 {
    ((priority.round() as i32 + delta).clamp(1, 100)) as u8
}

fn path_is_dirty(workspace: &Workspace, path: &Path, cx: &App) -> bool {
    workspace.items(cx).any(|item| {
        item.is_dirty(cx)
            && item
                .project_path(cx)
                .and_then(|project_path| {
                    workspace
                        .project()
                        .read(cx)
                        .absolute_path(&project_path, cx)
                })
                .is_some_and(|item_path| item_path == path)
    })
}

fn move_selection(selected_index: usize, row_count: usize, delta: isize) -> usize {
    if row_count == 0 {
        return 0;
    }

    selected_index
        .saturating_add_signed(delta)
        .min(row_count - 1)
}

fn half_page_rows(viewport_height: Pixels) -> usize {
    ((f32::from(viewport_height) / 28.).floor() as usize / 2).max(1)
}

fn position_from_storage(value: &str) -> Option<DockPosition> {
    match value {
        "left" => Some(DockPosition::Left),
        "bottom" => Some(DockPosition::Bottom),
        "right" => Some(DockPosition::Right),
        _ => None,
    }
}

fn position_for_storage(position: DockPosition) -> String {
    match position {
        DockPosition::Left => "left",
        DockPosition::Bottom => "bottom",
        DockPosition::Right => "right",
    }
    .to_string()
}

impl AlterePriorityQueuePanel {
    pub(crate) async fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: AsyncWindowContext,
    ) -> anyhow::Result<Entity<Self>> {
        let panel_workspace = workspace.clone();
        workspace.update_in(&mut cx, move |_, _, cx| {
            cx.new(|cx| {
                let position = KeyValueStore::global(cx)
                    .read_kvp(PANEL_POSITION_KEY)
                    .log_err()
                    .flatten()
                    .as_deref()
                    .and_then(position_from_storage)
                    .unwrap_or(DockPosition::Bottom);
                let mut panel = Self {
                    workspace: panel_workspace,
                    focus_handle: cx.focus_handle(),
                    position,
                    projection: None,
                    load_state: QueueLoadState::Loading,
                    list_state: ListState::new(0, ListAlignment::Top, px(48.)),
                    selected_index: 0,
                    _refresh_task: Task::ready(()),
                };
                panel.refresh(cx);
                panel
            })
        })
    }

    pub(super) fn refresh(&mut self, cx: &mut Context<Self>) {
        self.load_state = QueueLoadState::Loading;
        cx.notify();
        self._refresh_task = cx.spawn(async move |this, cx| {
            let result = load_projection().await;
            if this
                .update(cx, |this, cx| this.finish_refresh(result, cx))
                .is_err()
            {
                return;
            }
        });
    }

    fn finish_refresh(&mut self, result: anyhow::Result<QueueProjection>, cx: &mut Context<Self>) {
        match result {
            Ok(projection) => {
                let selected_id = self
                    .projection
                    .as_ref()
                    .and_then(|projection| projection.rows.get(self.selected_index))
                    .map(|row| row.id.clone());
                self.selected_index = selected_index_after_refresh(
                    selected_id.as_deref(),
                    self.selected_index,
                    &projection.rows,
                );
                self.list_state.reset(projection.rows.len());
                self.projection = Some(projection);
                self.load_state = QueueLoadState::Ready;
            }
            Err(error) => {
                self.load_state = QueueLoadState::error(error);
            }
        }
        cx.notify();
    }

    fn open_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self
            .projection
            .as_ref()
            .and_then(|projection| projection.rows.get(self.selected_index))
        else {
            return;
        };
        let path = row.path.clone();
        let workspace = self.workspace.clone();
        cx.spawn_in(window, async move |_, cx| {
            workspace
                .update_in(cx, |workspace, window, cx| {
                    workspace.open_abs_path(path, Default::default(), window, cx)
                })?
                .await?;
            anyhow::Ok(())
        })
        .detach_and_prompt_err(
            "Altere could not open the queue item",
            window,
            cx,
            |_, _, _| None,
        );
    }

    fn select(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let row_count = self
            .projection
            .as_ref()
            .map_or(0, |projection| projection.rows.len());
        if index >= row_count {
            return;
        }
        self.selected_index = index;
        self.list_state.scroll_to_reveal_item(index);
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn row_count(&self) -> usize {
        self.projection
            .as_ref()
            .map_or(0, |projection| projection.rows.len())
    }

    fn move_selection_by(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let index = move_selection(self.selected_index, self.row_count(), delta);
        self.select(index, window, cx);
    }

    fn select_next(&mut self, _: &SelectNext, window: &mut Window, cx: &mut Context<Self>) {
        self.move_selection_by(1, window, cx);
    }

    fn select_previous(&mut self, _: &SelectPrevious, window: &mut Window, cx: &mut Context<Self>) {
        self.move_selection_by(-1, window, cx);
    }

    fn select_first(&mut self, _: &SelectFirst, window: &mut Window, cx: &mut Context<Self>) {
        self.select(0, window, cx);
    }

    fn select_last(&mut self, _: &SelectLast, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.row_count().checked_sub(1) else {
            return;
        };
        self.select(index, window, cx);
    }

    fn scroll_up(&mut self, _: &ScrollUp, window: &mut Window, cx: &mut Context<Self>) {
        let rows = half_page_rows(self.list_state.viewport_bounds().size.height);
        self.move_selection_by(-(rows as isize), window, cx);
    }

    fn scroll_down(&mut self, _: &ScrollDown, window: &mut Window, cx: &mut Context<Self>) {
        let rows = half_page_rows(self.list_state.viewport_bounds().size.height);
        self.move_selection_by(rows as isize, window, cx);
    }

    fn open_selected_action(
        &mut self,
        _: &OpenSelected,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_selected(window, cx);
    }

    fn change_selected_priority(&mut self, delta: i32, cx: &mut Context<Self>) {
        let Some(row) = self
            .projection
            .as_ref()
            .and_then(|projection| projection.rows.get(self.selected_index))
            .cloned()
        else {
            return;
        };

        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        if path_is_dirty(workspace.read(cx), &row.path, cx) {
            self.load_state =
                QueueLoadState::Error(format!("Save {} before changing its priority.", row.name));
            cx.notify();
            return;
        }

        let priority = stepped_priority(row.priority, delta);
        if priority as f32 == row.priority {
            return;
        }
        self.load_state = QueueLoadState::Loading;
        cx.notify();
        self._refresh_task = cx.spawn(async move |this, cx| {
            let result = set_priority(&row.id, priority, &row.fingerprint).await;
            let _ = this.update(cx, |this, cx| this.finish_refresh(result, cx));
        });
    }

    fn more_important(&mut self, _: &MoreImportant, _: &mut Window, cx: &mut Context<Self>) {
        self.change_selected_priority(-5, cx);
    }

    fn less_important(&mut self, _: &LessImportant, _: &mut Window, cx: &mut Context<Self>) {
        self.change_selected_priority(5, cx);
    }
}

pub(super) fn key_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("j", SelectNext, Some("AlterePriorityQueue")),
        KeyBinding::new("down", SelectNext, Some("AlterePriorityQueue")),
        KeyBinding::new("k", SelectPrevious, Some("AlterePriorityQueue")),
        KeyBinding::new("up", SelectPrevious, Some("AlterePriorityQueue")),
        KeyBinding::new("g g", SelectFirst, Some("AlterePriorityQueue")),
        KeyBinding::new("shift-g", SelectLast, Some("AlterePriorityQueue")),
        KeyBinding::new("ctrl-u", ScrollUp, Some("AlterePriorityQueue")),
        KeyBinding::new("ctrl-d", ScrollDown, Some("AlterePriorityQueue")),
        KeyBinding::new("enter", OpenSelected, Some("AlterePriorityQueue")),
        KeyBinding::new("=", MoreImportant, Some("AlterePriorityQueue")),
        KeyBinding::new("shift-=", MoreImportant, Some("AlterePriorityQueue")),
        KeyBinding::new("-", LessImportant, Some("AlterePriorityQueue")),
    ]
}

pub(super) fn register_action(workspace: &mut Workspace) {
    workspace.register_action(|workspace, _: &ToggleFocus, window, cx| {
        workspace.toggle_panel_focus::<AlterePriorityQueuePanel>(window, cx);
    });
}

impl Panel for AlterePriorityQueuePanel {
    fn persistent_name() -> &'static str {
        "AlterePriorityQueuePanel"
    }

    fn panel_key() -> &'static str {
        "AlterePriorityQueuePanel"
    }

    fn position(&self, _: &Window, _: &App) -> DockPosition {
        self.position
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(
            position,
            DockPosition::Left | DockPosition::Bottom | DockPosition::Right
        )
    }

    fn set_position(&mut self, position: DockPosition, _: &mut Window, cx: &mut Context<Self>) {
        self.position = position;
        let key_value_store = KeyValueStore::global(cx);
        let position = position_for_storage(position);
        cx.background_spawn(async move {
            key_value_store
                .write_kvp(PANEL_POSITION_KEY.to_string(), position)
                .await
        })
        .detach_and_log_err(cx);
    }

    fn default_size(&self, _: &Window, _: &App) -> Pixels {
        px(280.)
    }

    fn icon(&self, _: &Window, _: &App) -> Option<IconName> {
        Some(IconName::ListTree)
    }

    fn icon_tooltip(&self, _: &Window, _: &App) -> Option<&'static str> {
        Some("Priority Queue")
    }

    fn icon_label(&self, _: &Window, _: &App) -> Option<String> {
        self.projection
            .as_ref()
            .map(|projection| projection.book_size.to_string())
    }

    fn toggle_action(&self) -> Box<dyn gpui::Action> {
        Box::new(ToggleFocus)
    }

    fn starts_open(&self, _: &Window, _: &App) -> bool {
        true
    }

    fn activation_priority(&self) -> u32 {
        1
    }
}

impl Focusable for AlterePriorityQueuePanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<PanelEvent> for AlterePriorityQueuePanel {}

impl AlterePriorityQueuePanel {
    fn render_row(
        &mut self,
        index: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(projection) = self.projection.as_ref() else {
            return Empty.into_any();
        };
        let Some(row) = projection.rows.get(index) else {
            return Empty.into_any();
        };

        let colors = cx.theme().colors().clone();
        let selected = index == self.selected_index;
        let row_index = index;
        let tooltip = format!("{} · {} · {}", row.id, row.rule, row.path.display());
        let marks_floor = row.protected
            && projection
                .rows
                .get(index + 1)
                .is_none_or(|next_row| !next_row.protected);
        let protected_percentile = projection.protected_percentile;

        v_flex()
            .w_full()
            .child(
                PriorityQueueRow::new(
                    ("altere-priority-queue-row", index),
                    PriorityQueueRowData::new(
                        row.rank,
                        row.percentile,
                        row.claim_cells,
                        row.lane.clone(),
                        row.name.clone(),
                        row.priority,
                        row.reads,
                    ),
                )
                .selected(selected)
                .on_click(
                    cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                        this.select(row_index, window, cx);
                        if event.click_count() >= 2 {
                            this.open_selected(window, cx);
                        }
                    }),
                )
                .tooltip(tooltip),
            )
            .when(marks_floor, |element| {
                element.child(
                    h_flex()
                        .w_full()
                        .h(px(20.))
                        .px_2()
                        .gap_2()
                        .items_center()
                        .border_t_1()
                        .border_color(colors.text_accent)
                        .bg(colors.element_selected.opacity(0.35))
                        .text_color(colors.text_accent)
                        .child("FLOOR")
                        .child(div().h(px(1.)).flex_1().bg(colors.text_accent))
                        .child(format!("TOP {protected_percentile}% PROTECTED")),
                )
            })
            .into_any_element()
    }

    fn render_load_state(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let colors = cx.theme().colors().clone();
        let error_color = cx.theme().status().error;
        match &self.load_state {
            QueueLoadState::Loading if self.projection.is_none() => Some(
                h_flex()
                    .w_full()
                    .py_4()
                    .gap_2()
                    .justify_center()
                    .text_color(colors.text_muted)
                    .child(SpinnerLabel::new())
                    .child("Loading priority queue…")
                    .into_any_element(),
            ),
            QueueLoadState::Loading => Some(
                h_flex()
                    .w_full()
                    .px_2()
                    .py_1()
                    .gap_2()
                    .border_b_1()
                    .border_color(colors.border_variant)
                    .text_color(colors.text_muted)
                    .child(SpinnerLabel::new())
                    .child("Refreshing…")
                    .into_any_element(),
            ),
            QueueLoadState::Error(message) => {
                let message = message.clone();
                Some(
                    h_flex()
                        .w_full()
                        .px_2()
                        .py_1()
                        .gap_2()
                        .justify_between()
                        .border_b_1()
                        .border_color(error_color)
                        .text_color(error_color)
                        .child(div().min_w_0().overflow_hidden().child(message))
                        .child(
                            Button::new("retry-altere-priority-queue", "Retry")
                                .label_size(LabelSize::Small)
                                .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                        )
                        .into_any_element(),
                )
            }
            QueueLoadState::Ready => None,
        }
    }
}

impl Render for AlterePriorityQueuePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors().clone();
        let book_size = self.projection.as_ref().map_or_else(
            || "BOOK —".to_string(),
            |projection| format!("BOOK {}", projection.book_size),
        );
        let protected_percentile = self
            .projection
            .as_ref()
            .map(|projection| projection.protected_percentile);
        let row_count = self
            .projection
            .as_ref()
            .map_or(0, |projection| projection.rows.len());
        let load_state = self.render_load_state(cx);

        v_flex()
            .track_focus(&self.focus_handle)
            .key_context("AlterePriorityQueue")
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_first))
            .on_action(cx.listener(Self::select_last))
            .on_action(cx.listener(Self::scroll_up))
            .on_action(cx.listener(Self::scroll_down))
            .on_action(cx.listener(Self::open_selected_action))
            .on_action(cx.listener(Self::more_important))
            .on_action(cx.listener(Self::less_important))
            .size_full()
            .font_buffer(cx)
            .text_ui_sm(cx)
            .bg(colors.editor_background)
            .child(
                h_flex()
                    .w_full()
                    .h(px(28.))
                    .px_2()
                    .gap_3()
                    .items_center()
                    .border_b_1()
                    .border_color(colors.border)
                    .bg(colors.panel_background)
                    .text_color(colors.text_muted)
                    .child(book_size)
                    .when_some(protected_percentile, |element, percentile| {
                        element.child(format!("FLOOR TOP {percentile}%"))
                    }),
            )
            .child(
                h_flex()
                    .w_full()
                    .h(px(24.))
                    .px_2()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(colors.border)
                    .bg(colors.panel_background)
                    .text_color(colors.text_muted)
                    .child(div().w(px(34.)).child("RANK"))
                    .child(div().w(px(42.)).child("%ILE"))
                    .child(div().w(px(86.)).child("CLAIM"))
                    .child(div().w(px(66.)).child("LANE"))
                    .child("ELEMENT"),
            )
            .when_some(load_state, |element, load_state| element.child(load_state))
            .child(
                div()
                    .id("altere-priority-queue-rows")
                    .role(Role::List)
                    .aria_label("Priority queue")
                    .min_h_0()
                    .flex_1()
                    .when(
                        row_count == 0 && matches!(self.load_state, QueueLoadState::Ready),
                        |element| {
                            element.child(
                                h_flex()
                                    .size_full()
                                    .justify_center()
                                    .text_color(colors.text_muted)
                                    .child("Nothing in rotation."),
                            )
                        },
                    )
                    .when(row_count > 0, |element| {
                        element.child(
                            list(
                                self.list_state.clone(),
                                cx.processor(|this, index, window, cx| {
                                    this.render_row(index, window, cx)
                                }),
                            )
                            .size_full(),
                        )
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_projection_load_becomes_retryable_panel_state() {
        let state = QueueLoadState::error(anyhow::anyhow!("runtime offline"));

        assert!(matches!(
            state,
            QueueLoadState::Error(message) if message.contains("runtime offline")
        ));
    }

    #[test]
    fn replacement_projection_clamps_selection_without_owning_queue_order() {
        assert_eq!(clamp_selected_index(12, 3), 2);
        assert_eq!(clamp_selected_index(12, 0), 0);
        assert_eq!(clamp_selected_index(1, 3), 1);
    }

    #[test]
    fn replacement_projection_preserves_selection_by_stable_row_identity() {
        let rows = vec![
            QueueProjectionRow {
                id: "two.md".into(),
                path: "/tmp/collection/two.md".into(),
                fingerprint: "two".into(),
                name: "two.md".into(),
                rank: 1,
                percentile: 0.,
                claim_cells: 10,
                lane: "reading".into(),
                priority: 20.,
                reads: 0,
                protected: true,
                rule: "rank".into(),
            },
            QueueProjectionRow {
                id: "one.md".into(),
                path: "/tmp/collection/one.md".into(),
                fingerprint: "one".into(),
                name: "one.md".into(),
                rank: 2,
                percentile: 100.,
                claim_cells: 1,
                lane: "reading".into(),
                priority: 42.,
                reads: 0,
                protected: false,
                rule: "rank".into(),
            },
        ];

        assert_eq!(selected_index_after_refresh(Some("one.md"), 0, &rows), 1);
        assert_eq!(
            selected_index_after_refresh(Some("missing.md"), 8, &rows),
            1
        );
    }

    #[test]
    fn queue_shortcuts_derive_exact_bounded_priorities() {
        assert_eq!(stepped_priority(50., -5), 45);
        assert_eq!(stepped_priority(3., -5), 1);
        assert_eq!(stepped_priority(98., 5), 100);
    }

    #[test]
    fn vim_navigation_clamps_at_queue_edges() {
        assert_eq!(move_selection(0, 3, -1), 0);
        assert_eq!(move_selection(0, 3, 1), 1);
        assert_eq!(move_selection(2, 3, 1), 2);
        assert_eq!(move_selection(0, 0, 1), 0);
    }

    #[test]
    fn half_page_navigation_uses_the_visible_row_count() {
        assert_eq!(half_page_rows(px(280.)), 5);
        assert_eq!(half_page_rows(px(27.)), 1);
        assert_eq!(half_page_rows(px(0.)), 1);
    }

    #[test]
    fn panel_position_storage_round_trips_all_supported_docks() {
        for position in [
            DockPosition::Left,
            DockPosition::Bottom,
            DockPosition::Right,
        ] {
            assert_eq!(
                position_from_storage(&position_for_storage(position)),
                Some(position)
            );
        }
        assert_eq!(position_from_storage("unsupported"), None);
    }

    #[test]
    fn zed_requests_a_projection_without_owning_queue_policy() {
        assert_eq!(
            queue_invocation(Path::new("/opt/altere/ir.ts"), Path::new("/tmp/collection")),
            [
                "/opt/altere/ir.ts",
                "rotation",
                "queue",
                "--collection",
                "/tmp/collection",
            ]
        );
    }

    #[test]
    fn zed_requests_one_atomic_priority_set() {
        assert_eq!(
            priority_invocation(
                Path::new("/opt/altere/ir.ts"),
                Path::new("/tmp/collection"),
                "one.md",
                45,
                "fingerprint-1",
            ),
            [
                "/opt/altere/ir.ts",
                "priority",
                "set",
                "--collection",
                "/tmp/collection",
                "--source",
                "one.md",
                "--value",
                "45",
                "--expected-fingerprint",
                "fingerprint-1",
            ]
        );
    }

    #[test]
    fn queue_projection_is_a_renderer_contract() {
        let projection: QueueProjection = serde_json::from_str(
            r#"{
                "version": 1,
                "bookSize": 1,
                "protectedPercentile": 10,
                "rows": [{
                    "id": "problem.md",
                    "path": "/tmp/collection/problem.md",
                    "fingerprint": "abc123",
                    "name": "problem.md",
                    "rank": 1,
                    "percentile": 0,
                    "claimCells": 10,
                    "lane": "problem",
                    "priority": 99,
                    "reads": 0,
                    "protected": true,
                    "rule": "rank"
                }]
            }"#,
        )
        .expect("valid projection fixture");

        assert_eq!(projection.book_size, 1);
        assert_eq!(projection.rows[0].claim_cells, 10);
        assert_eq!(
            projection.rows[0].path,
            Path::new("/tmp/collection/problem.md")
        );
    }
}
