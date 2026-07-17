mod departed_agent_thread;
mod guided_attention;
mod knowledge_tree;
mod maintained_markdown_preview;
mod next_button;
mod priority_queue;
#[cfg_attr(not(test), allow(dead_code))]
mod proposal_review;

pub(crate) use knowledge_tree::AltereKnowledgeTreePanel;
pub(crate) use maintained_markdown_preview::AltereMarkdownPreviewToolbar;
pub(crate) use next_button::{AltereNextButton, FocusNextRepetition, NextBuffer, next_button};
pub(crate) use priority_queue::AlterePriorityQueuePanel;
pub(crate) use proposal_review::AltereProposalReviewToolbar;

use anyhow::Context as _;
use command_palette_hooks::CommandPaletteFilter;
use gpui::{App, Context, KeyBinding, Unbind, Window};
use serde::Deserialize;
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};
use uuid::Uuid;
use workspace::{Workspace, notifications::DetachAndPromptErr};

#[derive(Clone, Copy)]
pub(crate) struct AltereMode {
    enabled: bool,
}

impl AltereMode {
    pub(crate) fn from_env() -> Self {
        Self::from_value(std::env::var_os("ALTERE_MODE").as_deref())
    }

    fn from_value(value: Option<&OsStr>) -> Self {
        Self {
            enabled: value == Some(OsStr::new("1")),
        }
    }

    pub(crate) fn show_next_button(self) -> bool {
        self.enabled
    }

    pub(crate) fn show_debugger(self) -> bool {
        !self.enabled
    }

    pub(crate) fn show_terminal(self) -> bool {
        !self.enabled
    }

    pub(crate) fn show_collaboration(self) -> bool {
        !self.enabled
    }

    pub(crate) fn show_edit_prediction(self) -> bool {
        !self.enabled
    }

    pub(crate) fn keep_markdown_preview_open(self) -> bool {
        self.enabled
    }

    fn hidden_command_namespaces(self) -> &'static [&'static str] {
        if self.enabled {
            &["terminal_panel", "terminal"]
        } else {
            &[]
        }
    }

    pub(crate) fn show_priority_queue(self) -> bool {
        self.enabled
    }

    pub(crate) fn show_knowledge_tree(self) -> bool {
        self.enabled
    }
}

pub(crate) fn runtime_invocation(
    runtime: &Path,
    collection: &Path,
    operation_id: &str,
) -> [String; 7] {
    [
        runtime.to_string_lossy().into_owned(),
        "rotation".into(),
        "next".into(),
        "--collection".into(),
        collection.to_string_lossy().into_owned(),
        "--operation-id".into(),
        operation_id.into(),
    ]
}

fn current_runtime_invocation(runtime: &Path, collection: &Path) -> [String; 5] {
    [
        runtime.to_string_lossy().into_owned(),
        "rotation".into(),
        "current".into(),
        "--collection".into(),
        collection.to_string_lossy().into_owned(),
    ]
}

fn proposal_invocation(runtime: &Path, collection: &Path, source_path: &Path) -> [String; 7] {
    [
        runtime.to_string_lossy().into_owned(),
        "annotation".into(),
        "proposal".into(),
        "--collection".into(),
        collection.to_string_lossy().into_owned(),
        "--source".into(),
        source_path.to_string_lossy().into_owned(),
    ]
}

#[derive(Deserialize)]
struct RuntimeReceipt {
    opened: Option<PathBuf>,
}

#[derive(Deserialize)]
struct RuntimeAction {
    path: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
enum NextRepetitionIntent {
    FocusCurrent,
    Advance,
}

fn next_repetition_intent(
    current_path: Option<&Path>,
    active_path: Option<&Path>,
) -> NextRepetitionIntent {
    match current_path {
        Some(current_path) if Some(current_path) != active_path => {
            NextRepetitionIntent::FocusCurrent
        }
        _ => NextRepetitionIntent::Advance,
    }
}

fn runtime_configuration() -> anyhow::Result<(PathBuf, PathBuf, PathBuf)> {
    let bun = std::env::var_os("ALTERE_BUN")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".bun/bin/bun"))
        })
        .context("ALTERE_BUN or HOME is required")?;
    let runtime = std::env::var_os("ALTERE_RUNTIME")
        .map(PathBuf::from)
        .context("ALTERE_RUNTIME is required in Altere mode")?;
    let collection = std::env::var_os("ALTERE_COLLECTION")
        .map(PathBuf::from)
        .context("ALTERE_COLLECTION is required in Altere mode")?;
    Ok((bun, runtime, collection))
}

pub(crate) fn register_actions(
    mode: AltereMode,
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if !mode.show_next_button() {
        return;
    }

    priority_queue::register_action(workspace);
    knowledge_tree::register_action(workspace);
    proposal_review::register_actions(workspace);
    departed_agent_thread::observe_document_departures(workspace, window, cx);
    guided_attention::observe(workspace, window, cx);
    if mode.keep_markdown_preview_open() {
        maintained_markdown_preview::register(workspace, window, cx);
    }

    workspace.register_action(|workspace, _: &FocusNextRepetition, window, cx| {
        let Some(button) = workspace
            .status_bar()
            .read(cx)
            .item_of_type::<AltereNextButton>()
        else {
            return;
        };
        let focus_handle = button.read(cx).focus_handle.clone();
        focus_handle.focus(window, cx);
    });

    workspace.register_action(|_, _: &NextBuffer, window, cx| {
        cx.spawn_in(window, async move |workspace, cx| {
            let (bun, runtime, collection) = runtime_configuration()?;
            let current_output = smol::process::Command::new(&bun)
                .args(current_runtime_invocation(&runtime, &collection))
                .output()
                .await?;
            anyhow::ensure!(
                current_output.status.success(),
                "Altere runtime failed: {}",
                String::from_utf8_lossy(&current_output.stderr).trim()
            );
            let current: Option<RuntimeAction> = serde_json::from_slice(&current_output.stdout)?;
            if let Some(current) = current {
                let active_path = workspace.update(cx, |workspace, cx| {
                    let item = workspace.active_item(cx)?;
                    item.to_any_view().downcast::<editor::Editor>().ok()?;
                    let project_path = item.project_path(cx)?;
                    workspace
                        .project()
                        .read(cx)
                        .absolute_path(&project_path, cx)
                })?;
                if next_repetition_intent(Some(&current.path), active_path.as_deref())
                    == NextRepetitionIntent::FocusCurrent
                {
                    let open_task = workspace.update_in(cx, |workspace, window, cx| {
                        workspace.open_abs_path(current.path, Default::default(), window, cx)
                    })?;
                    open_task.await?;
                    return anyhow::Ok(());
                }
            }

            let operation_id = Uuid::new_v4().to_string();
            let output = smol::process::Command::new(&bun)
                .args(runtime_invocation(&runtime, &collection, &operation_id))
                .output()
                .await?;
            anyhow::ensure!(
                output.status.success(),
                "Altere runtime failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
            let receipt: RuntimeReceipt = serde_json::from_slice(&output.stdout)?;
            let Some(path) = receipt.opened else {
                return anyhow::Ok(());
            };

            let source_path = path.strip_prefix(&collection).with_context(|| {
                format!(
                    "Rotated path {} is outside Altere Collection {}",
                    path.display(),
                    collection.display()
                )
            })?;
            let proposal_output = smol::process::Command::new(&bun)
                .args(proposal_invocation(&runtime, &collection, source_path))
                .output()
                .await?;
            anyhow::ensure!(
                proposal_output.status.success(),
                "Altere proposal projection failed: {}",
                String::from_utf8_lossy(&proposal_output.stderr).trim()
            );
            let proposal: Option<proposal_review::DetachedProposalProjection> =
                serde_json::from_slice(&proposal_output.stdout)?;

            let open_task = workspace.update_in(cx, |workspace, window, cx| {
                workspace.open_abs_path(path, Default::default(), window, cx)
            })?;
            open_task.await?;

            if let Some(proposal) = proposal {
                let review = workspace.update_in(cx, |workspace, window, cx| {
                    proposal_review::open_detached_proposal_review(proposal, workspace, window, cx)
                })?;
                if let Some(review) = review {
                    review.await?;
                }
            }

            workspace.update(cx, |workspace, cx| {
                if let Some(panel) = workspace.panel::<AlterePriorityQueuePanel>(cx) {
                    panel.update(cx, |panel, cx| panel.refresh(cx));
                }
            })?;
            anyhow::Ok(())
        })
        .detach_and_prompt_err(
            "Altere could not open the next buffer",
            window,
            cx,
            |_, _, _| None,
        );
    });
}

pub(crate) fn apply_capability_policy(mode: AltereMode, cx: &mut App) {
    CommandPaletteFilter::update_global(cx, |filter, _| {
        for namespace in mode.hidden_command_namespaces() {
            filter.hide_namespace(namespace);
        }
    });
}

pub(crate) fn key_bindings() -> Vec<KeyBinding> {
    if !AltereMode::from_env().show_next_button() {
        return Vec::new();
    }

    let mut bindings = vec![
        KeyBinding::new("ctrl-`", Unbind("terminal_panel::Toggle".into()), None),
        KeyBinding::new(
            "escape",
            FocusNextRepetition,
            Some("Editor && vim_mode == normal && !menu"),
        ),
        KeyBinding::new(
            "escape",
            FocusNextRepetition,
            Some("Workspace && !Editor && !menu"),
        ),
        KeyBinding::new("enter", NextBuffer, Some("AltereNextRepetition")),
    ];
    bindings.extend(priority_queue::key_bindings());
    bindings.extend(knowledge_tree::key_bindings());
    bindings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zed_is_only_an_adapter_to_the_standalone_rotation_command() {
        let invocation = runtime_invocation(
            Path::new("/opt/altere/ir.ts"),
            Path::new("/tmp/collection"),
            "gesture-42",
        );

        assert_eq!(
            invocation,
            [
                "/opt/altere/ir.ts",
                "rotation",
                "next",
                "--collection",
                "/tmp/collection",
                "--operation-id",
                "gesture-42",
            ]
        );
        assert_eq!(
            current_runtime_invocation(
                Path::new("/opt/altere/ir.ts"),
                Path::new("/tmp/collection")
            ),
            [
                "/opt/altere/ir.ts",
                "rotation",
                "current",
                "--collection",
                "/tmp/collection",
            ]
        );
    }

    #[test]
    fn next_repetition_returns_to_the_current_repetition_before_advancing() {
        let current = Path::new("/tmp/collection/current.md");

        assert_eq!(
            next_repetition_intent(Some(current), Some(Path::new("/tmp/collection/other.md"))),
            NextRepetitionIntent::FocusCurrent
        );
        assert_eq!(
            next_repetition_intent(Some(current), None),
            NextRepetitionIntent::FocusCurrent
        );
        assert_eq!(
            next_repetition_intent(Some(current), Some(current)),
            NextRepetitionIntent::Advance
        );
        assert_eq!(
            next_repetition_intent(None, Some(Path::new("/tmp/collection/other.md"))),
            NextRepetitionIntent::Advance
        );
    }

    #[test]
    fn altere_mode_is_reversible() {
        let upstream = AltereMode::from_value(None);
        assert!(!upstream.show_next_button());
        assert!(upstream.show_debugger());
        assert!(upstream.show_terminal());
        assert!(upstream.show_collaboration());
        assert!(upstream.show_edit_prediction());
        assert!(!upstream.keep_markdown_preview_open());
        assert!(upstream.hidden_command_namespaces().is_empty());

        let altere = AltereMode::from_value(Some(OsStr::new("1")));
        assert!(altere.show_next_button());
        assert!(altere.show_priority_queue());
        assert!(!altere.show_debugger());
        assert!(!altere.show_terminal());
        assert!(!altere.show_collaboration());
        assert!(!altere.show_edit_prediction());
        assert!(altere.keep_markdown_preview_open());
        assert_eq!(
            altere.hidden_command_namespaces(),
            &["terminal_panel", "terminal"]
        );
    }
}
