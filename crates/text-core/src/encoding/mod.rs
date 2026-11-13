//! Automatic detection, explicit display decoding and strict save conversion.
pub use crate::format::{Charset, DecodedText, EncodingSettings, EncodingSource, TextFormat};
use encoding_rs::Encoding;
mod suggestion;
pub use self::suggestion::suggest;

fn codec(charset: Charset) -> &'static Encoding {
    match charset {
        Charset::Utf8 => encoding_rs::UTF_8,
        Charset::Utf16Le => encoding_rs::UTF_16LE,
        Charset::Utf16Be => encoding_rs::UTF_16BE,
        Charset::Gbk => encoding_rs::GBK,
        Charset::Gb18030 => encoding_rs::GB18030,
        Charset::Big5 => encoding_rs::BIG5,
        Charset::ShiftJis => encoding_rs::SHIFT_JIS,
        Charset::EucJp => encoding_rs::EUC_JP,
        Charset::EucKr => encoding_rs::EUC_KR,
        Charset::Windows1252 => encoding_rs::WINDOWS_1252,
    }
}
pub fn decode(
    bytes: &[u8],
    explicit: Option<TextFormat>,
    options: &EncodingSettings,
    limit: usize,
) -> Result<DecodedText, String> {
    if let Some(format) = explicit {
        return decode_explicit(bytes, format, limit);
    }
    use crate::format::Charset::*;
    let bom = if bytes.starts_with(&[0xff, 0xfe, 0, 0]) || bytes.starts_with(&[0, 0, 0xfe, 0xff]) {
        return Err("UTF-32 is not supported. Choose a supported text encoding.".into());
    } else if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
        Some((TextFormat::new(Utf8, true), 3))
    } else if bytes.starts_with(&[0xff, 0xfe]) {
        Some((TextFormat::new(Utf16Le, true), 2))
    } else if bytes.starts_with(&[0xfe, 0xff]) {
        Some((TextFormat::new(Utf16Be, true), 2))
    } else {
        None
    };
    let (format, source) = if let Some((format, _)) = bom {
        (format, EncodingSource::Bom)
    } else {
        // UTF-16 without BOM: only accept a strong alternating-zero pattern.
        let sample = &bytes[..bytes.len().min(8192)];
        let pairs = sample.len() / 2;
        let even = sample.iter().step_by(2).filter(|b| **b == 0).count();
        let odd = sample
            .iter()
            .skip(1)
            .step_by(2)
            .filter(|b| **b == 0)
            .count();
        if pairs >= 2 && odd * 5 >= pairs * 3 && even * 10 < pairs {
            (TextFormat::new(Utf16Le, false), EncodingSource::Detected)
        } else if pairs >= 2 && even * 5 >= pairs * 3 && odd * 10 < pairs {
            (TextFormat::new(Utf16Be, false), EncodingSource::Detected)
        } else {
            if bytes.contains(&0) {
                return Err("Binary file".into());
            }
            if std::str::from_utf8(bytes).is_ok() || !options.auto_detect {
                (TextFormat::default(), EncodingSource::Utf8)
            } else {
                let mut detector = chardetng::EncodingDetector::new();
                detector.feed(bytes, true);
                let guessed = detector.guess(None, true);
                let charset = options
                    .candidates
                    .iter()
                    .copied()
                    .find(|c| codec(*c) == guessed);
                match charset {
                    Some(charset) => (TextFormat::new(charset, false), EncodingSource::Detected),
                    None => (TextFormat::default(), EncodingSource::Utf8),
                }
            }
        }
    };
    let mut decoded = decode_explicit(bytes, format, limit)?;
    let text = &decoded.text;
    if text.contains('\0')
        || text
            .chars()
            .take(8192)
            .filter(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t' | '\u{c}'))
            .count()
            > text.chars().take(8192).count() / 20
    {
        return Err("Binary file".into());
    }
    decoded.source = source;
    Ok(decoded)
}

/// Honor a manual choice, including malformed sequences and binary-looking text.
/// Replacement characters are display results; the original bytes stay available.
pub fn decode_explicit(
    bytes: &[u8],
    format: TextFormat,
    limit: usize,
) -> Result<DecodedText, String> {
    format.validate()?;
    let signature: &[u8] = match format.charset {
        Charset::Utf8 => &[0xef, 0xbb, 0xbf],
        Charset::Utf16Le => &[0xff, 0xfe],
        Charset::Utf16Be => &[0xfe, 0xff],
        _ => &[],
    };
    let body = if !signature.is_empty() && bytes.starts_with(signature) {
        &bytes[signature.len()..]
    } else {
        bytes
    };
    let mut decoder = codec(format.charset).new_decoder_without_bom_handling();
    let mut text = String::new();
    let mut consumed = 0;
    loop {
        let remaining = limit.saturating_sub(text.len());
        // A small output buffer allows decoding the final incomplete sequence,
        // while checking the actual UTF-8 size before accepting it.
        text.reserve(remaining.clamp(4, 8192));
        let (result, read, _) = decoder.decode_to_string(&body[consumed..], &mut text, true);
        consumed += read;
        if text.len() > limit {
            return Err("Decoded text exceeds the file size limit".into());
        }
        if result == encoding_rs::CoderResult::InputEmpty {
            break;
        }
    }
    Ok(DecodedText {
        text,
        format,
        source: EncodingSource::Explicit,
        bytes: std::sync::Arc::new(bytes.to_vec()),
    })
}

pub fn encode(text: &str, format: TextFormat) -> Result<Vec<u8>, String> {
    format.validate()?;
    let mut out = Vec::new();
    if format.bom {
        out.extend_from_slice(match format.charset {
            Charset::Utf8 => &[0xef, 0xbb, 0xbf],
            Charset::Utf16Le => &[0xff, 0xfe],
            Charset::Utf16Be => &[0xfe, 0xff],
            _ => unreachable!(),
        });
    }
    match format.charset {
        Charset::Utf8 => out.extend_from_slice(text.as_bytes()),
        Charset::Utf16Le | Charset::Utf16Be => {
            for value in text.encode_utf16() {
                out.extend_from_slice(&if format.charset == Charset::Utf16Le {
                    value.to_le_bytes()
                } else {
                    value.to_be_bytes()
                });
            }
        }
        _ => {
            let (bytes, _, errors) = codec(format.charset).encode(text);
            if errors {
                for (index, ch) in text.char_indices() {
                    let mut buffer = [0; 4];
                    if codec(format.charset).encode(ch.encode_utf8(&mut buffer)).2 {
                        let line = text[..index].bytes().filter(|b| *b == b'\n').count() + 1;
                        let column = text[..index]
                            .rsplit('\n')
                            .next()
                            .unwrap_or("")
                            .chars()
                            .count()
                            + 1;
                        return Err(format!(
                            "{} cannot represent '{}' (U+{:04X}) at line {line}, column {column}. Choose UTF-8 or another encoding.",
                            format.label(),
                            ch,
                            ch as u32
                        ));
                    }
                }
                return Err(format!("Text cannot be represented as {}", format.label()));
            }
            out.extend_from_slice(&bytes);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
