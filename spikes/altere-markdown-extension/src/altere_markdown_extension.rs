use zed_extension_api as zed;

const LANGUAGE_SERVER_BINARY: &str = "altere-language-server";

struct AltereMarkdownExtension;

impl zed::Extension for AltereMarkdownExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        _language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> zed::Result<zed::Command> {
        let command = worktree.which(LANGUAGE_SERVER_BINARY).ok_or_else(|| {
            format!("'{LANGUAGE_SERVER_BINARY}' is not available in the Workbench PATH")
        })?;

        Ok(zed::Command {
            command,
            args: vec!["--stdio".to_string()],
            env: Default::default(),
        })
    }
}

zed::register_extension!(AltereMarkdownExtension);
