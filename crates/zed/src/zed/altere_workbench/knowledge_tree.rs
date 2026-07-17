use anyhow::Context as _;
use db::kvp::KeyValueStore;
use gpui::{
    AnyElement, App, AppContext as _, AsyncWindowContext, Context, Empty, Entity, EventEmitter,
    FocusHandle, Focusable, IntoElement, KeyBinding, ListAlignment, ListState, Pixels, Point,
    Render, Role, Task, TaskExt, WeakEntity, Window, actions, div, list, px,
};
use menu::{SelectFirst, SelectLast, SelectNext, SelectPrevious};
use serde::Deserialize;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};
use ui::{
    Button, Icon, IconName, IconSize, Label, LabelSize, ListItem, ListItemSpacing, SpinnerLabel,
    Tooltip, prelude::*,
};
use util::ResultExt as _;
use workspace::{
    Workspace,
    dock::{DockPosition, Panel, PanelEvent},
    notifications::DetachAndPromptErr,
};

actions!(
    altere_tree,
    [ToggleFocus, OpenSelected, CollapseSelected, ExpandSelected]
);

const PANEL_POSITION_KEY: &str = "altere_knowledge_tree_panel_position";

#[derive(Clone, Deserialize)]
struct HierarchyProjection {
    version: u8,
    rows: Vec<HierarchyProjectionRow>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HierarchyProjectionRow {
    id: String,
    path: PathBuf,
    name: String,
    parent_id: Option<String>,
    depth: usize,
    has_children: bool,
}

fn hierarchy_invocation(runtime: &Path, collection: &Path) -> [String; 5] {
    [
        runtime.to_string_lossy().into_owned(),
        "hierarchy".into(),
        "tree".into(),
        "--collection".into(),
        collection.to_string_lossy().into_owned(),
    ]
}

fn set_parent_invocation(
    runtime: &Path,
    collection: &Path,
    source_id: &str,
    parent_id: Option<&str>,
) -> Vec<String> {
    let mut invocation = vec![
        runtime.to_string_lossy().into_owned(),
        "hierarchy".into(),
        "set-parent".into(),
        "--collection".into(),
        collection.to_string_lossy().into_owned(),
        "--source".into(),
        source_id.into(),
    ];
    if let Some(parent_id) = parent_id {
        invocation.extend(["--parent".into(), parent_id.into()]);
    }
    invocation
}

async fn load_projection() -> anyhow::Result<HierarchyProjection> {
    let (bun, runtime, collection) = super::runtime_configuration()?;
    let output = smol::process::Command::new(bun)
        .args(hierarchy_invocation(&runtime, &collection))
        .output()
        .await?;
    anyhow::ensure!(
        output.status.success(),
        "Altere hierarchy runtime failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let projection: HierarchyProjection = serde_json::from_slice(&output.stdout)
        .context("decoding the Altere hierarchy projection")?;
    anyhow::ensure!(
        projection.version == 1,
        "unsupported Altere hierarchy projection"
    );
    Ok(projection)
}

async fn set_parent(source_id: String, parent_id: Option<String>) -> anyhow::Result<()> {
    let (bun, runtime, collection) = super::runtime_configuration()?;
    let output = smol::process::Command::new(bun)
        .args(set_parent_invocation(
            &runtime,
            &collection,
            &source_id,
            parent_id.as_deref(),
        ))
        .output()
        .await?;
    anyhow::ensure!(
        output.status.success(),
        "Altere hierarchy mutation failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}

#[derive(Clone)]
struct DraggedHierarchyItem {
    id: String,
    name: String,
}

struct DraggedHierarchyItemView {
    name: String,
    click_offset: Point<Pixels>,
}

impl Render for DraggedHierarchyItemView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .pl(self.click_offset.x + px(12.))
            .pt(self.click_offset.y + px(12.))
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .py_1()
                    .px_2()
                    .rounded_lg()
                    .bg(cx.theme().colors().background)
                    .child(Icon::new(IconName::File).size(IconSize::XSmall))
                    .child(Label::new(self.name.clone()).single_line()),
            )
    }
}

fn visible_row_indices(
    rows: &[HierarchyProjectionRow],
    collapsed_ids: &HashSet<String>,
) -> Vec<usize> {
    let mut visible = Vec::with_capacity(rows.len());
    let mut hidden_below_depth = None;
    for (index, row) in rows.iter().enumerate() {
        if hidden_below_depth.is_some_and(|depth| row.depth > depth) {
            continue;
        }
        hidden_below_depth = None;
        visible.push(index);
        if collapsed_ids.contains(&row.id) {
            hidden_below_depth = Some(row.depth);
        }
    }
    visible
}

fn optimistic_reparent(
    rows: &[HierarchyProjectionRow],
    source_id: &str,
    parent_id: Option<&str>,
) -> Option<Vec<HierarchyProjectionRow>> {
    let source_index = rows.iter().position(|row| row.id == source_id)?;
    let source_depth = rows[source_index].depth;
    let subtree_end = rows[source_index + 1..]
        .iter()
        .position(|row| row.depth <= source_depth)
        .map_or(rows.len(), |offset| source_index + 1 + offset);

    if parent_id.is_some_and(|parent_id| {
        rows[source_index..subtree_end]
            .iter()
            .any(|row| row.id == parent_id)
    }) {
        return None;
    }

    let mut subtree = rows[source_index..subtree_end].to_vec();
    let mut reordered = rows[..source_index].to_vec();
    reordered.extend_from_slice(&rows[subtree_end..]);

    let (insertion_index, new_depth) = if let Some(parent_id) = parent_id {
        let parent_index = reordered.iter().position(|row| row.id == parent_id)?;
        let parent_depth = reordered[parent_index].depth;
        let insertion_index = reordered[parent_index + 1..]
            .iter()
            .position(|row| row.depth <= parent_depth)
            .map_or(reordered.len(), |offset| parent_index + 1 + offset);
        (insertion_index, parent_depth + 1)
    } else {
        (reordered.len(), 0)
    };

    let depth_delta = new_depth as isize - source_depth as isize;
    for row in &mut subtree {
        row.depth = row.depth.checked_add_signed(depth_delta)?;
    }
    subtree[0].parent_id = parent_id.map(str::to_string);
    reordered.splice(insertion_index..insertion_index, subtree);

    let parent_ids = reordered
        .iter()
        .filter_map(|row| row.parent_id.clone())
        .collect::<HashSet<_>>();
    for row in &mut reordered {
        row.has_children = parent_ids.contains(&row.id);
    }

    Some(reordered)
}

fn move_selection(selected_index: usize, row_count: usize, delta: isize) -> usize {
    if row_count == 0 {
        return 0;
    }
    selected_index
        .saturating_add_signed(delta)
        .min(row_count - 1)
}

fn selected_index_after_refresh(
    selected_id: Option<&str>,
    previous_index: usize,
    rows: &[HierarchyProjectionRow],
    visible_indices: &[usize],
) -> usize {
    selected_id
        .and_then(|selected_id| {
            visible_indices.iter().position(|row_index| {
                rows.get(*row_index)
                    .is_some_and(|row| row.id == selected_id)
            })
        })
        .unwrap_or_else(|| previous_index.min(visible_indices.len().saturating_sub(1)))
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

enum HierarchyLoadState {
    Loading,
    Ready,
    Error(String),
}

impl HierarchyLoadState {
    fn error(error: anyhow::Error) -> Self {
        Self::Error(error.to_string())
    }
}

pub(crate) struct AltereKnowledgeTreePanel {
    workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
    position: DockPosition,
    projection: Option<HierarchyProjection>,
    load_state: HierarchyLoadState,
    collapsed_ids: HashSet<String>,
    visible_indices: Vec<usize>,
    list_state: ListState,
    selected_index: usize,
    _refresh_task: Task<()>,
}

impl AltereKnowledgeTreePanel {
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
                    .unwrap_or(DockPosition::Left);
                let mut panel = Self {
                    workspace: panel_workspace,
                    focus_handle: cx.focus_handle(),
                    position,
                    projection: None,
                    load_state: HierarchyLoadState::Loading,
                    collapsed_ids: HashSet::new(),
                    visible_indices: Vec::new(),
                    list_state: ListState::new(0, ListAlignment::Top, px(32.)),
                    selected_index: 0,
                    _refresh_task: Task::ready(()),
                };
                panel.refresh(cx);
                panel
            })
        })
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.load_state = HierarchyLoadState::Loading;
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

    fn finish_refresh(
        &mut self,
        result: anyhow::Result<HierarchyProjection>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(projection) => {
                let selected_id = self.selected_row().map(|row| row.id.clone());
                let visible_indices = visible_row_indices(&projection.rows, &self.collapsed_ids);
                self.selected_index = selected_index_after_refresh(
                    selected_id.as_deref(),
                    self.selected_index,
                    &projection.rows,
                    &visible_indices,
                );
                self.list_state.reset(visible_indices.len());
                self.visible_indices = visible_indices;
                self.projection = Some(projection);
                self.load_state = HierarchyLoadState::Ready;
            }
            Err(error) => self.load_state = HierarchyLoadState::error(error),
        }
        cx.notify();
    }

    fn selected_row(&self) -> Option<&HierarchyProjectionRow> {
        let row_index = *self.visible_indices.get(self.selected_index)?;
        self.projection.as_ref()?.rows.get(row_index)
    }

    fn open_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.selected_row().map(|row| row.path.clone()) else {
            return;
        };
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
            "Altere could not open the hierarchy item",
            window,
            cx,
            |_, _, _| None,
        );
    }

    fn set_parent(
        &mut self,
        source_id: String,
        parent_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if parent_id.as_deref() == Some(source_id.as_str()) {
            return;
        }
        let Some(previous_projection) = self.projection.clone() else {
            return;
        };
        let Some(rows) =
            optimistic_reparent(&previous_projection.rows, &source_id, parent_id.as_deref())
        else {
            return;
        };
        self.projection = Some(HierarchyProjection {
            version: previous_projection.version,
            rows,
        });
        self.load_state = HierarchyLoadState::Ready;
        self.recompute_visible_rows_selecting(Some(&source_id), cx);

        cx.spawn_in(window, async move |this, cx| {
            let result = async {
                set_parent(source_id, parent_id).await?;
                load_projection().await
            }
            .await;
            match result {
                Ok(projection) => {
                    this.update(cx, |this, cx| this.finish_refresh(Ok(projection), cx))?;
                    anyhow::Ok(())
                }
                Err(error) => {
                    this.update(cx, |this, cx| {
                        this.finish_refresh(Ok(previous_projection), cx)
                    })?;
                    Err(error)
                }
            }
        })
        .detach_and_prompt_err(
            "Altere could not move the hierarchy item",
            window,
            cx,
            |_, _, _| None,
        );
    }

    fn select(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.visible_indices.len() {
            return;
        }
        self.selected_index = index;
        self.list_state.scroll_to_reveal_item(index);
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn move_selection_by(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let index = move_selection(self.selected_index, self.visible_indices.len(), delta);
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
        let Some(index) = self.visible_indices.len().checked_sub(1) else {
            return;
        };
        self.select(index, window, cx);
    }

    fn recompute_visible_rows(&mut self, cx: &mut Context<Self>) {
        let selected_id = self.selected_row().map(|row| row.id.clone());
        self.recompute_visible_rows_selecting(selected_id.as_deref(), cx);
    }

    fn recompute_visible_rows_selecting(
        &mut self,
        selected_id: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        let Some(projection) = self.projection.as_ref() else {
            return;
        };
        self.visible_indices = visible_row_indices(&projection.rows, &self.collapsed_ids);
        self.selected_index = selected_index_after_refresh(
            selected_id,
            self.selected_index,
            &projection.rows,
            &self.visible_indices,
        );
        self.list_state.reset(self.visible_indices.len());
        self.list_state.scroll_to_reveal_item(self.selected_index);
        cx.notify();
    }

    fn toggle_row(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.collapsed_ids.remove(id) {
            self.collapsed_ids.insert(id.to_string());
        }
        self.recompute_visible_rows(cx);
    }

    fn collapse_selected(&mut self, _: &CollapseSelected, _: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.selected_row() else {
            return;
        };
        if row.has_children && self.collapsed_ids.insert(row.id.clone()) {
            self.recompute_visible_rows(cx);
        }
    }

    fn expand_selected(&mut self, _: &ExpandSelected, _: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected_row().map(|row| row.id.clone()) else {
            return;
        };
        if self.collapsed_ids.remove(&id) {
            self.recompute_visible_rows(cx);
        }
    }

    fn open_selected_action(
        &mut self,
        _: &OpenSelected,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_selected(window, cx);
    }

    fn render_row(
        &mut self,
        visible_index: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(row_index) = self.visible_indices.get(visible_index).copied() else {
            return Empty.into_any();
        };
        let Some(row) = self
            .projection
            .as_ref()
            .and_then(|projection| projection.rows.get(row_index))
        else {
            return Empty.into_any();
        };
        let id = row.id.clone();
        let name = row.name.clone();
        let parent_id = row.parent_id.clone();
        let depth = row.depth;
        let has_children = row.has_children;
        let selected = visible_index == self.selected_index;
        let expanded = has_children && !self.collapsed_ids.contains(&id);
        let tooltip = row.path.display().to_string();
        let accessibility_label = format!(
            "{} at hierarchy level {}{}",
            name,
            depth + 1,
            parent_id
                .as_deref()
                .map(|parent| format!(", child of {parent}"))
                .unwrap_or_default()
        );
        let drop_target_id = id.clone();
        let toggle_id = id.clone();
        let dragged_item = DraggedHierarchyItem {
            id,
            name: name.clone(),
        };

        div()
            .id(("altere-knowledge-tree-drag-row", visible_index))
            .w_full()
            .can_drop({
                let drop_target_id = drop_target_id.clone();
                move |value, _, _| {
                    value
                        .downcast_ref::<DraggedHierarchyItem>()
                        .is_some_and(|dragged| dragged.id != drop_target_id)
                }
            })
            .drag_over::<DraggedHierarchyItem>(|style, _, _, cx| {
                style.bg(cx.theme().colors().drop_target_background)
            })
            .on_drag(dragged_item, |item, click_offset, _, cx| {
                cx.new(|_| DraggedHierarchyItemView {
                    name: item.name.clone(),
                    click_offset,
                })
            })
            .on_drop(
                cx.listener(move |this, dragged: &DraggedHierarchyItem, window, cx| {
                    cx.stop_propagation();
                    this.set_parent(dragged.id.clone(), Some(drop_target_id.clone()), window, cx);
                }),
            )
            .child(
                ListItem::new(("altere-knowledge-tree-row", visible_index))
                    .height(px(28.))
                    .spacing(ListItemSpacing::ExtraDense)
                    .indent_level(depth + 1)
                    .indent_step_size(px(16.))
                    .toggle(has_children.then_some(expanded))
                    .always_show_disclosure_icon(has_children)
                    .on_toggle(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.select(visible_index, window, cx);
                        this.toggle_row(&toggle_id, cx);
                    }))
                    .toggle_state(selected)
                    .aria_role(Role::TreeItem)
                    .aria_label(accessibility_label)
                    .when(selected, |item| item.aria_active_descendant())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select(visible_index, window, cx);
                        this.open_selected(window, cx);
                    }))
                    .tooltip(Tooltip::text(tooltip))
                    .when(!has_children, |item| {
                        item.start_slot(
                            Icon::new(IconName::File)
                                .size(IconSize::XSmall)
                                .color(Color::Muted),
                        )
                    })
                    .child(Label::new(name).single_line().truncate()),
            )
            .into_any_element()
    }

    fn render_load_state(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let colors = cx.theme().colors().clone();
        let error_color = cx.theme().status().error;
        match &self.load_state {
            HierarchyLoadState::Loading if self.projection.is_none() => Some(
                h_flex()
                    .w_full()
                    .py_4()
                    .gap_2()
                    .justify_center()
                    .text_color(colors.text_muted)
                    .child(SpinnerLabel::new())
                    .child("Loading knowledge tree…")
                    .into_any_element(),
            ),
            HierarchyLoadState::Loading => Some(
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
            HierarchyLoadState::Error(message) => {
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
                            Button::new("retry-altere-knowledge-tree", "Retry")
                                .label_size(LabelSize::Small)
                                .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                        )
                        .into_any_element(),
                )
            }
            HierarchyLoadState::Ready => None,
        }
    }
}

pub(super) fn key_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("j", SelectNext, Some("AltereKnowledgeTree")),
        KeyBinding::new("down", SelectNext, Some("AltereKnowledgeTree")),
        KeyBinding::new("k", SelectPrevious, Some("AltereKnowledgeTree")),
        KeyBinding::new("up", SelectPrevious, Some("AltereKnowledgeTree")),
        KeyBinding::new("g g", SelectFirst, Some("AltereKnowledgeTree")),
        KeyBinding::new("shift-g", SelectLast, Some("AltereKnowledgeTree")),
        KeyBinding::new("h", CollapseSelected, Some("AltereKnowledgeTree")),
        KeyBinding::new("left", CollapseSelected, Some("AltereKnowledgeTree")),
        KeyBinding::new("l", ExpandSelected, Some("AltereKnowledgeTree")),
        KeyBinding::new("right", ExpandSelected, Some("AltereKnowledgeTree")),
        KeyBinding::new("enter", OpenSelected, Some("AltereKnowledgeTree")),
    ]
}

pub(super) fn register_action(workspace: &mut Workspace) {
    workspace.register_action(|workspace, _: &ToggleFocus, window, cx| {
        workspace.toggle_panel_focus::<AltereKnowledgeTreePanel>(window, cx);
    });
}

impl Panel for AltereKnowledgeTreePanel {
    fn persistent_name() -> &'static str {
        "AltereKnowledgeTreePanel"
    }

    fn panel_key() -> &'static str {
        "AltereKnowledgeTreePanel"
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
        px(260.)
    }

    fn icon(&self, _: &Window, _: &App) -> Option<IconName> {
        Some(IconName::FileTree)
    }

    fn icon_tooltip(&self, _: &Window, _: &App) -> Option<&'static str> {
        Some("Knowledge Tree")
    }

    fn icon_label(&self, _: &Window, _: &App) -> Option<String> {
        self.projection
            .as_ref()
            .map(|projection| projection.rows.len().to_string())
    }

    fn toggle_action(&self) -> Box<dyn gpui::Action> {
        Box::new(ToggleFocus)
    }

    fn starts_open(&self, _: &Window, _: &App) -> bool {
        true
    }

    fn activation_priority(&self) -> u32 {
        4
    }
}

impl Focusable for AltereKnowledgeTreePanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<PanelEvent> for AltereKnowledgeTreePanel {}

impl Render for AltereKnowledgeTreePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors().clone();
        let node_count = self
            .projection
            .as_ref()
            .map_or(0, |projection| projection.rows.len());
        let visible_count = self.visible_indices.len();
        let load_state = self.render_load_state(cx);

        v_flex()
            .track_focus(&self.focus_handle)
            .key_context("AltereKnowledgeTree")
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_first))
            .on_action(cx.listener(Self::select_last))
            .on_action(cx.listener(Self::collapse_selected))
            .on_action(cx.listener(Self::expand_selected))
            .on_action(cx.listener(Self::open_selected_action))
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
                    .child("KNOWLEDGE TREE")
                    .child(div().flex_1())
                    .child(format!("{node_count}")),
            )
            .when_some(load_state, |element, load_state| element.child(load_state))
            .child(
                v_flex()
                    .id("altere-knowledge-tree-rows")
                    .role(Role::Tree)
                    .aria_label("Knowledge tree")
                    .min_h_0()
                    .flex_1()
                    .when(
                        node_count == 0 && matches!(self.load_state, HierarchyLoadState::Ready),
                        |element| {
                            element.child(
                                h_flex()
                                    .size_full()
                                    .justify_center()
                                    .text_color(colors.text_muted)
                                    .child("The knowledge tree is empty."),
                            )
                        },
                    )
                    .when(visible_count > 0, |element| {
                        element.child(
                            list(
                                self.list_state.clone(),
                                cx.processor(|this, index, window, cx| {
                                    this.render_row(index, window, cx)
                                }),
                            )
                            .w_full()
                            .min_h_0()
                            .flex_1(),
                        )
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        id: &str,
        parent_id: Option<&str>,
        depth: usize,
        has_children: bool,
    ) -> HierarchyProjectionRow {
        HierarchyProjectionRow {
            id: id.into(),
            path: PathBuf::from("/tmp/collection").join(id),
            name: id.into(),
            parent_id: parent_id.map(str::to_string),
            depth,
            has_children,
        }
    }

    #[test]
    fn zed_invokes_only_the_standalone_hierarchy_projection() {
        assert_eq!(
            hierarchy_invocation(Path::new("/opt/altere/ir.ts"), Path::new("/tmp/collection")),
            [
                "/opt/altere/ir.ts",
                "hierarchy",
                "tree",
                "--collection",
                "/tmp/collection"
            ]
        );
    }

    #[test]
    fn zed_sends_only_reparent_intent_to_the_standalone_runtime() {
        assert_eq!(
            set_parent_invocation(
                Path::new("/opt/altere/ir.ts"),
                Path::new("/tmp/collection"),
                "child.md",
                Some("parent.md"),
            ),
            vec![
                "/opt/altere/ir.ts",
                "hierarchy",
                "set-parent",
                "--collection",
                "/tmp/collection",
                "--source",
                "child.md",
                "--parent",
                "parent.md",
            ]
        );
        assert_eq!(
            set_parent_invocation(
                Path::new("/opt/altere/ir.ts"),
                Path::new("/tmp/collection"),
                "child.md",
                None,
            ),
            vec![
                "/opt/altere/ir.ts",
                "hierarchy",
                "set-parent",
                "--collection",
                "/tmp/collection",
                "--source",
                "child.md",
            ]
        );
    }

    #[test]
    fn collapse_filters_only_descendants_from_the_bun_order() {
        let rows = vec![
            row("root.md", None, 0, true),
            row("child.md", Some("root.md"), 1, true),
            row("grandchild.md", Some("child.md"), 2, false),
            row("sibling.md", Some("root.md"), 1, false),
            row("other.md", None, 0, false),
        ];

        assert_eq!(
            visible_row_indices(&rows, &HashSet::from(["child.md".into()])),
            vec![0, 1, 3, 4]
        );
        assert_eq!(
            visible_row_indices(&rows, &HashSet::from(["root.md".into()])),
            vec![0, 4]
        );
    }

    #[test]
    fn selection_survives_projection_refresh_by_stable_identity() {
        let rows = vec![
            row("root.md", None, 0, true),
            row("child.md", Some("root.md"), 1, false),
            row("other.md", None, 0, false),
        ];

        assert_eq!(
            selected_index_after_refresh(Some("child.md"), 0, &rows, &[0, 1, 2]),
            1
        );
        assert_eq!(
            selected_index_after_refresh(Some("child.md"), 1, &rows, &[0, 2]),
            1
        );
    }

    #[test]
    fn optimistic_reparent_moves_the_complete_subtree_under_its_new_parent() {
        let rows = vec![
            row("root.md", None, 0, true),
            row("child.md", Some("root.md"), 1, true),
            row("grandchild.md", Some("child.md"), 2, false),
            row("sibling.md", Some("root.md"), 1, false),
            row("other.md", None, 0, false),
        ];

        let moved = optimistic_reparent(&rows, "child.md", Some("other.md")).unwrap();

        assert_eq!(
            moved
                .iter()
                .map(|row| (row.id.as_str(), row.parent_id.as_deref(), row.depth))
                .collect::<Vec<_>>(),
            vec![
                ("root.md", None, 0),
                ("sibling.md", Some("root.md"), 1),
                ("other.md", None, 0),
                ("child.md", Some("other.md"), 1),
                ("grandchild.md", Some("child.md"), 2),
            ]
        );
        assert!(
            moved
                .iter()
                .find(|row| row.id == "other.md")
                .unwrap()
                .has_children
        );
        assert!(
            moved
                .iter()
                .find(|row| row.id == "child.md")
                .unwrap()
                .has_children
        );
    }

    #[test]
    fn optimistic_root_drop_promotes_the_subtree_without_flattening_it() {
        let rows = vec![
            row("root.md", None, 0, true),
            row("child.md", Some("root.md"), 1, true),
            row("grandchild.md", Some("child.md"), 2, false),
            row("other.md", None, 0, false),
        ];

        let moved = optimistic_reparent(&rows, "child.md", None).unwrap();
        let child = moved.iter().find(|row| row.id == "child.md").unwrap();
        let grandchild = moved.iter().find(|row| row.id == "grandchild.md").unwrap();

        assert_eq!((child.parent_id.as_deref(), child.depth), (None, 0));
        assert_eq!(
            (grandchild.parent_id.as_deref(), grandchild.depth),
            (Some("child.md"), 1)
        );
    }

    #[test]
    fn optimistic_reparent_refuses_self_and_descendant_targets() {
        let rows = vec![
            row("root.md", None, 0, true),
            row("child.md", Some("root.md"), 1, true),
            row("grandchild.md", Some("child.md"), 2, false),
        ];

        assert!(optimistic_reparent(&rows, "child.md", Some("child.md")).is_none());
        assert!(optimistic_reparent(&rows, "child.md", Some("grandchild.md")).is_none());
    }

    #[test]
    fn vim_navigation_clamps_at_visible_tree_edges() {
        assert_eq!(move_selection(0, 3, -1), 0);
        assert_eq!(move_selection(0, 3, 1), 1);
        assert_eq!(move_selection(2, 3, 1), 2);
        assert_eq!(move_selection(4, 0, -1), 0);
    }
}
