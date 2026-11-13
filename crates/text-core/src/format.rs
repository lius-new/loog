//! File byte formats. Editor text is Unicode; source bytes survive manual decoding.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Charset {
    #[default]
    #[serde(rename = "utf8", alias = "UTF-8")]
    Utf8,
    #[serde(rename = "utf16le", alias = "UTF-16LE")]
    Utf16Le,
    #[serde(rename = "utf16be", alias = "UTF-16BE")]
    Utf16Be,
    #[serde(rename = "gbk", alias = "cp936")]
    Gbk,
    #[serde(rename = "gb18030")]
    Gb18030,
    #[serde(rename = "big5")]
    Big5,
    #[serde(rename = "shift_jis")]
    ShiftJis,
    #[serde(rename = "euc-jp")]
    EucJp,
    #[serde(rename = "euc-kr")]
    EucKr,
    #[serde(rename = "windows-1252")]
    Windows1252,
}
impl Charset {
    pub fn name(self) -> &'static str {
        match self {
            Self::Utf8 => "UTF-8",
            Self::Utf16Le => "UTF-16LE",
            Self::Utf16Be => "UTF-16BE",
            Self::Gbk => "GBK",
            Self::Gb18030 => "gb18030",
            Self::Big5 => "Big5",
            Self::ShiftJis => "Shift_JIS",
            Self::EucJp => "EUC-JP",
            Self::EucKr => "EUC-KR",
            Self::Windows1252 => "windows-1252",
        }
    }
    pub fn unicode(self) -> bool {
        matches!(self, Self::Utf8 | Self::Utf16Le | Self::Utf16Be)
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextFormat {
    pub charset: Charset,
    #[serde(default)]
    pub bom: bool,
}
impl TextFormat {
    pub const fn new(charset: Charset, bom: bool) -> Self {
        Self { charset, bom }
    }
    pub fn code(self) -> u32 {
        self.charset as u32 * 2 + u32::from(self.bom)
    }
    pub fn from_code(code: u32) -> Self {
        let code = code & 255;
        use self::Charset::*;
        let charset = match code / 2 {
            0 => Utf8,
            1 => Utf16Le,
            2 => Utf16Be,
            3 => Gbk,
            4 => Gb18030,
            5 => Big5,
            6 => ShiftJis,
            7 => EucJp,
            8 => EucKr,
            9 => Windows1252,
            _ => Utf8,
        };
        Self::new(charset, code % 2 == 1 && charset.unicode())
    }
    pub fn label(self) -> String {
        format!(
            "{}{}",
            self.charset.name(),
            if self.bom { " BOM" } else { "" }
        )
    }
    pub fn validate(self) -> Result<(), String> {
        if self.bom && !self.charset.unicode() {
            Err("BOM is only supported for Unicode encodings".into())
        } else {
            Ok(())
        }
    }
    pub fn choices() -> Vec<Self> {
        use self::Charset::*;
        vec![
            Self::new(Utf8, false),
            Self::new(Utf8, true),
            Self::new(Utf16Le, true),
            Self::new(Utf16Be, true),
            Self::new(Utf16Le, false),
            Self::new(Utf16Be, false),
            Self::new(Gbk, false),
            Self::new(Gb18030, false),
            Self::new(Big5, false),
            Self::new(ShiftJis, false),
            Self::new(EucJp, false),
            Self::new(EucKr, false),
            Self::new(Windows1252, false),
        ]
    }
    pub fn search_label(self) -> String {
        let aliases = match self.charset {
            Charset::Gbk => " cp936 936 简体中文",
            Charset::Gb18030 => " 简体中文",
            Charset::Big5 => " 繁体中文",
            Charset::ShiftJis | Charset::EucJp => " 日文 Japanese",
            Charset::EucKr => " 韩文 Korean",
            _ => "",
        };
        format!("{}{aliases}", self.label())
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum EncodingSource {
    #[default]
    Utf8,
    Bom,
    Detected,
    Explicit,
}
#[derive(Clone, Debug)]
pub struct DecodedText {
    pub text: String,
    pub format: TextFormat,
    pub source: EncodingSource,
    pub bytes: std::sync::Arc<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EncodingSettings {
    pub new_file: TextFormat,
    pub auto_detect: bool,
    pub candidates: Vec<Charset>,
}
impl Default for EncodingSettings {
    fn default() -> Self {
        use self::Charset::*;
        Self {
            new_file: TextFormat::default(),
            auto_detect: true,
            candidates: vec![Gbk, Gb18030, Big5, ShiftJis, EucJp, EucKr, Windows1252],
        }
    }
}
