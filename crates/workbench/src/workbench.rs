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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadingPositionId(String);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadingPositionStatus {
    Exact,
    Relocated,
    Stale,
}

pub struct ResolvedReadingPosition {
    status: ReadingPositionStatus,
    absolute_offset: usize,
    paragraph: String,
    offset_in_paragraph: usize,
}

impl ResolvedReadingPosition {
    pub fn status(&self) -> ReadingPositionStatus {
        self.status
    }

    pub fn absolute_offset(&self) -> usize {
        self.absolute_offset
    }

    pub fn paragraph(&self) -> &str {
        &self.paragraph
    }

    pub fn offset_in_paragraph(&self) -> usize {
        self.offset_in_paragraph
    }
}

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
    connection: sqlez::connection::Connection,
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
            connection,
        })
    }

    pub fn id(&self) -> &CollectionId {
        &self.id
    }

    pub fn source(&self) -> &CollectionSource {
        &self.source
    }

    pub fn save_reading_position(
        &mut self,
        markdown: &str,
        absolute_offset: usize,
    ) -> anyhow::Result<ReadingPositionId> {
        anyhow::ensure!(
            absolute_offset <= markdown.len(),
            "reading position offset exceeds Markdown length"
        );
        anyhow::ensure!(
            markdown.is_char_boundary(absolute_offset),
            "reading position offset is not a UTF-8 boundary"
        );

        let paragraphs = markdown_paragraphs(markdown);
        let paragraph_index = paragraphs
            .iter()
            .position(|paragraph| {
                absolute_offset >= paragraph.start && absolute_offset <= paragraph.end
            })
            .ok_or_else(|| {
                anyhow::anyhow!("reading position is not inside a Markdown paragraph")
            })?;
        let paragraph = &paragraphs[paragraph_index];
        let position_id = uuid::Uuid::new_v4().to_string();
        let previous = paragraph_index
            .checked_sub(1)
            .map(|index| paragraphs[index].text.clone());
        let next = paragraphs
            .get(paragraph_index + 1)
            .map(|paragraph| paragraph.text.clone());

        self.connection
            .with_savepoint("save_reading_position", || {
                self.connection.exec_bound(
                    r#"
                    INSERT INTO reading_positions (
                        id,
                        source_id,
                        prior_absolute_offset,
                        paragraph_snapshot,
                        intra_paragraph_offset,
                        previous_paragraph_snapshot,
                        next_paragraph_snapshot,
                        resolution_status,
                        prior_document_length
                    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                "#,
                )?((
                    position_id.clone(),
                    self.source.id.0.clone(),
                    absolute_offset,
                    paragraph.text.clone(),
                    absolute_offset - paragraph.start,
                    previous,
                    next,
                    "Exact",
                    markdown.len(),
                ))?;
                self.connection.exec_bound(
                    r#"
                        INSERT INTO current_reading_positions (source_id, position_id)
                        VALUES (?, ?)
                        ON CONFLICT(source_id) DO UPDATE SET position_id = excluded.position_id
                    "#,
                )?((self.source.id.0.clone(), position_id.clone()))
            })?;

        Ok(ReadingPositionId(position_id))
    }

    pub fn resolve_reading_position(
        &self,
        position: &ReadingPositionId,
        markdown: &str,
    ) -> anyhow::Result<ResolvedReadingPosition> {
        self.connection
            .with_savepoint("resolve_reading_position", || {
                let (
                    prior_absolute_offset,
                    snapshot,
                    saved_intra_offset,
                    saved_previous,
                    saved_next,
                    prior_document_length,
                ) = self.connection.select_row_bound::<(String, String), (
                    usize,
                    String,
                    usize,
                    Option<String>,
                    Option<String>,
                    usize,
                )>(
                    r#"
                        SELECT
                            prior_absolute_offset,
                            paragraph_snapshot,
                            intra_paragraph_offset,
                            previous_paragraph_snapshot,
                            next_paragraph_snapshot,
                            prior_document_length
                        FROM reading_positions
                        WHERE id = ? AND source_id = ?
                    "#,
                )?((position.0.clone(), self.source.id.0.clone()))?
                .ok_or_else(|| {
                    anyhow::anyhow!("reading position does not exist for this source")
                })?;

                anyhow::ensure!(
                    prior_document_length > 0,
                    "saved reading position has an invalid prior document length"
                );
                let prior_paragraph_start =
                    prior_absolute_offset
                        .checked_sub(saved_intra_offset)
                        .ok_or_else(|| anyhow::anyhow!("saved reading position is inconsistent"))?;
                let paragraphs = markdown_paragraphs(markdown);
                let has_exact_match = paragraphs
                    .iter()
                    .any(|paragraph| paragraph.text == snapshot);
                let mut matches = paragraphs
                    .iter()
                    .enumerate()
                    .filter(|(_, paragraph)| {
                        if has_exact_match {
                            paragraph.text == snapshot
                        } else {
                            normalized_whitespace_equal(&paragraph.text, &snapshot)
                        }
                    })
                    .map(|(index, paragraph)| {
                        let previous = index
                            .checked_sub(1)
                            .map(|previous_index| paragraphs[previous_index].text.as_str());
                        let next = paragraphs.get(index + 1).map(|next| next.text.as_str());
                        let context_matches = |current: Option<&str>, saved: Option<&str>| {
                            if has_exact_match {
                                current == saved
                            } else {
                                match (current, saved) {
                                    (Some(current), Some(saved)) => {
                                        normalized_whitespace_equal(current, saved)
                                    }
                                    (None, None) => true,
                                    _ => false,
                                }
                            }
                        };
                        let context_score =
                            usize::from(context_matches(previous, saved_previous.as_deref()))
                                + usize::from(context_matches(next, saved_next.as_deref()));
                        let proportional_distance = (paragraph.start as u128
                            * prior_document_length as u128)
                            .abs_diff(prior_paragraph_start as u128 * markdown.len() as u128);
                        (paragraph, context_score, proportional_distance)
                    })
                    .collect::<Vec<_>>();
                if matches.is_empty() {
                    let resolved = stale_reading_position(markdown, prior_absolute_offset);
                    self.connection.exec_bound(
                        "UPDATE reading_positions SET resolution_status = 'Stale' WHERE id = ?",
                    )?(position.0.clone())?;
                    return Ok(resolved);
                }
                matches.sort_by_key(|(paragraph, context_score, proportional_distance)| {
                    (
                        std::cmp::Reverse(*context_score),
                        *proportional_distance,
                        paragraph.start,
                    )
                });
                let paragraph = matches[0].0;
                let offset_in_paragraph = if has_exact_match {
                    clamp_to_utf8_boundary(&paragraph.text, saved_intra_offset)
                } else {
                    map_normalized_intra_offset(&snapshot, &paragraph.text, saved_intra_offset)?
                };
                let absolute_offset = paragraph.start + offset_in_paragraph;
                let status = if has_exact_match && absolute_offset == prior_absolute_offset {
                    ReadingPositionStatus::Exact
                } else {
                    ReadingPositionStatus::Relocated
                };
                let durable_status = match status {
                    ReadingPositionStatus::Exact => "Exact",
                    ReadingPositionStatus::Relocated => "Relocated",
                    ReadingPositionStatus::Stale => "Stale",
                };
                self.connection.exec_bound(
                    "UPDATE reading_positions SET resolution_status = ? WHERE id = ?",
                )?((durable_status, position.0.clone()))?;

                Ok(ResolvedReadingPosition {
                    status,
                    absolute_offset,
                    paragraph: paragraph.text.clone(),
                    offset_in_paragraph,
                })
            })
    }

    pub fn resolve_current_reading_position(
        &self,
        markdown: &str,
    ) -> anyhow::Result<Option<ResolvedReadingPosition>> {
        let position_id = self.connection.select_row_bound::<String, String>(
            "SELECT position_id FROM current_reading_positions WHERE source_id = ?",
        )?(self.source.id.0.clone())?;
        let Some(position_id) = position_id else {
            return Ok(None);
        };

        let target_exists = self.connection.select_row_bound::<(String, String), bool>(
            r#"
                    SELECT EXISTS (
                        SELECT 1 FROM reading_positions WHERE id = ? AND source_id = ?
                    )
                "#,
        )?((position_id.clone(), self.source.id.0.clone()))?
        .unwrap_or(false);
        anyhow::ensure!(
            target_exists,
            "current reading position points to missing position {position_id}"
        );

        self.resolve_reading_position(&ReadingPositionId(position_id), markdown)
            .map(Some)
    }
}

struct MarkdownParagraph {
    start: usize,
    end: usize,
    text: String,
}

fn markdown_paragraphs(markdown: &str) -> Vec<MarkdownParagraph> {
    let mut paragraphs = Vec::new();
    let mut paragraph_start = None;
    let mut paragraph_end = 0;
    let mut line_start = 0;

    for line in markdown.split_inclusive('\n') {
        let line_end = line_start + line.len();
        let content_end = line
            .strip_suffix('\n')
            .unwrap_or(line)
            .strip_suffix('\r')
            .map_or(line_end - usize::from(line.ends_with('\n')), |content| {
                line_start + content.len()
            });
        let is_blank = markdown[line_start..content_end].trim().is_empty();
        if is_blank {
            if let Some(start) = paragraph_start.take() {
                paragraphs.push(MarkdownParagraph {
                    start,
                    end: paragraph_end,
                    text: markdown[start..paragraph_end].to_string(),
                });
            }
        } else {
            paragraph_start.get_or_insert(line_start);
            paragraph_end = content_end;
        }
        line_start = line_end;
    }

    if let Some(start) = paragraph_start {
        paragraphs.push(MarkdownParagraph {
            start,
            end: paragraph_end,
            text: markdown[start..paragraph_end].to_string(),
        });
    }

    paragraphs
}

fn clamp_to_utf8_boundary(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

fn stale_reading_position(markdown: &str, prior_absolute_offset: usize) -> ResolvedReadingPosition {
    let fallback_offset = clamp_to_utf8_boundary(markdown, prior_absolute_offset);
    let paragraphs = markdown_paragraphs(markdown);
    let Some(paragraph) = paragraphs.iter().min_by_key(|paragraph| {
        let distance = if fallback_offset < paragraph.start {
            paragraph.start - fallback_offset
        } else {
            fallback_offset.saturating_sub(paragraph.end)
        };
        (distance, paragraph.start)
    }) else {
        return ResolvedReadingPosition {
            status: ReadingPositionStatus::Stale,
            absolute_offset: fallback_offset,
            paragraph: String::new(),
            offset_in_paragraph: 0,
        };
    };

    let absolute_offset = fallback_offset.clamp(paragraph.start, paragraph.end);
    ResolvedReadingPosition {
        status: ReadingPositionStatus::Stale,
        absolute_offset,
        paragraph: paragraph.text.clone(),
        offset_in_paragraph: absolute_offset - paragraph.start,
    }
}

fn normalized_whitespace_equal(left: &str, right: &str) -> bool {
    left.split_whitespace().eq(right.split_whitespace())
}

fn token_spans(text: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut token_start = None;
    for (offset, character) in text.char_indices() {
        if character.is_whitespace() {
            if let Some(start) = token_start.take() {
                spans.push((start, offset));
            }
        } else {
            token_start.get_or_insert(offset);
        }
    }
    if let Some(start) = token_start {
        spans.push((start, text.len()));
    }
    spans
}

fn map_normalized_intra_offset(
    saved: &str,
    current: &str,
    saved_offset: usize,
) -> anyhow::Result<usize> {
    let saved_offset = clamp_to_utf8_boundary(saved, saved_offset);
    let saved_tokens = token_spans(saved);
    let current_tokens = token_spans(current);
    anyhow::ensure!(
        saved_tokens.len() == current_tokens.len(),
        "normalized paragraph token counts differ"
    );

    if let Some((token_index, (saved_start, _))) = saved_tokens
        .iter()
        .enumerate()
        .find(|(_, (start, end))| saved_offset >= *start && saved_offset < *end)
    {
        let character_offset = saved[*saved_start..saved_offset].chars().count();
        let (current_start, current_end) = current_tokens[token_index];
        let within_current = current[current_start..current_end]
            .char_indices()
            .nth(character_offset)
            .map_or(current_end - current_start, |(offset, _)| offset);
        return Ok(current_start + within_current);
    }

    Ok(saved_tokens
        .iter()
        .enumerate()
        .find(|(_, (start, _))| *start >= saved_offset)
        .map_or(current.len(), |(index, _)| current_tokens[index].0))
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
            r#"
                CREATE TABLE reading_positions (
                    id TEXT PRIMARY KEY,
                    source_id TEXT NOT NULL REFERENCES sources(id),
                    prior_absolute_offset INTEGER NOT NULL,
                    paragraph_snapshot TEXT NOT NULL,
                    intra_paragraph_offset INTEGER NOT NULL,
                    previous_paragraph_snapshot TEXT,
                    next_paragraph_snapshot TEXT,
                    resolution_status TEXT NOT NULL CHECK (
                        resolution_status IN ('Exact', 'Relocated', 'Stale')
                    )
                );
                PRAGMA user_version = 3;
            "#,
            r#"
                ALTER TABLE reading_positions
                    ADD COLUMN prior_document_length INTEGER NOT NULL DEFAULT 1;
                UPDATE reading_positions
                    SET prior_document_length = MAX(prior_absolute_offset + 1, 1);
                PRAGMA user_version = 4;
            "#,
            r#"
                CREATE TABLE current_reading_positions (
                    source_id TEXT PRIMARY KEY REFERENCES sources(id),
                    position_id TEXT NOT NULL UNIQUE REFERENCES reading_positions(id)
                );
                PRAGMA user_version = 5;
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

    #[test]
    fn reading_position_relocates_with_its_paragraph_after_external_edits() {
        let collection_root = tempfile::tempdir().expect("temporary Collection should be created");
        let source = collection_root.path().join("source.md");
        let target = "A distinctive paragraph carries durable attention.";
        let original_markdown =
            format!("# Source\n\nOpening paragraph.\n\n{target}\n\nEnding paragraph.");
        std::fs::write(&source, &original_markdown).expect("source Markdown should be written");
        let offset_in_paragraph = 16;
        let original_offset = original_markdown
            .find(target)
            .expect("target paragraph should exist")
            + offset_in_paragraph;

        let mut collection = super::WorkbenchCollection::open(collection_root.path())
            .expect("portable Collection should open");
        let position = collection
            .save_reading_position(&original_markdown, original_offset)
            .expect("reading position should be saved");
        drop(collection);

        let relocated_markdown = format!(
            "# Inserted material\n\nFirst external paragraph.\n\nSecond external paragraph.\n\n{original_markdown}"
        );
        std::fs::write(&source, &relocated_markdown)
            .expect("external Markdown edit should be written");
        let collection = super::WorkbenchCollection::open(collection_root.path())
            .expect("Collection should reopen after an external edit");
        let resolved = collection
            .resolve_reading_position(&position, &relocated_markdown)
            .expect("saved reading position should resolve");

        assert_eq!(resolved.status(), super::ReadingPositionStatus::Relocated);
        assert_eq!(resolved.paragraph(), target);
        assert_eq!(resolved.offset_in_paragraph(), offset_in_paragraph);
        assert_eq!(
            resolved.absolute_offset(),
            relocated_markdown
                .find(target)
                .expect("target paragraph should remain")
                + offset_in_paragraph
        );
        assert_ne!(resolved.absolute_offset(), original_offset);
    }

    #[test]
    fn reading_position_relocates_to_the_intended_duplicate_paragraph() {
        let collection_root = tempfile::tempdir().expect("temporary Collection should be created");
        let source = collection_root.path().join("source.md");
        let target = "Repeated paragraph with the same visible text.";
        let original_markdown = format!(
            "# Source\n\nContext before the first copy.\n\n{target}\n\nContext after the first copy.\n\nBridge paragraph.\n\nContext before the intended copy.\n\n{target}\n\nContext after the intended copy."
        );
        std::fs::write(&source, &original_markdown).expect("source Markdown should be written");
        let offset_in_paragraph = 9;
        let intended_original_offset = original_markdown
            .rfind(target)
            .expect("intended duplicate should exist")
            + offset_in_paragraph;

        let mut collection = super::WorkbenchCollection::open(collection_root.path())
            .expect("portable Collection should open");
        let position = collection
            .save_reading_position(&original_markdown, intended_original_offset)
            .expect("position inside the intended duplicate should be saved");
        drop(collection);

        let relocated_markdown = format!(
            "# Externally inserted material\n\nAnother preceding paragraph.\n\n{original_markdown}"
        );
        std::fs::write(&source, &relocated_markdown)
            .expect("external Markdown edit should be written");
        let first_duplicate = relocated_markdown
            .find(target)
            .expect("first duplicate should remain");
        let intended_duplicate = relocated_markdown
            .rfind(target)
            .expect("intended duplicate should remain");
        assert_ne!(first_duplicate, intended_duplicate);

        let collection = super::WorkbenchCollection::open(collection_root.path())
            .expect("Collection should reopen after external edits");
        let resolved = collection
            .resolve_reading_position(&position, &relocated_markdown)
            .expect("context should resolve the intended duplicate deterministically");

        assert_eq!(resolved.status(), super::ReadingPositionStatus::Relocated);
        assert_eq!(resolved.paragraph(), target);
        assert_eq!(resolved.offset_in_paragraph(), offset_in_paragraph);
        assert_eq!(
            resolved.absolute_offset(),
            intended_duplicate + offset_in_paragraph
        );
        assert_ne!(
            resolved.absolute_offset(),
            first_duplicate + offset_in_paragraph
        );
    }

    #[test]
    fn reading_position_relocates_across_normalized_whitespace_reflow() {
        let collection_root = tempfile::tempdir().expect("temporary Collection should be created");
        let source = collection_root.path().join("source.md");
        let distinctive_word = "mnemonic";
        let original_paragraph =
            "Learning   requires\tcareful attention to mnemonic structure in durable material.";
        let reflowed_paragraph =
            "Learning requires\ncareful   attention to\tmnemonic structure in durable material.";
        assert_ne!(original_paragraph, reflowed_paragraph);
        assert_eq!(
            original_paragraph.split_whitespace().collect::<Vec<_>>(),
            reflowed_paragraph.split_whitespace().collect::<Vec<_>>()
        );

        let original_markdown =
            format!("# Source\n\nOpening context.\n\n{original_paragraph}\n\nEnding context.");
        std::fs::write(&source, &original_markdown).expect("source Markdown should be written");
        let original_offset = original_markdown
            .find(distinctive_word)
            .expect("distinctive word should exist");
        let mut collection = super::WorkbenchCollection::open(collection_root.path())
            .expect("portable Collection should open");
        let position = collection
            .save_reading_position(&original_markdown, original_offset)
            .expect("position at the distinctive word should be saved");
        drop(collection);

        let relocated_markdown = format!(
            "# Inserted material\n\nA new preceding paragraph.\n\n# Source\n\nOpening context.\n\n{reflowed_paragraph}\n\nEnding context."
        );
        std::fs::write(&source, &relocated_markdown)
            .expect("externally reflowed Markdown should be written");
        let expected_absolute_offset = relocated_markdown
            .find(distinctive_word)
            .expect("distinctive word should remain after reflow");
        let expected_paragraph_start = relocated_markdown
            .find(reflowed_paragraph)
            .expect("reflowed paragraph should exist");

        let collection = super::WorkbenchCollection::open(collection_root.path())
            .expect("Collection should reopen after reflow");
        let resolved = collection
            .resolve_reading_position(&position, &relocated_markdown)
            .expect("normalized whitespace should preserve the semantic position");

        assert_eq!(resolved.status(), super::ReadingPositionStatus::Relocated);
        assert_eq!(resolved.paragraph(), reflowed_paragraph);
        assert_eq!(resolved.absolute_offset(), expected_absolute_offset);
        assert_eq!(
            resolved.offset_in_paragraph(),
            expected_absolute_offset - expected_paragraph_start
        );
    }

    #[test]
    fn missing_paragraph_resolves_to_an_explicit_stale_fallback() {
        let collection_root = tempfile::tempdir().expect("temporary Collection should be created");
        let source = collection_root.path().join("source.md");
        let deleted_paragraph =
            "This late paragraph will be completely removed from the external rewrite.";
        let original_markdown = format!(
            "# Original source\n\nSeveral earlier paragraphs make this document long.\n\nAnother substantial paragraph precedes the target.\n\n{deleted_paragraph}"
        );
        std::fs::write(&source, &original_markdown).expect("source Markdown should be written");
        let original_offset = original_markdown
            .find("completely")
            .expect("position word should exist");
        let mut collection = super::WorkbenchCollection::open(collection_root.path())
            .expect("portable Collection should open");
        let position = collection
            .save_reading_position(&original_markdown, original_offset)
            .expect("late reading position should be saved");
        drop(collection);

        let unrelated_markdown = "# New\n\nCafé";
        assert!(original_offset > unrelated_markdown.len());
        assert!(unrelated_markdown.is_char_boundary(unrelated_markdown.len()));
        std::fs::write(&source, unrelated_markdown)
            .expect("unrelated replacement Markdown should be written");
        let collection = super::WorkbenchCollection::open(collection_root.path())
            .expect("Collection should reopen after replacement");
        let resolved = collection
            .resolve_reading_position(&position, unrelated_markdown)
            .expect("missing paragraph should produce a usable stale fallback");

        assert_eq!(resolved.status(), super::ReadingPositionStatus::Stale);
        assert_ne!(resolved.status(), super::ReadingPositionStatus::Exact);
        assert_ne!(resolved.status(), super::ReadingPositionStatus::Relocated);
        assert_eq!(resolved.absolute_offset(), unrelated_markdown.len());
        assert!(unrelated_markdown.is_char_boundary(resolved.absolute_offset()));
        assert_eq!(resolved.paragraph(), "Café");
        assert_eq!(resolved.offset_in_paragraph(), "Café".len());
        assert_ne!(resolved.paragraph(), deleted_paragraph);
    }

    #[test]
    fn current_reading_position_restores_without_a_session_held_id() {
        let collection_root = tempfile::tempdir().expect("temporary Collection should be created");
        let source = collection_root.path().join("source.md");
        let target = "A durable paragraph restores without UI session identity.";
        let markdown = format!("# Source\n\nOpening context.\n\n{target}\n\nEnding context.");
        std::fs::write(&source, &markdown).expect("source Markdown should be written");
        let offset_in_paragraph = 10;
        let absolute_offset = markdown
            .find(target)
            .expect("target paragraph should exist")
            + offset_in_paragraph;

        let mut collection = super::WorkbenchCollection::open(collection_root.path())
            .expect("portable Collection should open");
        collection
            .save_reading_position(&markdown, absolute_offset)
            .expect("current reading position should be saved");
        drop(collection);

        let collection = super::WorkbenchCollection::open(collection_root.path())
            .expect("Collection should reopen without UI session state");
        let resolved = collection
            .resolve_current_reading_position(&markdown)
            .expect("durable current position should resolve")
            .expect("a saved current position should exist");

        assert_eq!(resolved.status(), super::ReadingPositionStatus::Exact);
        assert_eq!(resolved.paragraph(), target);
        assert_eq!(resolved.offset_in_paragraph(), offset_in_paragraph);
        assert_eq!(resolved.absolute_offset(), absolute_offset);
    }
}
