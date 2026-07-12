#[cfg(test)]
use std::cell::Cell;
#[cfg(test)]
use std::path::Component;
use std::path::{Path, PathBuf};
use std::rc::Rc;

#[cfg(test)]
use anyhow::Result;
#[cfg(test)]
use anyhow::bail;
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectionId(String);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceId(String);

pub struct CollectionSource {
    id: SourceId,
    relative_path: PathBuf,
}

impl CollectionSource {
    pub fn id(&self) -> &SourceId {
        &self.id
    }

    pub fn relative_path(&self) -> &Path {
        &self.relative_path
    }
}

pub struct WorkbenchCollection {
    id: CollectionId,
    source: CollectionSource,
}

impl WorkbenchCollection {
    pub fn open(collection_root: &Path) -> anyhow::Result<Self> {
        anyhow::ensure!(
            collection_root.is_dir(),
            "Collection path is not a directory"
        );
        let relative_path = PathBuf::from("source.md");
        anyhow::ensure!(
            collection_root.join(&relative_path).is_file(),
            "Collection source.md is not a file"
        );

        let connection = open_collection_metadata(collection_root)?;
        let (collection_id, source_id) = connection.with_savepoint(
            "load_workbench_collection_identity",
            || -> anyhow::Result<(String, String)> {
                let new_collection_id = uuid::Uuid::new_v4().to_string();
                connection.exec_bound(
                    "INSERT OR IGNORE INTO collection_identity (singleton, id) VALUES (1, ?)",
                )?(new_collection_id)?;
                let collection_id = connection.select_row::<String>(
                    "SELECT id FROM collection_identity WHERE singleton = 1",
                )?()?
                .ok_or_else(|| anyhow::anyhow!("Collection identity was not initialized"))?;

                let new_source_id = uuid::Uuid::new_v4().to_string();
                connection.exec_bound(
                    "INSERT OR IGNORE INTO sources (id, relative_path) VALUES (?, ?)",
                )?((new_source_id, "source.md"))?;
                let source_id = connection.select_row_bound::<&str, String>(
                    "SELECT id FROM sources WHERE relative_path = ?",
                )?("source.md")?
                .ok_or_else(|| anyhow::anyhow!("Collection source identity was not initialized"))?;

                Ok((collection_id, source_id))
            },
        )?;

        Ok(Self {
            id: CollectionId(collection_id),
            source: CollectionSource {
                id: SourceId(source_id),
                relative_path,
            },
        })
    }

    pub fn id(&self) -> &CollectionId {
        &self.id
    }

    pub fn source(&self) -> &CollectionSource {
        &self.source
    }
}

fn open_collection_metadata(
    collection_root: &std::path::Path,
) -> anyhow::Result<sqlez::connection::Connection> {
    let metadata_directory = collection_root.join(".workbench");
    std::fs::create_dir_all(&metadata_directory)?;

    let metadata_path = metadata_directory.join("collection.sqlite");
    let connection = sqlez::connection::Connection::open_file(&metadata_path.to_string_lossy());
    anyhow::ensure!(
        connection.persistent(),
        "failed to open portable Collection metadata at {}",
        metadata_path.display()
    );
    connection.migrate(
        "workbench_collection",
        &[
            "PRAGMA user_version = 1;",
            r#"
                CREATE TABLE collection_identity (
                    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                    id TEXT NOT NULL UNIQUE
                );
                CREATE TABLE sources (
                    id TEXT PRIMARY KEY,
                    relative_path TEXT NOT NULL UNIQUE
                );
                PRAGMA user_version = 2;
            "#,
        ],
        &mut |_, _, _| false,
    )?;

    Ok(connection)
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
            let collection = match WorkbenchCollection::open(&collection_root) {
                Ok(collection) => collection,
                Err(error) => {
                    log::error!("failed to open Workbench Collection: {error:#}");
                    return;
                }
            };
            host.open_collection(OpenCollectionRequest {
                source: collection_root.join(collection.source().relative_path()),
                collection_root,
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

    #[gpui::test]
    async fn opening_a_collection_initializes_portable_metadata_before_host_open(
        cx: &mut gpui::TestAppContext,
    ) {
        use std::{cell::Cell, rc::Rc};

        struct RecordingHost {
            opened: Cell<bool>,
            metadata_existed_when_opened: Cell<bool>,
        }

        impl super::WorkbenchHost for RecordingHost {
            fn open_collection(&self, request: super::OpenCollectionRequest) {
                self.metadata_existed_when_opened.set(
                    request
                        .collection_root
                        .join(".workbench/collection.sqlite")
                        .is_file(),
                );
                self.opened.set(true);
            }
        }

        let collection = tempfile::tempdir().expect("temporary Collection should be created");
        std::fs::write(collection.path().join("source.md"), "# Source")
            .expect("Collection source should be written");
        let host = Rc::new(RecordingHost {
            opened: Cell::new(false),
            metadata_existed_when_opened: Cell::new(false),
        });

        cx.update(|cx| super::init_workbench_capability(host.clone(), cx));
        cx.update(|cx| cx.dispatch_action(&super::OpenCollection));
        cx.simulate_path_prompt_response({
            let selected = collection.path().to_path_buf();
            move |_options| Some(vec![selected])
        });
        cx.run_until_parked();

        assert!(
            host.opened.get(),
            "selected Collection should reach the host"
        );
        assert!(
            host.metadata_existed_when_opened.get(),
            "portable Collection metadata should exist before host open"
        );
        assert!(
            collection
                .path()
                .join(".workbench/collection.sqlite")
                .is_file(),
            "opening should initialize portable Collection metadata"
        );
    }

    #[test]
    fn collection_identity_survives_relocating_its_directory() {
        let parent = tempfile::tempdir().expect("temporary parent should be created");
        let original_root = parent.path().join("collection");
        std::fs::create_dir(&original_root).expect("Collection directory should be created");
        std::fs::write(original_root.join("source.md"), "# Source")
            .expect("Collection source should be written");

        let original = super::WorkbenchCollection::open(&original_root)
            .expect("portable Collection should open");
        let collection_id = original.id().clone();
        let source_id = original.source().id().clone();
        assert_eq!(
            original.source().relative_path(),
            std::path::Path::new("source.md")
        );
        drop(original);

        let relocated_root = parent.path().join("renamed-collection");
        std::fs::rename(&original_root, &relocated_root)
            .expect("entire Collection should relocate");
        let relocated = super::WorkbenchCollection::open(&relocated_root)
            .expect("relocated Collection should reopen");

        assert_eq!(relocated.id(), &collection_id);
        assert_eq!(relocated.source().id(), &source_id);
        assert_eq!(
            relocated.source().relative_path(),
            std::path::Path::new("source.md")
        );
    }
}
