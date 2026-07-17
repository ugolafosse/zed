use editor::Editor;
use gpui::{App, Context, Entity, Window};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use workspace::{Workspace, notifications::DetachAndPromptErr};

#[derive(Clone)]
struct ActiveMarkdownEditor {
    path: PathBuf,
    editor: Entity<Editor>,
}

#[derive(Deserialize)]
struct CurrentRotationAction {
    path: PathBuf,
}

fn current_invocation(runtime: &Path, collection: &Path) -> [String; 5] {
    [
        runtime.to_string_lossy().into_owned(),
        "rotation".into(),
        "current".into(),
        "--collection".into(),
        collection.to_string_lossy().into_owned(),
    ]
}

fn guided_read_only(active_path: &Path, current_path: Option<&Path>) -> bool {
    current_path != Some(active_path)
}

fn active_markdown_editor(workspace: &Workspace, cx: &App) -> Option<ActiveMarkdownEditor> {
    let item = workspace.active_item(cx)?;
    let project_path = item.project_path(cx)?;
    let editor = item.act_as::<Editor>(cx)?;
    let path = workspace
        .project()
        .read(cx)
        .absolute_path(&project_path, cx)?;
    (path.extension().and_then(|extension| extension.to_str()) == Some("md"))
        .then_some(ActiveMarkdownEditor { path, editor })
}

fn apply_to_active_editor(workspace: &Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let Some(active) = active_markdown_editor(workspace, cx) else {
        return;
    };

    cx.spawn_in(window, async move |workspace, cx| {
        let (bun, runtime, collection) = super::runtime_configuration()?;
        let output = smol::process::Command::new(&bun)
            .args(current_invocation(&runtime, &collection))
            .output()
            .await?;
        anyhow::ensure!(
            output.status.success(),
            "Altere current rotation projection failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        let current: Option<CurrentRotationAction> = serde_json::from_slice(&output.stdout)?;
        let read_only = guided_read_only(
            &active.path,
            current.as_ref().map(|action| action.path.as_path()),
        );

        workspace.update_in(cx, |_, _, cx| {
            active.editor.update(cx, |editor, cx| {
                editor.set_read_only(read_only);
                cx.notify();
            });
        })?;
        anyhow::Ok(())
    })
    .detach_and_prompt_err(
        "Altere could not apply Guided attention",
        window,
        cx,
        |_, _, _| None,
    );
}

pub(crate) fn observe(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    if std::env::var("ALTERE_ATTENTION_MODE").as_deref() == Ok("open") {
        return;
    }

    apply_to_active_editor(workspace, window, cx);
    let workspace_handle = cx.entity();
    cx.subscribe_in(
        &workspace_handle,
        window,
        |workspace, _, event, window, cx| {
            if matches!(event, workspace::Event::ActiveItemChanged) {
                apply_to_active_editor(workspace, window, cx);
            }
        },
    )
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn guided_attention_only_allows_the_current_rotation_document_to_edit() {
        let current = Path::new("/collection/current.md");

        assert!(!guided_read_only(current, Some(current)));
        assert!(guided_read_only(
            Path::new("/collection/other.md"),
            Some(current)
        ));
        assert!(guided_read_only(current, None));
    }

    #[test]
    fn zed_only_requests_the_current_rotation_projection() {
        assert_eq!(
            current_invocation(Path::new("/opt/altere/ir.ts"), Path::new("/collection")),
            [
                "/opt/altere/ir.ts",
                "rotation",
                "current",
                "--collection",
                "/collection",
            ]
        );
    }
}
