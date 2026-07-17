use editor::{Editor, MultiBuffer, actions::DiffClipboardWithSelectionData};
use git_ui::text_diff_view::TextDiffView;
use gpui::{
    App, AppContext as _, Context, Entity, EntityId, EventEmitter, Global, Task, Window, actions,
};
use language::Buffer;
use serde::Deserialize;
use std::{collections::HashMap, path::PathBuf};
use ui::{Button, ButtonStyle, TintColor, prelude::*};
use workspace::{
    ItemHandle, ToolbarItemEvent, ToolbarItemLocation, ToolbarItemView, Workspace,
    notifications::DetachAndPromptErr,
};

actions!(
    altere,
    [
        /// Keeps the active Altere proposal after its native review.
        KeepProposal,
        /// Rejects the active Altere proposal without changing its source.
        RejectProposal
    ]
);

pub(crate) fn is_proposal_review(active_pane_item: Option<&dyn ItemHandle>, cx: &App) -> bool {
    active_pane_item
        .and_then(|item| item.downcast::<TextDiffView>())
        .is_some_and(|review| {
            cx.try_global::<ProposalReviewRegistry>()
                .is_some_and(|registry| registry.0.contains_key(&review.entity_id()))
        })
}

#[derive(Clone)]
struct ProposalReviewReference {
    proposal_id: String,
    source_path: PathBuf,
}

#[derive(Default)]
struct ProposalReviewRegistry(HashMap<EntityId, ProposalReviewReference>);

impl Global for ProposalReviewRegistry {}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DetachedProposalProjection {
    pub proposal_id: String,
    pub source: DetachedProposalSource,
    pub hunk: DetachedProposalHunk,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct DetachedProposalSource {
    pub path: PathBuf,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DetachedProposalHunk {
    pub original_text: String,
    pub proposed_text: String,
}

pub(crate) fn open_detached_proposal_review(
    projection: DetachedProposalProjection,
    workspace: &Workspace,
    window: &mut Window,
    cx: &mut App,
) -> Option<Task<anyhow::Result<Entity<TextDiffView>>>> {
    let reference = ProposalReviewReference {
        proposal_id: projection.proposal_id.clone(),
        source_path: projection.source.path.clone(),
    };
    let title = format!(
        "Proposal {} · {}",
        projection.proposal_id,
        projection.source.path.display()
    );
    let proposed_buffer = cx.new(|cx| Buffer::local(projection.hunk.proposed_text, cx));
    let proposed_editor = cx.new(|cx| {
        let proposed_buffer =
            cx.new(|cx| MultiBuffer::singleton(proposed_buffer, cx).with_title(title));
        Editor::for_multibuffer(proposed_buffer, None, window, cx)
    });

    let open = TextDiffView::open(
        &DiffClipboardWithSelectionData {
            clipboard_text: projection.hunk.original_text,
            editor: proposed_editor,
        },
        workspace,
        window,
        cx,
    )?;
    let workspace = workspace.weak_handle();

    Some(window.spawn(cx, async move |cx| {
        let review = open.await?;
        workspace.update_in(cx, |workspace, _window, cx| {
            cx.default_global::<ProposalReviewRegistry>()
                .0
                .insert(review.entity_id(), reference.clone());

            if let Some(next_action) = workspace
                .status_bar()
                .read(cx)
                .item_of_type::<super::AltereNextButton>()
            {
                next_action.update(cx, |action, cx| action.set_reviewing_proposal(true, cx));
            }

            let is_active = workspace
                .active_item(cx)
                .and_then(|item| item.downcast::<TextDiffView>())
                .is_some_and(|active_review| active_review.entity_id() == review.entity_id());
            if is_active {
                let toolbar = workspace.active_pane().read(cx).toolbar().clone();
                if let Some(proposal_toolbar) = toolbar
                    .read(cx)
                    .item_of_type::<AltereProposalReviewToolbar>()
                {
                    proposal_toolbar.update(cx, |toolbar, cx| {
                        toolbar.active = Some(reference);
                        cx.emit(ToolbarItemEvent::ChangeLocation(
                            ToolbarItemLocation::PrimaryRight,
                        ));
                        cx.notify();
                    });
                }
            }
        })?;
        Ok(review)
    }))
}

fn proposal_resolution_invocation(
    runtime: &std::path::Path,
    collection: &std::path::Path,
    proposal_id: &str,
    outcome: &str,
) -> [String; 7] {
    [
        runtime.to_string_lossy().into_owned(),
        "proposal".into(),
        outcome.into(),
        "--collection".into(),
        collection.to_string_lossy().into_owned(),
        "--proposal-id".into(),
        proposal_id.into(),
    ]
}

fn resolve_active_proposal(
    outcome: &'static str,
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(review) = workspace
        .active_item(cx)
        .and_then(|item| item.downcast::<TextDiffView>())
    else {
        return;
    };
    let Some(reference) = cx
        .try_global::<ProposalReviewRegistry>()
        .and_then(|registry| registry.0.get(&review.entity_id()).cloned())
    else {
        return;
    };
    let review_id = review.entity_id();

    cx.spawn_in(window, async move |workspace, cx| {
        let (bun, runtime, collection) = super::runtime_configuration()?;
        let output = smol::process::Command::new(&bun)
            .args(proposal_resolution_invocation(
                &runtime,
                &collection,
                &reference.proposal_id,
                outcome,
            ))
            .output()
            .await?;
        anyhow::ensure!(
            output.status.success(),
            "Altere could not {outcome} proposal {}: {}",
            reference.proposal_id,
            String::from_utf8_lossy(&output.stderr).trim()
        );

        workspace.update_in(cx, |workspace, window, cx| {
            cx.default_global::<ProposalReviewRegistry>()
                .0
                .remove(&review_id);
            if let Some(next_action) = workspace
                .status_bar()
                .read(cx)
                .item_of_type::<super::AltereNextButton>()
            {
                next_action.update(cx, |action, cx| action.set_reviewing_proposal(false, cx));
            }
            if let Some(pane) = workspace.pane_for_item_id(review_id) {
                pane.update(cx, |pane, cx| {
                    pane.remove_item(review_id, false, false, window, cx)
                });
            }
        })?;

        let source = collection.join(&reference.source_path);
        let open_source = workspace.update_in(cx, |workspace, window, cx| {
            workspace.open_abs_path(source, Default::default(), window, cx)
        })?;
        open_source.await?;
        anyhow::Ok(())
    })
    .detach_and_prompt_err(
        "Altere could not resolve the proposal",
        window,
        cx,
        |_, _, _| None,
    );
}

pub(super) fn register_actions(workspace: &mut Workspace) {
    workspace.register_action(|workspace, _: &KeepProposal, window, cx| {
        resolve_active_proposal("keep", workspace, window, cx)
    });
    workspace.register_action(|workspace, _: &RejectProposal, window, cx| {
        resolve_active_proposal("reject", workspace, window, cx)
    });
}

pub(crate) struct AltereProposalReviewToolbar {
    active: Option<ProposalReviewReference>,
}

impl AltereProposalReviewToolbar {
    pub(crate) fn new(_: &mut Context<Self>) -> Self {
        Self { active: None }
    }
}

impl EventEmitter<ToolbarItemEvent> for AltereProposalReviewToolbar {}

impl ToolbarItemView for AltereProposalReviewToolbar {
    fn set_active_pane_item(
        &mut self,
        active_pane_item: Option<&dyn ItemHandle>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ToolbarItemLocation {
        self.active = active_pane_item
            .and_then(|item| item.downcast::<TextDiffView>())
            .and_then(|review| {
                cx.try_global::<ProposalReviewRegistry>()
                    .and_then(|registry| registry.0.get(&review.entity_id()).cloned())
            });
        cx.notify();
        if self.active.is_some() {
            ToolbarItemLocation::PrimaryRight
        } else {
            ToolbarItemLocation::Hidden
        }
    }
}

impl gpui::Render for AltereProposalReviewToolbar {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl gpui::IntoElement {
        h_flex()
            .gap_1()
            .child(
                Button::new("altere-reject-proposal", "Reject")
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(RejectProposal), cx)),
            )
            .child(
                Button::new("altere-keep-proposal", "Keep")
                    .style(ButtonStyle::Tinted(TintColor::Accent))
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(KeepProposal), cx)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zed::tests::init_test;
    use editor::{Editor, SplittableEditor};
    use gpui::TestAppContext;
    use project::Project;
    use serde_json::json;
    use util::path;
    use workspace::MultiWorkspace;

    #[test]
    fn proposal_resolution_remains_an_atomic_bun_command() {
        assert_eq!(
            proposal_resolution_invocation(
                std::path::Path::new("/runtime/ir.ts"),
                std::path::Path::new("/collection"),
                "proposal-1",
                "keep",
            ),
            [
                "/runtime/ir.ts",
                "proposal",
                "keep",
                "--collection",
                "/collection",
                "--proposal-id",
                "proposal-1",
            ]
        );
    }

    #[gpui::test]
    async fn detached_native_review_does_not_touch_the_canonical_source(cx: &mut TestAppContext) {
        let app_state = init_test(cx);
        let canonical_text = "# Durable note\n\nThe source stays canonical.\n";
        let proposed_text = "# Durable note\n\nThe proposal changes this paragraph.\n";
        app_state
            .fs
            .as_fake()
            .insert_tree(path!("/root"), json!({ "note.md": canonical_text }))
            .await;

        let project = Project::test(app_state.fs.clone(), [path!("/root").as_ref()], cx).await;
        let canonical_buffer = project
            .update(cx, |project, cx| {
                project.open_local_buffer(path!("/root/note.md"), cx)
            })
            .await
            .expect("canonical buffer should open");
        let canonical_before = canonical_buffer.read_with(cx, |buffer, _| buffer.text());

        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project, window, cx));
        let workspace = multi_workspace.read_with(cx, |workspace, _| workspace.workspace().clone());
        let (proposal_toolbar, next_action) = workspace.update_in(cx, |workspace, window, cx| {
            let proposal_toolbar = cx.new(AltereProposalReviewToolbar::new);
            let toolbar = workspace.active_pane().read(cx).toolbar().clone();
            toolbar.update(cx, |toolbar, cx| {
                toolbar.add_item(proposal_toolbar.clone(), window, cx)
            });
            let next_action = super::super::next_button(cx);
            workspace.status_bar().update(cx, |status_bar, cx| {
                status_bar.add_right_item(next_action.clone(), window, cx)
            });
            (proposal_toolbar, next_action)
        });

        let review = workspace
            .update_in(cx, |workspace, window, cx| {
                open_detached_proposal_review(
                    serde_json::from_value(json!({
                        "version": 1,
                        "proposalId": "proposal-1",
                        "status": "pending",
                        "source": {
                            "path": "note.md",
                            "fingerprint": "owned-by-the-runtime"
                        },
                        "hunk": {
                            "kind": "replace-document",
                            "originalText": canonical_text,
                            "proposedText": proposed_text
                        },
                        "outcome": null
                    }))
                    .expect("runtime projection should deserialize"),
                    workspace,
                    window,
                    cx,
                )
            })
            .expect("review should be constructible")
            .await
            .expect("review should open");
        cx.executor().run_until_parked();

        let (review_editor, original_editor) = workspace.read_with(cx, |workspace, cx| {
            let active_item = workspace.active_item(cx).expect("review should be active");
            let review_editor = active_item
                .act_as::<Editor>(cx)
                .expect("native review should expose an editor");
            let split_editor = active_item
                .act_as::<SplittableEditor>(cx)
                .expect("native review should expose Zed's diff editor");
            let original_editor = split_editor
                .read(cx)
                .lhs_editor()
                .cloned()
                .expect("split review should expose the detached original");
            (review_editor, original_editor)
        });
        assert_eq!(
            review.entity_id(),
            workspace.read_with(cx, |workspace, cx| {
                workspace
                    .items_of_type::<git_ui::text_diff_view::TextDiffView>(cx)
                    .next()
                    .expect("native diff view should be mounted")
                    .entity_id()
            })
        );
        assert_eq!(
            review_editor.read_with(cx, |editor, cx| editor.text(cx)),
            proposed_text
        );
        assert_eq!(
            original_editor.read_with(cx, |editor, cx| editor.text(cx)),
            canonical_text
        );
        assert_eq!(
            canonical_buffer.read_with(cx, |buffer, _| buffer.text()),
            canonical_before
        );
        assert_eq!(
            app_state
                .fs
                .load(std::path::Path::new(path!("/root/note.md")))
                .await
                .expect("canonical file should remain readable"),
            canonical_text
        );
        assert_eq!(
            proposal_toolbar.read_with(cx, |toolbar, _| {
                toolbar
                    .active
                    .as_ref()
                    .map(|reference| reference.proposal_id.clone())
            }),
            Some("proposal-1".into())
        );
        assert!(next_action.read_with(cx, |action, _| action.is_reviewing_proposal()));
    }
}
