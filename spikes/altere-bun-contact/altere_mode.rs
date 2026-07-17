use std::ffi::OsStr;

#[derive(Clone, Copy)]
pub struct AltereMode {
    enabled: bool,
}

impl AltereMode {
    pub fn from_env() -> Self {
        Self::from_value(std::env::var_os("ALTERE_MODE").as_deref())
    }

    pub fn from_value(value: Option<&OsStr>) -> Self {
        Self {
            enabled: value == Some(OsStr::new("1")),
        }
    }

    pub fn show_next_button(self) -> bool {
        self.enabled
    }

    pub fn show_debugger(self) -> bool {
        !self.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_is_reversible() {
        let upstream = AltereMode::from_value(None);
        assert!(!upstream.show_next_button());
        assert!(upstream.show_debugger());

        let altere = AltereMode::from_value(Some(std::ffi::OsStr::new("1")));
        assert!(altere.show_next_button());
        assert!(!altere.show_debugger());
    }
}
