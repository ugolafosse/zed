use std::path::PathBuf;

use editor::{
    Bias, Editor, MultiBufferOffset, SelectionEffects, ToOffset as _, scroll::Autoscroll,
};
use gpui::{App, Entity, Window};
use workspace::Workspace;

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "production workspace discovery is a later gate")
)]
pub fn install_workbench_lifecycle(
    workspace: Entity<Workspace>,
    collection_root: PathBuf,
    cx: &mut App,
) {
    let source_path = collection_root.join("source.md");
    cx.subscribe(&workspace, move |workspace, event, cx| {
        let workspace::Event::UserSavedItem { item, .. } = event else {
            return;
        };
        let Some(item) = item.upgrade() else {
            return;
        };
        let Some(project_path) = item.project_path(cx) else {
            return;
        };
        let Some(saved_path) = workspace
            .read(cx)
            .project()
            .read(cx)
            .absolute_path(&project_path, cx)
        else {
            log::error!("Workbench could not resolve the saved item's project path");
            return;
        };
        if saved_path != source_path {
            return;
        }
        let Some(editor) = item.act_as::<Editor>(cx) else {
            return;
        };

        let (markdown, absolute_offset) = {
            let editor = editor.read(cx);
            let snapshot = editor.buffer().read(cx).snapshot(cx);
            let absolute_offset = editor.newest_selection_head().to_offset(&snapshot).0;
            (editor.text(cx), absolute_offset)
        };
        let result =
            workbench::WorkbenchCollection::open(&collection_root).and_then(|mut collection| {
                collection
                    .save_reading_position(&markdown, absolute_offset)
                    .map(drop)
            });
        if let Err(error) = result {
            log::error!("Workbench failed to persist the saved reading position: {error:#}");
        }
    })
    .detach();
}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "production workspace discovery is a later gate")
)]
pub fn install_workbench_restore_lifecycle(
    workspace: Entity<Workspace>,
    collection_root: PathBuf,
    window: &mut Window,
    cx: &mut App,
) {
    let source_path = collection_root.join("source.md");
    workspace.update(cx, |_, cx| {
        cx.subscribe_in(
            &workspace,
            window,
            move |workspace, _, event, window, cx| {
                let workspace::Event::ItemAdded { item } = event else {
                    return;
                };
                let Some(project_path) = item.project_path(cx) else {
                    return;
                };
                let Some(opened_path) = workspace
                    .project()
                    .read(cx)
                    .absolute_path(&project_path, cx)
                else {
                    log::error!("Workbench could not resolve the opened item's project path");
                    return;
                };
                if opened_path != source_path {
                    return;
                }
                let Some(editor) = item.act_as::<Editor>(cx) else {
                    return;
                };
                let markdown = editor.read(cx).text(cx);
                let resolved = match workbench::WorkbenchCollection::open(&collection_root)
                    .and_then(|collection| collection.resolve_current_reading_position(&markdown))
                {
                    Ok(Some(resolved)) => resolved,
                    Ok(None) => return,
                    Err(error) => {
                        log::error!("Workbench failed to restore the reading position: {error:#}");
                        return;
                    }
                };

                editor.update(cx, |editor, cx| {
                    let snapshot = editor.buffer().read(cx).snapshot(cx);
                    let offset = snapshot
                        .clip_offset(MultiBufferOffset(resolved.absolute_offset()), Bias::Left);
                    let anchor = snapshot.anchor_before(offset);
                    editor.change_selections(
                        SelectionEffects::scroll(Autoscroll::center()),
                        window,
                        cx,
                        |selections| selections.select_anchor_ranges([anchor..anchor]),
                    );
                });
            },
        )
        .detach();
    });
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use editor::{Editor, MultiBufferOffset, ToOffset as _};
    use gpui::TestAppContext;
    use project::Project;
    use serde_json::json;
    use workspace::{MultiWorkspace, OpenOptions, OpenVisible, SaveIntent};

    struct TemporaryCollection(PathBuf);

    impl Drop for TemporaryCollection {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    #[gpui::test]
    async fn successful_user_save_persists_the_real_editor_cursor(cx: &mut TestAppContext) {
        let app_state = crate::zed::tests::init_test(cx);
        let collection_root =
            std::env::temp_dir().join(format!("zed-workbench-lifecycle-{}", uuid::Uuid::new_v4()));
        let _temporary_collection = TemporaryCollection(collection_root.clone());
        std::fs::create_dir(&collection_root).expect("temporary Collection should be created");
        let source = collection_root.join("source.md");
        let target = "A distinctive paragraph owns the lifecycle cursor.";
        let markdown = format!("# Source\n\nOpening context.\n\n{target}\n\nEnding context.");
        std::fs::write(&source, &markdown).expect("Collection source should be written");
        app_state
            .fs
            .as_fake()
            .insert_tree(
                &collection_root,
                json!({
                    "source.md": markdown.clone(),
                }),
            )
            .await;

        let project = Project::test(app_state.fs.clone(), [collection_root.as_path()], cx).await;
        let window = cx.add_window(|window, cx| MultiWorkspace::test_new(project, window, cx));
        let workspace = window
            .read_with(cx, |multi_workspace, _cx| {
                multi_workspace.workspace().clone()
            })
            .expect("MultiWorkspace window should exist");
        window
            .update(cx, |_, window, cx| {
                workspace.update(cx, |workspace, cx| {
                    workspace.open_paths(
                        vec![source.clone()],
                        OpenOptions {
                            visible: Some(OpenVisible::All),
                            ..Default::default()
                        },
                        None,
                        window,
                        cx,
                    )
                })
            })
            .expect("Workspace window should be available")
            .await;
        let editor = cx.read(|cx| {
            workspace
                .read(cx)
                .active_item_as::<Editor>(cx)
                .expect("active item should be a real Editor")
        });
        let offset_in_paragraph = 12;
        let absolute_offset = markdown
            .find(target)
            .expect("target paragraph should exist")
            + offset_in_paragraph;
        window
            .update(cx, |_, window, cx| {
                editor.update(cx, |editor, cx| {
                    editor.change_selections(Default::default(), window, cx, |selections| {
                        selections
                            .select_ranges([MultiBufferOffset(absolute_offset)
                                ..MultiBufferOffset(absolute_offset)]);
                    });
                });
            })
            .expect("Editor cursor should move");

        cx.update(|cx| {
            super::install_workbench_lifecycle(workspace.clone(), collection_root.clone(), cx)
        });

        let (pane, item) = cx.read(|cx| {
            let pane = workspace.read(cx).active_pane().clone();
            let item = pane
                .read(cx)
                .active_item()
                .expect("active Editor item should exist");
            (pane, item)
        });
        workspace.update(cx, |_workspace, cx| {
            cx.emit(workspace::Event::UserSavedItem {
                pane: pane.downgrade(),
                item: item.downgrade_item(),
                save_intent: SaveIntent::Save,
            });
        });
        cx.run_until_parked();

        let collection = workbench::WorkbenchCollection::open(&collection_root)
            .expect("Collection should reopen after successful save");
        let resolved = collection
            .resolve_current_reading_position(&markdown)
            .expect("durable current reading position should resolve")
            .expect("successful user save should persist a current reading position");
        assert_eq!(resolved.status(), workbench::ReadingPositionStatus::Exact);
        assert_eq!(resolved.paragraph(), target);
        assert_eq!(resolved.offset_in_paragraph(), offset_in_paragraph);
        assert_eq!(resolved.absolute_offset(), absolute_offset);
    }

    #[gpui::test]
    async fn opening_collection_source_restores_the_relocated_cursor(cx: &mut TestAppContext) {
        let app_state = crate::zed::tests::init_test(cx);
        let collection_root =
            std::env::temp_dir().join(format!("zed-workbench-restore-{}", uuid::Uuid::new_v4()));
        let _temporary_collection = TemporaryCollection(collection_root.clone());
        std::fs::create_dir(&collection_root).expect("temporary Collection should be created");
        let source = collection_root.join("source.md");
        let target = "A distinctive paragraph owns the restored cursor.";
        let original_markdown =
            format!("# Source\n\nOpening context.\n\n{target}\n\nEnding context.");
        std::fs::write(&source, &original_markdown).expect("original source should be written");
        let offset_in_paragraph = 14;
        let original_offset = original_markdown
            .find(target)
            .expect("target paragraph should exist")
            + offset_in_paragraph;
        let mut collection = workbench::WorkbenchCollection::open(&collection_root)
            .expect("portable Collection should open");
        collection
            .save_reading_position(&original_markdown, original_offset)
            .expect("fixture reading position should be saved");
        drop(collection);

        let relocated_markdown = format!(
            "# Externally inserted material\n\nFirst preceding paragraph.\n\nSecond preceding paragraph.\n\n{original_markdown}"
        );
        std::fs::write(&source, &relocated_markdown)
            .expect("externally edited source should be written");
        app_state
            .fs
            .as_fake()
            .insert_tree(
                &collection_root,
                json!({
                    "source.md": relocated_markdown.clone(),
                }),
            )
            .await;

        let project = Project::test(app_state.fs.clone(), [collection_root.as_path()], cx).await;
        let window = cx.add_window(|window, cx| MultiWorkspace::test_new(project, window, cx));
        let workspace = window
            .read_with(cx, |multi_workspace, _cx| {
                multi_workspace.workspace().clone()
            })
            .expect("MultiWorkspace window should exist");
        window
            .update(cx, |_, window, cx| {
                super::install_workbench_restore_lifecycle(
                    workspace.clone(),
                    collection_root.clone(),
                    window,
                    cx,
                );
            })
            .expect("Workspace window should be available");

        window
            .update(cx, |_, window, cx| {
                workspace.update(cx, |workspace, cx| {
                    workspace.open_paths(
                        vec![source.clone()],
                        OpenOptions {
                            visible: Some(OpenVisible::All),
                            ..Default::default()
                        },
                        None,
                        window,
                        cx,
                    )
                })
            })
            .expect("Workspace window should be available")
            .await;
        cx.run_until_parked();

        let editor = cx.read(|cx| {
            workspace
                .read(cx)
                .active_item_as::<Editor>(cx)
                .expect("opened source should be a real Editor")
        });
        let restored_offset = cx.read(|cx| {
            let editor = editor.read(cx);
            let snapshot = editor.buffer().read(cx).snapshot(cx);
            editor.newest_selection_head().to_offset(&snapshot).0
        });
        let expected_offset = relocated_markdown
            .find(target)
            .expect("target paragraph should remain")
            + offset_in_paragraph;

        assert_eq!(restored_offset, expected_offset);
        assert_ne!(restored_offset, original_offset);
    }
}
