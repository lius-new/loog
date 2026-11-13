//! Conservative, bounded hints. Suggestions never change the chosen format.
use super::codec;
use crate::format::{Charset, EncodingSettings, TextFormat};

const SAMPLE_BYTES: usize = 64 * 1024;

pub fn suggest(bytes: &[u8], options: &EncodingSettings) -> Option<TextFormat> {
    use crate::format::Charset::*;
    if bytes.is_empty()
        || bytes.starts_with(&[0xff, 0xfe, 0, 0])
        || bytes.starts_with(&[0, 0, 0xfe, 0xff])
    {
        return None;
    }
    for (signature, charset) in [
        (&[0xef, 0xbb, 0xbf][..], Utf8),
        (&[0xff, 0xfe][..], Utf16Le),
        (&[0xfe, 0xff][..], Utf16Be),
    ] {
        if bytes.starts_with(signature) {
            let format = TextFormat::new(charset, true);
            return plausible_text(bytes, format).map(|_| format);
        }
    }

    let sample = &bytes[..bytes.len().min(SAMPLE_BYTES)];
    let pairs = sample.len() / 2;
    let even = sample.iter().step_by(2).filter(|b| **b == 0).count();
    let odd = sample
        .iter()
        .skip(1)
        .step_by(2)
        .filter(|b| **b == 0)
        .count();
    let utf16 = if pairs >= 4 && odd * 5 >= pairs * 3 && even * 10 < pairs {
        Some(Utf16Le)
    } else if pairs >= 4 && even * 5 >= pairs * 3 && odd * 10 < pairs {
        Some(Utf16Be)
    } else {
        None
    };
    if let Some(charset) = utf16 {
        let format = TextFormat::new(charset, false);
        return plausible_text(bytes, format).map(|_| format);
    }
    // ASCII fits many encodings equally well; binary-looking bytes are no hint.
    if sample.is_ascii() || sample.contains(&0) {
        return None;
    }
    let utf8 = TextFormat::default();
    if plausible_text(bytes, utf8).is_some_and(|text| !text.is_ascii()) {
        return Some(utf8);
    }
    // Statistical guesses on a handful of non-ASCII bytes are too weak to show.
    if sample.iter().filter(|byte| !byte.is_ascii()).count() < 8 {
        return None;
    }
    let mut detector = chardetng::EncodingDetector::new();
    detector.feed(sample, sample.len() == bytes.len());
    let (guessed, assessed) = detector.guess_assess(None, true);
    if !assessed {
        return None;
    }
    let charset = if guessed == encoding_rs::GBK {
        let four_byte = sample.windows(4).any(|part| {
            (0x81..=0xfe).contains(&part[0])
                && (0x30..=0x39).contains(&part[1])
                && (0x81..=0xfe).contains(&part[2])
                && (0x30..=0x39).contains(&part[3])
        });
        [Gbk, Gb18030].into_iter().find(|charset| {
            (!four_byte || *charset == Gb18030) && options.candidates.contains(charset)
        })?
    } else {
        options
            .candidates
            .iter()
            .copied()
            .find(|c| codec(*c) == guessed)?
    };
    let format = TextFormat::new(charset, false);
    plausible_text(bytes, format).map(|_| format)
}

fn plausible_text(bytes: &[u8], format: TextFormat) -> Option<String> {
    let skip = if format.bom {
        if format.charset == Charset::Utf8 {
            3
        } else {
            2
        }
    } else {
        0
    };
    let body = &bytes[skip..];
    let sample = &body[..body.len().min(SAMPLE_BYTES - skip)];
    let mut decoder = codec(format.charset).new_decoder_without_bom_handling();
    let capacity = decoder.max_utf8_buffer_length_without_replacement(sample.len())?;
    let mut text = String::with_capacity(capacity);
    let (result, read) =
        decoder.decode_to_string_without_replacement(sample, &mut text, sample.len() == body.len());
    if !matches!(result, encoding_rs::DecoderResult::InputEmpty) || read != sample.len() {
        return None;
    }
    if text
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t' | '\u{c}'))
    {
        return None;
    }
    Some(text)
}
