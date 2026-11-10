//! Per-document editing preferences; line endings also participate in history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    Crlf,
}
impl LineEnding {
    pub fn text(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::Crlf => "\r\n",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Lf => "LF",
            Self::Crlf => "CRLF",
        }
    }
    pub fn code(self) -> u32 {
        match self {
            Self::Lf => 1 << 8,
            Self::Crlf => 2 << 8,
        }
    }
    pub fn from_code(code: u32) -> Option<Self> {
        match (code >> 8) & 3 {
            1 => Some(Self::Lf),
            2 => Some(Self::Crlf),
            _ => None,
        }
    }
}
pub const ENDING_MASK: u32 = 3 << 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditingOptions {
    pub soft_wrap: bool,
    pub match_brackets: bool,
    pub indent_guides: bool,
    pub show_whitespace: bool,
    pub language: std::sync::Arc<str>,
    pub indent_size: usize,
    pub tab_width: usize,
    pub use_tabs: bool,
    pub auto_pairs: bool,
}
impl Default for EditingOptions {
    fn default() -> Self {
        Self {
            soft_wrap: false,
            match_brackets: true,
            indent_guides: true,
            show_whitespace: false,
            language: "text".into(),
            indent_size: 2,
            tab_width: 4,
            use_tabs: false,
            auto_pairs: true,
        }
    }
}
impl EditingOptions {
    pub fn clamped(self) -> Self {
        Self {
            indent_size: self.indent_size.clamp(1, 16),
            tab_width: self.tab_width.clamp(1, 16),
            ..self
        }
    }
    pub fn indent(&self) -> String {
        if self.use_tabs {
            "\t".into()
        } else {
            " ".repeat(self.indent_size.clamp(1, 16))
        }
    }
}

/// Normalize inserted text, preserving whether it has a final newline.
pub fn normalize_endings(text: &str, ending: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\n', ending)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FileOptions {
    pub indent_size_from_tab: bool,
    pub indent_size: Option<usize>,
    pub tab_width: Option<usize>,
    pub use_tabs: Option<bool>,
    pub line_ending: Option<LineEnding>,
}
