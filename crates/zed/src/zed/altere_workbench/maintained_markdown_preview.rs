use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, Global, Subscription, Window, actions,
};
use markdown_preview::markdown_preview_view::MarkdownPreviewView;
use ui::{Color, IconButton, IconName, IconSize, Tooltip, prelude::*};
use workspace::{ItemHandle, ToolbarItemEvent, ToolbarItemLocation, ToolbarItemView, Workspace};

actions!(
    altere,
    [
        /// Toggles whether Altere keeps a following Markdown preview open.
        ToggleMarkdownPreviewTracking
    ]
);

struct PreviewTrackingPolicy {
    enabled: bool,
}

impl Default for PreviewTrackingPolicy {
    fn default() -> Self {
        Self { enabled: true }
    }
}

impl PreviewTrackingPolicy {
    fn should_ensure_preview(&self) -> bool {
        self.enabled
    }

    fn toggle(&mut self) -> bool {
        self.enabled = !self.enabled;
        self.enabled
    }
}

#[derive(Default)]
struct PreviewTrackingState {
    policy: PreviewTrackingPolicy,
}

impl PreviewTrackingState {
    fn toggle(&mut self, cx: &mut Context<Self>) -> bool {
        let enabled = self.policy.toggle();
        cx.notify();
        enabled
    }

    fn enabled(&self) -> bool {
        self.policy.should_ensure_preview()
    }
}

struct PreviewTrackingGlobal(Entity<PreviewTrackingState>);

impl Global for PreviewTrackingGlobal {}

fn preview_tracking(cx: &mut App) -> Entity<PreviewTrackingState> {
    cx.try_global::<PreviewTrackingGlobal>()
        .map(|global| global.0.clone())
        .unwrap_or_else(|| {
            let tracking = cx.new(|_| PreviewTrackingState::default());
            cx.set_global(PreviewTrackingGlobal(tracking.clone()));
            tracking
        })
}

fn ensure_preview(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> bool {
    if MarkdownPreviewView::resolve_active_item_as_markdown_editor(workspace, cx).is_none() {
        return false;
    }
    MarkdownPreviewView::open_following_preview_to_the_side(workspace, window, cx);
    true
}

fn defer_preview_ensure(
    tracking: Entity<PreviewTrackingState>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    cx.defer_in(window, move |workspace, window, cx| {
        if tracking.read(cx).enabled() {
            ensure_preview(workspace, window, cx);
        }
    });
}

fn toggle_tracking(
    workspace: &mut Workspace,
    tracking: &Entity<PreviewTrackingState>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> bool {
    let enabled = tracking.update(cx, |tracking, cx| tracking.toggle(cx));
    if enabled {
        ensure_preview(workspace, window, cx);
    }
    enabled
}

pub(crate) fn register(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let tracking = preview_tracking(cx);
    if tracking.read(cx).enabled() {
        ensure_preview(workspace, window, cx);
    }

    let action_tracking = tracking.clone();
    workspace.register_action(
        move |workspace, _: &ToggleMarkdownPreviewTracking, window, cx| {
            toggle_tracking(workspace, &action_tracking, window, cx);
        },
    );

    let workspace_handle = cx.entity();
    cx.subscribe_in(
        &workspace_handle,
        window,
        move |_workspace, _, event, window, cx| {
            if matches!(
                event,
                workspace::Event::ActiveItemChanged
                    | workspace::Event::ItemRemoved { .. }
                    | workspace::Event::PaneRemoved
            ) {
                defer_preview_ensure(tracking.clone(), window, cx);
            }
        },
    )
    .detach();
}

pub(crate) struct AltereMarkdownPreviewToolbar {
    preview_active: bool,
    tracking: Entity<PreviewTrackingState>,
    _tracking_subscription: Subscription,
}

impl AltereMarkdownPreviewToolbar {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let tracking = preview_tracking(cx);
        let tracking_subscription = cx.observe(&tracking, |_, _, cx| cx.notify());
        Self {
            preview_active: false,
            tracking,
            _tracking_subscription: tracking_subscription,
        }
    }
}

impl EventEmitter<ToolbarItemEvent> for AltereMarkdownPreviewToolbar {}

impl ToolbarItemView for AltereMarkdownPreviewToolbar {
    fn set_active_pane_item(
        &mut self,
        active_pane_item: Option<&dyn ItemHandle>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ToolbarItemLocation {
        self.preview_active = active_pane_item
            .and_then(|item| item.downcast::<MarkdownPreviewView>())
            .is_some();
        cx.notify();
        if self.preview_active {
            ToolbarItemLocation::PrimaryRight
        } else {
            ToolbarItemLocation::Hidden
        }
    }
}

impl gpui::Render for AltereMarkdownPreviewToolbar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        let tracking = self.tracking.read(cx).enabled();
        let tooltip_label = if tracking {
            "Stop Tracking Markdown Preview"
        } else {
            "Track Markdown Preview"
        };

        IconButton::new("altere-markdown-preview-tracking", IconName::Crosshair)
            .icon_size(IconSize::Small)
            .icon_color(Color::Muted)
            .toggle_state(tracking)
            .selected_icon_color(Some(Color::Custom(cx.theme().players().agent().cursor)))
            .tooltip(move |_window, cx| {
                Tooltip::for_action(tooltip_label, &ToggleMarkdownPreviewTracking, cx)
            })
            .on_click(|_, window, cx| {
                window.dispatch_action(Box::new(ToggleMarkdownPreviewTracking), cx)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zed::tests::init_test;
    use gpui::TestAppContext;
    use serde_json::json;
    use std::path::PathBuf;
    use util::path;
    use util::rel_path::rel_path;
    use workspace::{MultiWorkspace, OpenOptions, open_paths};

    #[test]
    fn tracking_repeatedly_ensures_preview_until_explicitly_disabled() {
        let policy = PreviewTrackingPolicy::default();

        assert!(policy.should_ensure_preview());
        assert!(
            policy.should_ensure_preview(),
            "closing and reopening does not disable tracking"
        );
    }

    #[test]
    fn explicit_disable_prevents_preview_ensure() {
        let mut policy = PreviewTrackingPolicy::default();

        policy.toggle();

        assert!(!policy.should_ensure_preview());
    }

    #[test]
    fn explicit_enable_rearms_preview_ensure() {
        let mut policy = PreviewTrackingPolicy::default();
        policy.toggle();

        policy.toggle();

        assert!(policy.should_ensure_preview());
    }

    #[gpui::test]
    async fn closing_preview_reopens_until_tracking_is_disabled(cx: &mut TestAppContext) {
        let app_state = init_test(cx);
        app_state.languages.add(language::markdown_lang());
        app_state
            .fs
            .as_fake()
            .insert_tree(path!("/root"), json!({ "note.md": "# Note\n" }))
            .await;
        cx.update(|cx| {
            open_paths(
                &[PathBuf::from(path!("/root"))],
                app_state.clone(),
                OpenOptions::default(),
                cx,
            )
        })
        .await
        .expect("workspace should open");
        let multi_workspace = cx.update(|cx| {
            cx.windows()[0]
                .downcast::<MultiWorkspace>()
                .expect("workspace window should exist")
        });
        multi_workspace
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace
                    .workspace()
                    .update(cx, |workspace, cx| register(workspace, window, cx));
            })
            .expect("preview tracking should register");
        let worktree_id = multi_workspace
            .update(cx, |multi_workspace, _, cx| {
                multi_workspace
                    .workspace()
                    .read(cx)
                    .project()
                    .read(cx)
                    .worktrees(cx)
                    .next()
                    .expect("test project should expose a worktree")
                    .read(cx)
                    .id()
            })
            .expect("worktree should resolve");
        multi_workspace
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    workspace.open_path((worktree_id, rel_path("note.md")), None, true, window, cx)
                })
            })
            .expect("Markdown open task should start")
            .await
            .expect("Markdown should open");
        cx.run_until_parked();

        let first_preview = multi_workspace
            .update(cx, |multi_workspace, _, cx| {
                multi_workspace
                    .workspace()
                    .read(cx)
                    .items_of_type::<MarkdownPreviewView>(cx)
                    .next()
                    .expect("tracking should open a preview")
            })
            .expect("preview should resolve");
        multi_workspace
            .update(cx, |_, window, cx| {
                let toolbar = cx.new(AltereMarkdownPreviewToolbar::new);
                toolbar.update(cx, |toolbar, cx| {
                    assert_eq!(
                        toolbar.set_active_pane_item(Some(&first_preview), window, cx),
                        ToolbarItemLocation::PrimaryRight
                    );
                    assert_eq!(
                        toolbar.set_active_pane_item(None, window, cx),
                        ToolbarItemLocation::Hidden
                    );
                });
            })
            .expect("toolbar visibility should resolve");
        multi_workspace
            .update(cx, |multi_workspace, window, cx| {
                let workspace = multi_workspace.workspace().read(cx);
                let pane = workspace
                    .pane_for_item_id(first_preview.entity_id())
                    .expect("preview should belong to a pane");
                pane.update(cx, |pane, cx| {
                    pane.remove_item(first_preview.entity_id(), false, false, window, cx)
                });
            })
            .expect("preview should close");
        cx.run_until_parked();

        let replacement_preview = multi_workspace
            .update(cx, |multi_workspace, _, cx| {
                let workspace = multi_workspace.workspace().read(cx);
                let previews = workspace
                    .items_of_type::<MarkdownPreviewView>(cx)
                    .collect::<Vec<_>>();
                assert_eq!(
                    previews.len(),
                    1,
                    "tracking should restore exactly one preview"
                );
                previews[0].clone()
            })
            .expect("replacement preview should resolve");
        assert_ne!(first_preview.entity_id(), replacement_preview.entity_id());

        let tracking = cx.update(preview_tracking);
        multi_workspace
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    assert!(!toggle_tracking(workspace, &tracking, window, cx));
                    let pane = workspace
                        .pane_for_item_id(replacement_preview.entity_id())
                        .expect("replacement preview should belong to a pane");
                    pane.update(cx, |pane, cx| {
                        pane.remove_item(replacement_preview.entity_id(), false, false, window, cx)
                    });
                });
            })
            .expect("tracking should disable and preview should close");
        cx.run_until_parked();
        assert_eq!(
            multi_workspace
                .update(cx, |multi_workspace, _, cx| multi_workspace
                    .workspace()
                    .read(cx)
                    .items_of_type::<MarkdownPreviewView>(cx)
                    .count())
                .expect("preview count should resolve"),
            0,
            "explicitly disabled tracking must leave a closed preview closed"
        );

        multi_workspace
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.workspace().update(cx, |workspace, cx| {
                    assert!(toggle_tracking(workspace, &tracking, window, cx));
                });
            })
            .expect("tracking should re-enable");
        cx.run_until_parked();
        assert_eq!(
            multi_workspace
                .update(cx, |multi_workspace, _, cx| multi_workspace
                    .workspace()
                    .read(cx)
                    .items_of_type::<MarkdownPreviewView>(cx)
                    .count())
                .expect("preview count should resolve"),
            1,
            "re-enabling tracking should reopen the preview"
        );
    }
}
