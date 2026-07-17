use gpui::{AnyElement, App, ClickEvent, ElementId, IntoElement, RenderOnce, SharedString, Window};
use ui::{ListItem, Tooltip, prelude::*};

#[derive(Clone)]
pub struct PriorityQueueRowData {
    rank: usize,
    percentile: f32,
    claim_cells: usize,
    lane: SharedString,
    name: SharedString,
    priority: f32,
    reads: usize,
}

impl PriorityQueueRowData {
    pub fn new(
        rank: usize,
        percentile: f32,
        claim_cells: usize,
        lane: impl Into<SharedString>,
        name: impl Into<SharedString>,
        priority: f32,
        reads: usize,
    ) -> Self {
        Self {
            rank,
            percentile,
            claim_cells,
            lane: lane.into(),
            name: name.into(),
            priority,
            reads,
        }
    }

    fn accessibility_label(&self) -> String {
        format!(
            "Rank {}, percentile {:.0}, {}, {}, priority {}, {} reads",
            self.rank, self.percentile, self.lane, self.name, self.priority, self.reads
        )
    }
}

type OnClick = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

#[derive(IntoElement, RegisterComponent)]
pub struct PriorityQueueRow {
    id: ElementId,
    data: PriorityQueueRowData,
    selected: bool,
    tooltip: Option<SharedString>,
    on_click: Option<OnClick>,
}

impl PriorityQueueRow {
    pub fn new(id: impl Into<ElementId>, data: PriorityQueueRowData) -> Self {
        Self {
            id: id.into(),
            data,
            selected: false,
            tooltip: None,
            on_click: None,
        }
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for PriorityQueueRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors().clone();
        let claim_cells = self.data.claim_cells.min(10);
        let claim = format!(
            "{}{}",
            "█".repeat(claim_cells),
            "░".repeat(10 - claim_cells)
        );

        ListItem::new(self.id)
            .height(px(28.))
            .toggle_state(self.selected)
            .aria_role(gpui::Role::ListItem)
            .aria_label(self.data.accessibility_label())
            .when(self.selected, |item| item.aria_active_descendant())
            .when_some(self.on_click, |item, on_click| item.on_click(on_click))
            .when_some(self.tooltip, |item, tooltip| {
                item.tooltip(Tooltip::text(tooltip))
            })
            .child(
                h_flex()
                    .min_w_0()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .w(px(34.))
                            .text_color(colors.text_muted)
                            .child(format!("{:02}", self.data.rank)),
                    )
                    .child(
                        div()
                            .w(px(42.))
                            .text_color(colors.text_accent)
                            .child(format!("{:.0}%", self.data.percentile)),
                    )
                    .child(div().w(px(86.)).text_color(colors.text_accent).child(claim))
                    .child(
                        div()
                            .w(px(66.))
                            .text_color(colors.text_muted)
                            .child(self.data.lane.to_uppercase()),
                    )
                    .child(
                        h_flex()
                            .min_w_0()
                            .flex_1()
                            .gap_2()
                            .overflow_hidden()
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .overflow_hidden()
                                    .child(self.data.name),
                            )
                            .child(
                                div().text_color(colors.text_muted).child(format!(
                                    "p{} · r{}",
                                    self.data.priority, self.data.reads
                                )),
                            ),
                    ),
            )
    }
}

impl Component for PriorityQueueRow {
    fn scope() -> ComponentScope {
        ComponentScope::Altere
    }

    fn description() -> &'static str {
        "A projected Altere priority queue row. It formats immutable display data and owns no queue rules."
    }

    fn preview(_window: &mut Window, cx: &mut App) -> AnyElement {
        let specimen = |id, width, name, selected| {
            div()
                .w(px(width))
                .border_1()
                .border_color(cx.theme().colors().border_variant)
                .child(
                    PriorityQueueRow::new(
                        id,
                        PriorityQueueRowData::new(3, 12., 7, "reading", name, 25., 4),
                    )
                    .selected(selected),
                )
                .into_any_element()
        };

        example_group_with_title(
            "Altere Priority Queue Row",
            vec![
                single_example(
                    "Representative",
                    specimen("altere-row-representative", 900., "example.md", false),
                ),
                single_example(
                    "Selected",
                    specimen("altere-row-selected", 900., "example.md", true),
                ),
                single_example(
                    "Narrow",
                    specimen(
                        "altere-row-narrow",
                        320.,
                        "a-very-long-reading-name-that-must-not-break-the-panel.md",
                        false,
                    ),
                ),
            ],
        )
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accessibility_label_is_derived_only_from_projected_display_data() {
        let row = PriorityQueueRowData::new(3, 12., 7, "reading", "example.md", 25., 4);

        assert_eq!(
            row.accessibility_label(),
            "Rank 3, percentile 12, reading, example.md, priority 25, 4 reads"
        );
    }

    #[test]
    fn priority_queue_row_is_registered_for_component_preview() {
        component::init();

        let components = component::components();
        let metadata = components
            .get(&PriorityQueueRow::id())
            .expect("Altere priority queue row should be registered");

        assert_eq!(metadata.scope(), ComponentScope::Altere);
    }
}
