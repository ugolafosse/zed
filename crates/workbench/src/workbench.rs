#[cfg(test)]
use std::{cell::Cell, rc::Rc};

#[cfg(test)]
use anyhow::Result;
#[cfg(test)]
use assets::Assets;
#[cfg(test)]
use editor::Editor;
#[cfg(test)]
use gpui::{
    AnyWindowHandle, AppContext as _, Entity, Focusable as _, TestAppContext, UpdateGlobal as _,
};
#[cfg(test)]
use settings::{KeybindSource, SettingsStore};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkbenchMode {
    Normal,
    Insert,
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
            Assets.load_test_fonts(cx);
            settings::init(cx);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            release_channel::init(semver::Version::new(0, 0, 0), cx);
            editor::init(cx);
            vim::init(cx);

            SettingsStore::update_global(cx, |store, cx| {
                let _ = store.set_user_settings(r#"{"vim_mode":true}"#, cx);
            });

            let mut default_keymap = settings::KeymapFile::load_asset_allow_partial_failure(
                settings::DEFAULT_KEYMAP_PATH,
                cx,
            )?;
            for binding in &mut default_keymap {
                binding.set_meta(KeybindSource::Default.meta());
            }
            cx.bind_keys(default_keymap);

            let mut vim_keymap = settings::KeymapFile::load_asset_allow_partial_failure(
                settings::VIM_KEYMAP_PATH,
                cx,
            )?;
            for binding in &mut vim_keymap {
                binding.set_meta(KeybindSource::Vim.meta());
            }
            cx.bind_keys(vim_keymap);

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
    use super::{WorkbenchMode, WorkbenchTestSession};

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
}
