use gpui::{
    App, AppContext as _, Context, Entity, FocusHandle, Focusable, Window, actions, div, px,
};
use ui::{
    Button, ButtonSize, ButtonStyle, KeyBinding as KeyBindingHint, LabelSize, TintColor, prelude::*,
};

actions!(
    altere,
    [
        /// Asks the standalone Altere runtime for the next Markdown buffer.
        NextBuffer,
        /// Focuses the permanent rotation action.
        FocusNextRepetition
    ]
);

const NEXT_REPETITION_LABEL: &str = "NEXT REPETITION";
const NEXT_REPETITION_WIDTH: f32 = 240.;

pub(crate) struct AltereNextButton {
    pub(crate) focus_handle: FocusHandle,
    reviewing_proposal: bool,
}

impl AltereNextButton {
    pub(crate) fn set_reviewing_proposal(
        &mut self,
        reviewing_proposal: bool,
        cx: &mut Context<Self>,
    ) {
        if self.reviewing_proposal != reviewing_proposal {
            self.reviewing_proposal = reviewing_proposal;
            cx.notify();
        }
    }

    #[cfg(test)]
    pub(crate) fn is_reviewing_proposal(&self) -> bool {
        self.reviewing_proposal
    }
}

impl Focusable for AltereNextButton {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl gpui::Render for AltereNextButton {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        let focused = self.focus_handle.is_focused(window);

        div()
            .key_context(if self.reviewing_proposal {
                "AltereProposalReview"
            } else {
                "AltereNextRepetition"
            })
            .track_focus(&self.focus_handle)
            .when(self.reviewing_proposal, |this| {
                this.child(
                    h_flex()
                        .gap_1()
                        .child(
                            Button::new("altere-reject-proposal", "REJECT")
                                .size(ButtonSize::Large)
                                .label_size(LabelSize::Large)
                                .on_click(|_, window, cx| {
                                    window.dispatch_action(
                                        Box::new(super::proposal_review::RejectProposal),
                                        cx,
                                    )
                                }),
                        )
                        .child(
                            Button::new("altere-keep-proposal", "KEEP")
                                .track_focus(&self.focus_handle)
                                .style(ButtonStyle::Tinted(TintColor::Accent))
                                .selected_style(ButtonStyle::Filled)
                                .toggle_state(focused)
                                .size(ButtonSize::Large)
                                .width(px(128.))
                                .label_size(LabelSize::Large)
                                .on_click(|_, window, cx| {
                                    window.dispatch_action(
                                        Box::new(super::proposal_review::KeepProposal),
                                        cx,
                                    )
                                }),
                        ),
                )
            })
            .when(!self.reviewing_proposal, |this| {
                this.child(
                    Button::new("altere-next-buffer", NEXT_REPETITION_LABEL)
                        .track_focus(&self.focus_handle)
                        .style(ButtonStyle::Tinted(TintColor::Accent))
                        .selected_style(ButtonStyle::Filled)
                        .toggle_state(focused)
                        .size(ButtonSize::Large)
                        .width(px(NEXT_REPETITION_WIDTH))
                        .label_size(LabelSize::Large)
                        .key_binding(KeyBindingHint::for_action_in(
                            &NextBuffer,
                            &self.focus_handle,
                            cx,
                        ))
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(NextBuffer), cx)),
                )
            })
    }
}

impl workspace::StatusItemView for AltereNextButton {
    fn set_active_pane_item(
        &mut self,
        active_pane_item: Option<&dyn workspace::ItemHandle>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_reviewing_proposal(
            super::proposal_review::is_proposal_review(active_pane_item, cx),
            cx,
        );
    }

    fn hide_setting(&self, _cx: &App) -> Option<workspace::HideStatusItem> {
        None
    }
}

pub(crate) fn next_button(cx: &mut App) -> Entity<AltereNextButton> {
    cx.new(|cx| AltereNextButton {
        focus_handle: cx.focus_handle(),
        reviewing_proposal: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_repetition_is_the_primary_visible_action() {
        assert_eq!(NEXT_REPETITION_LABEL, "NEXT REPETITION");
        assert!(NEXT_REPETITION_WIDTH >= 240.);
    }
}
