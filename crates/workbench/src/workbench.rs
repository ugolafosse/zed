#[cfg(test)]
use std::cell::Cell;
#[cfg(test)]
use std::path::Component;
use std::path::PathBuf;
use std::rc::Rc;

#[cfg(test)]
use anyhow::{Result, bail};
#[cfg(test)]
use assets::Assets;
#[cfg(test)]
use editor::Editor;
#[cfg(test)]
use gpui::{AnyWindowHandle, Entity, TestAppContext};
use gpui::{App, PathPromptOptions};
#[cfg(test)]
use gpui::{AppContext as _, Focusable as _, UpdateGlobal as _};
#[cfg(test)]
use settings::{KeybindSource, SettingsStore};

gpui::actions!(workbench, [OpenCollection]);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenCollectionRequest {
    pub collection_root: PathBuf,
    pub source: PathBuf,
}

pub trait WorkbenchHost {
    fn open_collection(&self, request: OpenCollectionRequest);
}

pub fn init_workbench_capability(host: Rc<dyn WorkbenchHost>, cx: &mut App) {
    cx.on_action(move |_: &OpenCollection, cx| {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open Collection".into()),
        });
        let host = host.clone();
        cx.spawn(async move |_cx| {
            let Ok(Ok(Some(paths))) = prompt.await else {
                return;
            };
            let Some(collection_root) = paths.into_iter().next() else {
                return;
            };
            let source = collection_root.join("source.md");
            if !collection_root.is_dir() || !source.is_file() {
                return;
            }
            host.open_collection(OpenCollectionRequest {
                collection_root,
                source,
            });
        })
        .detach();
    });
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkbenchMode {
    Normal,
    Insert,
}

#[cfg(test)]
pub struct WorkbenchLaunch {
    markdown: String,
}

#[cfg(test)]
impl WorkbenchLaunch {
    pub fn from_cli_arguments<I, S>(arguments: I) -> anyhow::Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let mut arguments = arguments.into_iter();
        let collection = arguments
            .next()
            .map(|argument| PathBuf::from(argument.as_ref()))
            .ok_or_else(|| anyhow::anyhow!("missing Collection directory"))?;
        let relative_source = arguments
            .next()
            .map(|argument| PathBuf::from(argument.as_ref()))
            .ok_or_else(|| anyhow::anyhow!("missing relative Markdown path"))?;
        if arguments.next().is_some() {
            bail!("expected a Collection directory and one relative Markdown path");
        }
        if relative_source.is_absolute()
            || relative_source.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            bail!("source path must remain relative to the Collection");
        }

        let collection = collection.canonicalize()?;
        if !collection.is_dir() {
            bail!("Collection path is not a directory");
        }
        let source = collection.join(relative_source).canonicalize()?;
        if !source.starts_with(&collection) {
            bail!("source path resolves outside the Collection");
        }

        Ok(Self {
            markdown: std::fs::read_to_string(source)?,
        })
    }

    pub async fn launch_for_test(self, cx: &mut TestAppContext) -> Result<WorkbenchTestSession> {
        WorkbenchTestSession::launch(&self.markdown, cx).await
    }
}

#[cfg(test)]
fn initialize(cx: &mut App) -> Result<()> {
    Assets.load_test_fonts(cx);
    settings::init(cx);
    theme_settings::init(theme::LoadThemes::JustBase, cx);
    release_channel::init(semver::Version::new(0, 0, 0), cx);
    editor::init(cx);
    vim::init(cx);

    SettingsStore::update_global(cx, |store, cx| {
        let _ = store.set_user_settings(r#"{"vim_mode":true}"#, cx);
    });

    let mut default_keymap =
        settings::KeymapFile::load_asset_allow_partial_failure(settings::DEFAULT_KEYMAP_PATH, cx)?;
    for binding in &mut default_keymap {
        binding.set_meta(KeybindSource::Default.meta());
    }
    cx.bind_keys(default_keymap);

    let mut vim_keymap =
        settings::KeymapFile::load_asset_allow_partial_failure(settings::VIM_KEYMAP_PATH, cx)?;
    for binding in &mut vim_keymap {
        binding.set_meta(KeybindSource::Vim.meta());
    }
    cx.bind_keys(vim_keymap);

    Ok(())
}

#[cfg(test)]
pub struct WorkbenchTestSession {
    window: AnyWindowHandle,
    editor: Entity<Editor>,
    mode: Rc<Cell<WorkbenchMode>>,
}

#[cfg(test)]
impl WorkbenchTestSession {
    pub async fn launch(markdown: &str, cx: &mut TestAppContext) -> Result<Self> {
        let mode = Rc::new(Cell::new(WorkbenchMode::Normal));
        cx.update(|cx| {
            initialize(cx)?;

            let insert_mode = mode.clone();
            cx.on_action(move |_: &vim::SwitchToInsertMode, _| {
                insert_mode.set(WorkbenchMode::Insert);
            });
            let normal_mode = mode.clone();
            cx.on_action(move |_: &vim::SwitchToNormalMode, _| {
                normal_mode.set(WorkbenchMode::Normal);
            });

            anyhow::Ok(())
        })?;

        let buffer = cx.new(|cx| language::Buffer::local(markdown, cx));
        let window = cx.add_window(|window, cx| {
            let editor = Editor::for_buffer(buffer, None, window, cx);
            window.focus(&editor.focus_handle(cx), cx);
            editor
        });
        let editor = window
            .root(cx)
            .expect("Workbench window should contain an editor");
        cx.run_until_parked();

        Ok(Self {
            window: window.into(),
            editor,
            mode,
        })
    }

    pub fn dispatch_keys(&self, keys: &str, cx: &mut TestAppContext) {
        cx.simulate_keystrokes(self.window, keys);
    }

    pub fn active_text(&self, cx: &TestAppContext) -> String {
        cx.read(|cx| self.editor.read(cx).text(cx))
    }

    pub fn active_mode(&self, _cx: &mut TestAppContext) -> WorkbenchMode {
        self.mode.get()
    }
}

#[cfg(test)]
mod tests {
    use super::{WorkbenchLaunch, WorkbenchMode, WorkbenchTestSession};

    #[gpui::test]
    async fn learner_edits_markdown_with_authentic_vim(cx: &mut gpui::TestAppContext) {
        let session = WorkbenchTestSession::launch("", cx)
            .await
            .expect("Workbench should launch an editor with Vim enabled");

        session.dispatch_keys("i # space shift-n o t e escape", cx);

        assert_eq!(session.active_text(cx), "# Note");
        assert_eq!(session.active_mode(cx), WorkbenchMode::Normal);

        session.dispatch_keys("0 l u", cx);

        assert_eq!(session.active_text(cx), "");
        assert_eq!(session.active_mode(cx), WorkbenchMode::Normal);
    }

    #[gpui::test]
    async fn command_line_launch_opens_collection_markdown_in_vim(cx: &mut gpui::TestAppContext) {
        let collection = tempfile::tempdir().expect("temporary Collection should be created");
        std::fs::write(collection.path().join("source.md"), "# Source\n")
            .expect("Markdown fixture should be written");

        let launch = WorkbenchLaunch::from_cli_arguments([
            collection.path().as_os_str(),
            std::ffi::OsStr::new("source.md"),
        ])
        .expect("valid Collection arguments should prepare a Workbench launch");
        let session = launch
            .launch_for_test(cx)
            .await
            .expect("prepared launch should compose the direct Editor/Vim shell");

        assert_eq!(session.active_text(cx), "# Source\n");
        assert_eq!(session.active_mode(cx), WorkbenchMode::Normal);
    }

    #[gpui::test]
    async fn open_collection_action_requests_the_selected_collection_from_the_host(
        cx: &mut gpui::TestAppContext,
    ) {
        use std::{cell::RefCell, rc::Rc};

        struct RecordingHost {
            request: RefCell<Option<super::OpenCollectionRequest>>,
        }

        impl super::WorkbenchHost for RecordingHost {
            fn open_collection(&self, request: super::OpenCollectionRequest) {
                self.request.replace(Some(request));
            }
        }

        let collection = tempfile::tempdir().expect("temporary Collection should be created");
        std::fs::write(collection.path().join("source.md"), "# Source")
            .expect("Collection source should be written");
        let host = Rc::new(RecordingHost {
            request: RefCell::new(None),
        });

        cx.update(|cx| super::init_workbench_capability(host.clone(), cx));
        cx.update(|cx| cx.dispatch_action(&super::OpenCollection));

        assert!(
            cx.did_prompt_for_paths(),
            "Open Collection should present the native directory picker"
        );
        cx.simulate_path_prompt_response({
            let selected = collection.path().to_path_buf();
            move |options| {
                assert!(options.directories);
                assert!(!options.files);
                assert!(!options.multiple);
                Some(vec![selected])
            }
        });
        cx.run_until_parked();

        assert_eq!(
            host.request.borrow().as_ref(),
            Some(&super::OpenCollectionRequest {
                collection_root: collection.path().to_path_buf(),
                source: collection.path().join("source.md"),
            })
        );
    }
}
