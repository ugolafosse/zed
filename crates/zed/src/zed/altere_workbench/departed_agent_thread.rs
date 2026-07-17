use agent_client_protocol::schema::v1 as acp;
use agent_ui::{
    Agent, AgentInitialContent, AgentPanel, AgentThreadSource, CreateThreadOptions, ThreadId,
};
use anyhow::Context as _;
use editor::Editor;
use futures::AsyncWriteExt as _;
use gpui::{App, Context, Entity, SharedString, Window};
use serde::Deserialize;
use std::{path::PathBuf, process::Stdio};
use workspace::{Workspace, notifications::DetachAndPromptErr};

pub(crate) struct DepartedDocumentAgentRequest {
    pub title: SharedString,
    pub prompt: String,
    pub agent: Option<Agent>,
}

#[derive(Clone)]
struct ActiveDocument {
    path: PathBuf,
    editor: Entity<Editor>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AnnotationDepartureRequest {
    annotation_id: String,
    title: String,
    prompt: String,
}

fn active_document(workspace: &Workspace, cx: &App) -> Option<ActiveDocument> {
    let item = workspace.active_item(cx)?;
    let project_path = item.project_path(cx)?;
    let editor = item.act_as::<Editor>(cx)?;
    let path = workspace
        .project()
        .read(cx)
        .absolute_path(&project_path, cx)?;
    (path.extension().and_then(|extension| extension.to_str()) == Some("md"))
        .then_some(ActiveDocument { path, editor })
}

fn annotation_departure_invocation(
    runtime: &std::path::Path,
    collection: &std::path::Path,
    source_path: &std::path::Path,
) -> [String; 7] {
    [
        runtime.to_string_lossy().into_owned(),
        "annotation".into(),
        "depart".into(),
        "--collection".into(),
        collection.to_string_lossy().into_owned(),
        "--source".into(),
        source_path.to_string_lossy().into_owned(),
    ]
}

fn annotation_acknowledgement_invocation(
    runtime: &std::path::Path,
    collection: &std::path::Path,
    annotation_id: &str,
    thread_id: &str,
) -> [String; 9] {
    [
        runtime.to_string_lossy().into_owned(),
        "annotation".into(),
        "acknowledge".into(),
        "--collection".into(),
        collection.to_string_lossy().into_owned(),
        "--annotation-id".into(),
        annotation_id.into(),
        "--thread-id".into(),
        thread_id.into(),
    ]
}

async fn request_annotation_departure(
    bun: &std::path::Path,
    runtime: &std::path::Path,
    collection: &std::path::Path,
    source_path: &std::path::Path,
    markdown: &str,
) -> anyhow::Result<Option<AnnotationDepartureRequest>> {
    let mut child = smol::process::Command::new(bun)
        .args(annotation_departure_invocation(
            runtime,
            collection,
            source_path,
        ))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .context("Altere runtime stdin unavailable")?;
    stdin.write_all(markdown.as_bytes()).await?;
    stdin.close().await?;
    drop(stdin);

    let output = child.output().await?;
    anyhow::ensure!(
        output.status.success(),
        "Altere annotation departure failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

async fn acknowledge_annotation_departure(
    bun: &std::path::Path,
    runtime: &std::path::Path,
    collection: &std::path::Path,
    annotation_id: &str,
    thread_id: &str,
) -> anyhow::Result<()> {
    let output = smol::process::Command::new(bun)
        .args(annotation_acknowledgement_invocation(
            runtime,
            collection,
            annotation_id,
            thread_id,
        ))
        .output()
        .await?;
    anyhow::ensure!(
        output.status.success(),
        "Altere annotation acknowledgement failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}

fn depart_document(document: ActiveDocument, window: &mut Window, cx: &mut Context<Workspace>) {
    let markdown = document.editor.read(cx).text(cx);
    cx.spawn_in(window, async move |workspace, cx| {
        let (bun, runtime, collection) = super::runtime_configuration()?;
        let source_path = document.path.strip_prefix(&collection).with_context(|| {
            format!(
                "Departed document {} is outside Altere Collection {}",
                document.path.display(),
                collection.display()
            )
        })?;
        let Some(request) =
            request_annotation_departure(&bun, &runtime, &collection, source_path, &markdown)
                .await?
        else {
            return anyhow::Ok(());
        };

        let existing_panel =
            workspace.update_in(cx, |workspace, _, cx| workspace.panel::<AgentPanel>(cx))?;
        let panel = if let Some(panel) = existing_panel {
            panel
        } else {
            let loaded_panel = AgentPanel::load(workspace.clone(), cx.clone()).await?;
            workspace.update_in(cx, |workspace, window, cx| {
                workspace.panel::<AgentPanel>(cx).unwrap_or_else(|| {
                    workspace.add_panel(loaded_panel.clone(), window, cx);
                    loaded_panel
                })
            })?
        };

        let agent_id = std::env::var("ALTERE_ANNOTATION_AGENT")
            .context("ALTERE_ANNOTATION_AGENT is required for annotation departure")?;
        let thread_id = panel.update_in(cx, |panel, window, cx| {
            dispatch_departed_document(
                panel,
                DepartedDocumentAgentRequest {
                    title: request.title.into(),
                    prompt: request.prompt,
                    agent: Some(Agent::Custom {
                        id: project::AgentId::new(agent_id),
                    }),
                },
                window,
                cx,
            )
        })?;
        acknowledge_annotation_departure(
            &bun,
            &runtime,
            &collection,
            &request.annotation_id,
            &thread_id.to_key_string(),
        )
        .await?;
        anyhow::Ok(())
    })
    .detach_and_prompt_err(
        "Altere could not start the departed correction",
        window,
        cx,
        |_, _, _| None,
    );
}

pub(crate) fn observe_document_departures(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let mut previous = active_document(workspace, cx);
    let workspace_handle = cx.entity();
    cx.subscribe_in(
        &workspace_handle,
        window,
        move |workspace, _, event, window, cx| {
            if !matches!(event, workspace::Event::ActiveItemChanged) {
                return;
            }

            let current = active_document(workspace, cx);
            let departed = previous.take().filter(|departed| {
                current
                    .as_ref()
                    .is_none_or(|current| current.path != departed.path)
            });
            previous = current;
            if let Some(departed) = departed {
                depart_document(departed, window, cx);
            }
        },
    )
    .detach();
}

pub(crate) fn dispatch_departed_document(
    panel: &mut AgentPanel,
    request: DepartedDocumentAgentRequest,
    window: &mut Window,
    cx: &mut Context<AgentPanel>,
) -> ThreadId {
    panel.create_thread_with_options(
        CreateThreadOptions {
            title: Some(request.title),
            initial_content: Some(AgentInitialContent::ContentBlock {
                blocks: vec![acp::ContentBlock::Text(acp::TextContent::new(
                    request.prompt,
                ))],
                auto_submit: true,
            }),
            agent: request.agent,
            model: None,
            work_dirs: None,
        },
        AgentThreadSource::AgentPanel,
        window,
        cx,
    )
}

#[cfg(all(test, feature = "visual-tests"))]
mod tests {
    use super::*;
    use crate::zed::tests::init_test;
    use acp_thread::AgentThreadEntry;
    use agent_ui::{Agent, AgentPanel};
    use editor::Editor;
    use gpui::{AppContext as _, Focusable as _, TestAppContext};
    use project::Project;
    use serde_json::json;
    use std::path::Path;
    use workspace::{MultiWorkspace, Panel as _, dock::DockPosition};

    #[gpui::test]
    async fn departed_document_starts_retained_thread_without_stealing_focus(
        cx: &mut TestAppContext,
    ) {
        let app_state = init_test(cx);
        cx.update(|cx| {
            agent::ThreadStore::init_global(cx);
            language_model::LanguageModelRegistry::test(cx);
        });
        app_state
            .fs
            .as_fake()
            .insert_tree("/project", json!({ "note.md": "# Source\n" }))
            .await;
        let project = Project::test(app_state.fs.clone(), [Path::new("/project")], cx).await;
        let source_buffer = project
            .update(cx, |project, cx| {
                project.open_local_buffer(Path::new("/project/note.md"), cx)
            })
            .await
            .expect("source buffer should open");
        let canonical_buffer = source_buffer.clone();
        let canonical_before = canonical_buffer.read_with(cx, |buffer, _| buffer.text());

        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace =
            multi_workspace.read_with(cx, |multi_workspace, _| multi_workspace.workspace().clone());
        let (weak_workspace, async_window_context) = workspace
            .update_in(cx, |workspace, window, cx| {
                (workspace.weak_handle(), window.to_async(cx))
            });
        let panel = AgentPanel::load(weak_workspace, async_window_context)
            .await
            .expect("agent panel should load");
        let editor = workspace.update_in(cx, |workspace, window, cx| {
            let editor = cx.new(|cx| Editor::for_buffer(source_buffer, Some(project), window, cx));
            workspace.add_item_to_active_pane(Box::new(editor.clone()), None, true, window, cx);
            editor.focus_handle(cx).focus(window, cx);
            workspace.add_panel(panel.clone(), window, cx);
            editor
        });
        let active_item_before = workspace.read_with(cx, |workspace, cx| {
            workspace
                .active_item(cx)
                .expect("editor should be active")
                .item_id()
        });
        let panel_position =
            workspace.update_in(cx, |_, window, cx| panel.read(cx).position(window, cx));
        let dock_was_closed = workspace.read_with(cx, |workspace, cx| match panel_position {
            DockPosition::Left => !workspace.left_dock().read(cx).is_open(),
            DockPosition::Right => !workspace.right_dock().read(cx).is_open(),
            DockPosition::Bottom => !workspace.bottom_dock().read(cx).is_open(),
        });
        assert!(dock_was_closed, "agent panel must begin hidden");

        let prompt = "Continue the correction attached to note.md without editing the source.";
        let thread_id = panel.update_in(cx, |panel, window, cx| {
            dispatch_departed_document(
                panel,
                DepartedDocumentAgentRequest {
                    title: "Correction · note.md".into(),
                    prompt: prompt.to_string(),
                    agent: Some(Agent::Stub),
                },
                window,
                cx,
            )
        });
        cx.run_until_parked();

        let (is_retained, transcript) = panel.read_with(cx, |panel, cx| {
            let conversation = panel
                .conversation_view_for_id(&thread_id, cx)
                .expect("retained thread should be addressable");
            let thread_view = conversation
                .read(cx)
                .root_thread_view()
                .expect("auto-submitted thread should connect");
            let transcript = thread_view
                .read(cx)
                .thread
                .read(cx)
                .entries()
                .iter()
                .map(|entry| {
                    (
                        matches!(entry, AgentThreadEntry::UserMessage(_)),
                        entry.to_markdown(cx),
                    )
                })
                .collect::<Vec<_>>();
            (panel.is_retained_thread(&thread_id), transcript)
        });
        assert!(
            is_retained,
            "departed-document thread should remain retained"
        );
        assert!(
            transcript.iter().any(|(is_user, markdown)| {
                *is_user && markdown == &format!("## User\n\n{prompt}\n\n")
            }),
            "initial content should be auto-submitted through ACP: {transcript:?}"
        );

        workspace.read_with(cx, |workspace, cx| {
            assert_eq!(
                workspace.active_item(cx).map(|item| item.item_id()),
                Some(active_item_before),
                "dispatch must not replace the active editor item"
            );
            let dock_is_still_closed = match panel_position {
                DockPosition::Left => !workspace.left_dock().read(cx).is_open(),
                DockPosition::Right => !workspace.right_dock().read(cx).is_open(),
                DockPosition::Bottom => !workspace.bottom_dock().read(cx).is_open(),
            };
            assert!(dock_is_still_closed, "dispatch must not open AgentPanel");
        });
        workspace.update_in(cx, |_, window, cx| {
            assert!(editor.read(cx).focus_handle(cx).is_focused(window));
        });
        assert_eq!(
            canonical_buffer.read_with(cx, |buffer, _| buffer.text()),
            canonical_before
        );
        assert_eq!(
            app_state
                .fs
                .load(Path::new("/project/note.md"))
                .await
                .expect("canonical file should remain readable"),
            "# Source\n"
        );
    }
}
