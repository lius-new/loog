use crate::encoding::*;
#[test]
fn encoding_every_supported_format_round_trips_and_preserves_crlf() {
    for format in TextFormat::choices() {
        let value = match format.charset {
            Charset::Gbk | Charset::Gb18030 | Charset::Big5 => "中文測試\r\nhello €\r\n",
            Charset::ShiftJis | Charset::EucJp => "日本語の文字コードです\r\nhello\r\n",
            Charset::EucKr => "한국어 텍스트입니다\r\nhello\r\n",
            Charset::Windows1252 => "café déjà vu €\r\nhello\r\n",
            _ => "中文 😀 café\r\nhello\r\n",
        };
        let bytes = encode(value, format).unwrap_or_else(|e| panic!("{}: {e}", format.label()));
        let decoded = decode(&bytes, Some(format), &EncodingSettings::default(), 10000).unwrap();
        assert_eq!(decoded.text, value, "{}", format.label());
        assert_eq!(decoded.format, format);
        assert_eq!(encode(&decoded.text, decoded.format).unwrap(), bytes);
    }
}
#[test]
fn encoding_auto_detection_handles_unicode_and_chinese() {
    let value = "这是一个用于验证编码识别的简体中文文件，包含常用汉字。\r\n文件应当能够正常打开和保存。\r\n";
    for format in TextFormat::choices().into_iter().filter(|f| {
        f.bom
            || f.charset == Charset::Utf8
            || f.charset == Charset::Gbk
            || f.charset == Charset::Gb18030
    }) {
        let bytes = encode(value, format).unwrap();
        let decoded = decode(&bytes, None, &EncodingSettings::default(), 10000)
            .unwrap_or_else(|e| panic!("{}: {e}", format.label()));
        assert_eq!(
            decoded.text,
            value,
            "{} detected as {}",
            format.label(),
            decoded.format.label()
        );
    }
}
#[test]
fn encoding_save_is_strict_while_malformed_text_remains_openable() {
    let gbk = TextFormat::new(Charset::Gbk, false);
    let error = encode("第一行\nhello 😀", gbk).unwrap_err();
    assert!(
        error.contains("U+1F600") && error.contains("line 2, column 7"),
        "{error}"
    );
    assert_eq!(
        decode(
            &[0xff, 0xfe, 0x00, 0xd8],
            None,
            &EncodingSettings::default(),
            100
        )
        .unwrap()
        .text,
        "\u{fffd}"
    );
    assert!(decode(b"a\0b", None, &EncodingSettings::default(), 100).is_err());
    assert!(
        decode(&[0xff, 0xfe, 0, 0], None, &EncodingSettings::default(), 100)
            .unwrap_err()
            .contains("UTF-32")
    );
    assert!(encode("x", TextFormat::new(Charset::Gbk, true)).is_err());
}
#[test]
fn encoding_limits_apply_to_decoded_size_and_utf8_bom_is_not_text() {
    let format = TextFormat::new(Charset::Utf16Le, true);
    let bytes = encode("中文中文", format).unwrap();
    assert!(decode(&bytes, None, &EncodingSettings::default(), 10).is_err());
    let text = decode(b"\xef\xbb\xbfhello", None, &EncodingSettings::default(), 5).unwrap();
    assert_eq!(text.text, "hello");
    assert!(text.format.bom);
    let settings = EncodingSettings {
        auto_detect: false,
        ..Default::default()
    };
    let bytes = encode("中文", TextFormat::new(Charset::Gbk, false)).unwrap();
    let fallback = decode(&bytes, None, &settings, 100).unwrap();
    assert_eq!(fallback.format, TextFormat::default());
    assert_ne!(fallback.text, "中文");
    assert_eq!(
        decode(
            &bytes,
            Some(TextFormat::new(Charset::Gbk, false)),
            &settings,
            100
        )
        .unwrap()
        .text,
        "中文"
    );
}

#[test]
fn encoding_manual_decoding_keeps_the_choice_even_when_bytes_are_invalid() {
    let original = encode("中文", TextFormat::new(Charset::Gbk, false)).unwrap();
    for format in TextFormat::choices() {
        let decoded = decode_explicit(&original, format, 1000).unwrap();
        assert_eq!(decoded.format, format);
        assert_eq!(decoded.source, EncodingSource::Explicit);
        assert_eq!(*decoded.bytes, original);
    }
    let wrong = decode_explicit(&original, TextFormat::default(), 1000).unwrap();
    assert!(wrong.text.contains('\u{fffd}'));
    assert!(decode_explicit(&original, TextFormat::default(), 1).is_err());
    let binary_looking = decode_explicit(b"a\0b", TextFormat::default(), 100).unwrap();
    assert_eq!(binary_looking.text, "a\0b");
    let with_bom = TextFormat::new(Charset::Utf8, true);
    assert_eq!(
        decode_explicit(b"hello", with_bom, 5).unwrap().format,
        with_bom
    );
    assert_eq!(
        decode_explicit(b"\xef\xbb\xbfhello", TextFormat::default(), 5)
            .unwrap()
            .text,
        "hello"
    );
}

#[test]
fn encoding_suggestions_use_byte_evidence_and_preserve_bom_variants() {
    let options = EncodingSettings::default();
    let text = "这是用于识别编码的中文内容，包含常见汉字。\r\n第二行也包含中文。\r\n";
    for format in [
        TextFormat::default(),
        TextFormat::new(Charset::Utf8, true),
        TextFormat::new(Charset::Utf16Le, true),
        TextFormat::new(Charset::Utf16Be, true),
        TextFormat::new(Charset::Gbk, false),
    ] {
        let bytes = encode(text, format).unwrap();
        assert_eq!(
            suggest(&bytes, &options),
            Some(format),
            "{}",
            format.label()
        );
    }
    for charset in [Charset::Utf16Le, Charset::Utf16Be] {
        let format = TextFormat::new(charset, false);
        assert_eq!(
            suggest(&encode("hello world\r\n", format).unwrap(), &options),
            Some(format)
        );
    }
}

#[test]
fn encoding_suggestions_are_omitted_for_ambiguous_or_invalid_input() {
    let options = EncodingSettings::default();
    for bytes in [
        b"".as_slice(),
        b"hello\r\n",
        b"a\0b",
        &[0xd6, 0xd0],
        &[0xff, 0xfe, 0, 0xd8],
        &[0xff, 0xfe, 0, 0],
    ] {
        assert_eq!(suggest(bytes, &options), None);
    }
    let restricted = EncodingSettings {
        candidates: vec![],
        ..options
    };
    let bytes = encode(
        "这是用于识别的中文内容，包含常见汉字。",
        TextFormat::new(Charset::Gbk, false),
    )
    .unwrap();
    assert_eq!(suggest(&bytes, &restricted), None);
}

#[test]
fn encoding_suggestions_handle_multibyte_sequences_at_the_sample_boundary() {
    let options = EncodingSettings::default();
    let bytes = "中".repeat(30_000).into_bytes();
    assert_eq!(suggest(&bytes, &options), Some(TextFormat::default()));
    let mut ambiguous = vec![b'a'; 64 * 1024 - 1];
    ambiguous.extend_from_slice("中".as_bytes());
    assert_eq!(suggest(&ambiguous, &options), None);
}
