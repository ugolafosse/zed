use std::path::{Component, PathBuf};
#[cfg(test)]
use std::{cell::Cell, rc::Rc};

use anyhow::Result;
use anyhow::bail;
use assets::Assets;
use editor::Editor;
#[cfg(test)]
use gpui::{AnyWindowHandle, Entity, TestAppContext};
use gpui::{App, AppContext as _, Focusable as _, UpdateGlobal as _, WindowOptions};
use settings::{KeybindSource, SettingsStore};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkbenchMode {
    Normal,
    Insert,
}

pub struct WorkbenchLaunch {
    markdown: String,
}

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

    #[cfg(test)]
    pub async fn launch_for_test(self, cx: &mut TestAppContext) -> Result<WorkbenchTestSession> {
        WorkbenchTestSession::launch(&self.markdown, cx).await
    }

    pub fn run(self) {
        gpui_platform::application()
            .with_assets(Assets)
            .run(move |cx| {
                initialize(cx, true).expect("Workbench application should initialize");
                cx.activate(true);
                let buffer = cx.new(|cx| language::Buffer::local(self.markdown, cx));
                cx.open_window(WindowOptions::default(), |window, cx| {
                    let editor = cx.new(|cx| Editor::for_buffer(buffer, None, window, cx));
                    editor.update(cx, |editor, cx| window.focus(&editor.focus_handle(cx), cx));
                    editor
                })
                .expect("Workbench editor window should open");
            });
    }
}

fn initialize(cx: &mut App, load_application_fonts: bool) -> Result<()> {
    if load_application_fonts {
        Assets.load_fonts(cx)?;
    } else {
        Assets.load_test_fonts(cx);
    }
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
            initialize(cx, false)?;

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
}
