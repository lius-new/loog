use super::{layout::*, model::*, parser::parse_document, reader::*};
use std::sync::Arc;
fn parse(source: &str) -> MarkdownDocument {
    parse_document(
        Arc::new(source.to_owned()),
        ContentFormat::Markdown,
        SyntaxOptions::default(),
        ParseLimits::automatic(),
        || false,
    )
    .unwrap()
}
#[test]
fn projection_keeps_unicode_html_and_complete_copy() {
    let source="# 中文 e\u{301} 😀\r\n\r\nA &amp; **bold** and \\*literal\\*.\r\n\r\n<pre>literal</pre>\n\n- [x] first\n- second\n\n|a|b|\n|-|-|\n|中|😀|\n".replace("\\u{301}","\u{301}");
    let doc = parse(&source);
    assert!(doc.display_text.contains("A & bold and *literal*."));
    assert!(doc.display_text.contains("<pre>literal</pre>"));
    assert!(doc.display_text.contains("a\tb\n"));
    assert!(doc.display_text.contains("中\t😀"));
    assert_eq!(doc.headings.len(), 1);
    for node in &doc.nodes {
        assert!(source.is_char_boundary(node.source.start.0));
        assert!(source.is_char_boundary(node.source.end.0));
        assert!(doc.display_text.is_char_boundary(node.display.start.0));
        assert!(doc.display_text.is_char_boundary(node.display.end.0));
    }
    let selection = LogicalSelection {
        revision: 7,
        anchor: DisplayByte(0),
        head: DisplayByte(doc.display_text.len()),
    };
    assert_eq!(selection.copy(&doc, 7), Some(doc.display_text.as_str()));
    assert_eq!(selection.copy(&doc, 8), None);
    for (at, _) in doc.display_text.char_indices() {
        let source = doc.source_at(DisplayByte(at)).unwrap();
        assert!(source.end.0 <= doc.source.len());
    }
}
#[test]
fn literal_format_and_explicit_budgets() {
    let source = Arc::new("**plain** <b>x</b>\r\n😀".to_owned());
    let doc = parse_document(
        source.clone(),
        ContentFormat::PlainText,
        SyntaxOptions::default(),
        ParseLimits::automatic(),
        || false,
    )
    .unwrap();
    assert_eq!(doc.display_text, *source);
    for (limits, reason) in [
        (
            ParseLimits {
                source_bytes: 1,
                ..Default::default()
            },
            ParseStop::SourceBudget,
        ),
        (
            ParseLimits {
                display_bytes: 1,
                ..Default::default()
            },
            ParseStop::DisplayBudget,
        ),
        (
            ParseLimits {
                events: 1,
                ..Default::default()
            },
            ParseStop::EventBudget,
        ),
        (
            ParseLimits {
                nodes: 1,
                ..Default::default()
            },
            ParseStop::NodeBudget,
        ),
        (
            ParseLimits {
                retained_bytes: 1,
                ..Default::default()
            },
            ParseStop::RetainedBudget,
        ),
    ] {
        assert_eq!(
            parse_document(
                source.clone(),
                ContentFormat::Markdown,
                SyntaxOptions::default(),
                limits,
                || false
            )
            .unwrap_err()
            .reason,
            reason
        );
    }
    assert_eq!(
        parse_document(
            source,
            ContentFormat::Markdown,
            SyntaxOptions::default(),
            ParseLimits::automatic(),
            || true
        )
        .unwrap_err()
        .reason,
        ParseStop::Cancelled
    );
}
#[test]
fn mappings_are_not_raw_offset_identity() {
    let doc = parse("a &amp; **中**");
    let at = doc.display_text.find('&').unwrap();
    let mapped = doc.source_at(DisplayByte(at)).unwrap();
    assert!(doc.source[mapped.start.0..mapped.end.0].contains("&amp;"));
    let at = doc.display_text.find('中').unwrap();
    let mapped = doc.source_at(DisplayByte(at)).unwrap();
    assert!(doc.source[mapped.start.0..mapped.end.0].contains('中'));
}
#[test]
fn grapheme_selection_and_index_units() {
    let source = "a e\u{301} 😀中文";
    let index = ParagraphIndex::new(source);
    for (byte, _) in source.char_indices() {
        let scalar = index.scalar(byte).unwrap();
        assert_eq!(index.byte(scalar), Some(byte));
        assert_eq!(
            index.scalar_from_utf16(index.utf16(scalar).unwrap()),
            Some(scalar)
        );
    }
    let combining = source.find('\u{301}').unwrap();
    assert_eq!(grapheme_floor(source, combining), source.find('e').unwrap());
}
#[test]
fn protocols_are_classified_without_execution() {
    assert_eq!(classify_link("HTTPS://example.com/a"), LinkIntent::Web);
    assert_eq!(
        classify_link("../文档/a%20b.md#中文"),
        LinkIntent::LocalDocument
    );
    assert_eq!(classify_link("C:\\文档\\a.md"), LinkIntent::LocalDocument);
    for value in [
        "javascript:alert(1)",
        "data:text/html,x",
        "file:///secret",
        "https://a\nb",
    ] {
        assert_eq!(classify_link(value), LinkIntent::Blocked);
    }
    assert_eq!(classify_link("#中文"), LinkIntent::Fragment);
}

#[test]
fn memory_markup_preserves_plaintext_and_composed_identity() {
    use super::content::Markup;
    let a = Markup::plain("**literal** <html> &amp;\nsecond");
    let b = Markup::markdown("**formatted**");
    let joined = Markup::join(&[a.clone(), b.clone()]).unwrap();
    let again = Markup::join(&[a, b]).unwrap();
    assert_eq!(joined.identity, again.identity);
    let doc = parse(&joined);
    assert!(
        doc.display_text
            .contains("**literal** <html> &amp;\nsecond")
    );
    assert!(doc.display_text.contains("formatted"));
    let code = Markup::code("a \x60\x60\x60 b", "text");
    assert!(parse(&code).display_text.contains("a \x60\x60\x60 b"));
}
#[test]
fn heading_fragments_preserve_combining_graphemes_and_avoid_global_collisions() {
    let doc = parse("# Foo\n# Foo\n# Foo-1\n# e\u{301}\n# 😀\n# 😀");
    let headings = super::resources::headings(&doc);
    let slugs = headings
        .iter()
        .map(|(slug, _)| slug.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        slugs,
        [
            "foo",
            "foo-1",
            "foo-1-1",
            "e\u{301}",
            "section",
            "section-1"
        ]
    );
}

#[derive(Default)]
struct RecordingText {
    calls: Vec<String>,
}
impl ParagraphBackend for RecordingText {
    fn summary(&mut self, r: &ParagraphRequest<'_>) -> Result<ParagraphSummary, TextFailure> {
        self.calls.push(r.text.to_owned());
        let index = ParagraphIndex::new(r.text);
        let width = index.len() as f32 * 8.0;
        let lines = if r.wrap {
            (width / r.width).ceil().max(1.0) as usize
        } else {
            1
        };
        Ok(ParagraphSummary {
            width: if r.wrap { width.min(r.width) } else { width },
            height: lines as f32 * 22.0,
            lines: vec![LineSummary {
                scalars: ScalarIndex(0)..ScalarIndex(index.len()),
                top: 0.0,
                baseline: 17.0,
                height: 22.0,
                width,
            }],
        })
    }
    fn geometry(
        &mut self,
        _: &ParagraphRequest<'_>,
        _: std::ops::Range<ScalarIndex>,
        _: usize,
    ) -> Result<Vec<GraphemeGeometry>, TextFailure> {
        unreachable!("structural layout must not request global glyph geometry")
    }
}
fn frame(document: MarkdownDocument, width: f32) -> ReaderFrame {
    let document = Arc::new(document);
    let mut backend = RecordingText::default();
    let layout = Arc::new(
        layout_document(
            document.clone(),
            LayoutStyle {
                width,
                ..Default::default()
            },
            LayoutLimits::default(),
            &mut backend,
            || false,
        )
        .unwrap(),
    );
    ReaderFrame {
        scrollbar_dragging: false,
        key: FrameKey {
            parse: 1,
            layout: 1,
        },
        document,
        layout,
        viewport: Viewport::new(100.0),
        selection: None,
        geometry: Arc::from([]),
        geometry_key: FrameKey {
            parse: 1,
            layout: 1,
        },
        horizontal: Default::default(),
        focused: true,
        extend_selection: false,
    }
}
#[test]
fn reflow_preserves_source_anchor_and_structural_layout_does_not_request_glyphs() {
    let doc = parse(
        &(0..40)
            .map(|n| format!("paragraph {n} {}\n\n", "中 e\u{301} 😀 ".repeat(20)))
            .collect::<String>(),
    );
    let narrow = frame(doc.clone(), 200.0);
    let wide = frame(doc, 600.0);
    let mut viewport = Viewport::new(100.0);
    viewport.scroll_to(
        &narrow.layout,
        narrow.layout.boxes[12].top + narrow.layout.boxes[12].height * 0.4,
    );
    let anchor = viewport.anchor(&narrow.layout);
    viewport.reflow(&narrow.layout, &wide.layout, 150.0);
    let restored = viewport.anchor(&wide.layout);
    assert_eq!(restored.source, anchor.source);
    assert!((restored.fraction - anchor.fraction).abs() < 0.001);
}
#[test]
fn reducer_rejects_old_frames_and_separates_link_click_from_drag_selection() {
    let frame = frame(parse("[link](https://example.com) e\u{301} 😀"), 240.0);
    let link = frame
        .document
        .nodes
        .iter()
        .position(|node| matches!(node.kind, NodeKind::Link { .. }))
        .map(NodeId)
        .unwrap();
    let mut state = InteractionState::default();
    let hit = Some(Hit {
        display: DisplayByte(1),
        link: Some(link),
    });
    let (_, out) = reduce(
        &mut state,
        &frame,
        ReaderEvent::PointerDown {
            key: FrameKey {
                parse: 0,
                layout: 0,
            },
            hit,
            extend: false,
        },
    );
    assert!(state.pressed.is_none());
    assert!(out.link.is_none());
    reduce(
        &mut state,
        &frame,
        ReaderEvent::PointerDown {
            key: frame.key,
            hit,
            extend: false,
        },
    );
    let (_, out) = reduce(
        &mut state,
        &frame,
        ReaderEvent::PointerUp {
            key: frame.key,
            hit,
        },
    );
    assert_eq!(out.link, Some(link));
    reduce(
        &mut state,
        &frame,
        ReaderEvent::PointerDown {
            key: frame.key,
            hit,
            extend: false,
        },
    );
    reduce(
        &mut state,
        &frame,
        ReaderEvent::PointerDrag {
            key: frame.key,
            hit: Some(Hit {
                display: DisplayByte(4),
                link: Some(link),
            }),
            point: (40.0, 20.0),
        },
    );
    let (_, out) = reduce(
        &mut state,
        &frame,
        ReaderEvent::PointerUp {
            key: frame.key,
            hit,
        },
    );
    assert_eq!(out.link, None);
    let emoji_end = frame.document.display_text.find("😀").unwrap() + "😀".len();
    state.selection = Some(LogicalSelection {
        revision: frame.key.parse,
        anchor: DisplayByte(emoji_end),
        head: DisplayByte(emoji_end),
    });
    reduce(
        &mut state,
        &frame,
        ReaderEvent::Move {
            key: frame.key,
            direction: Move::Left,
            extend: true,
        },
    );
    let (_, out) = reduce(&mut state, &frame, ReaderEvent::Copy { key: frame.key });
    assert_eq!(out.copy.as_deref(), Some("😀"));
}
#[test]
fn oversized_cells_and_tight_items_get_an_observable_fallback_without_native_work() {
    for source in [
        format!("- {}\n\nTail", "word ".repeat(100)),
        format!("|header|\n|---|\n|{}|\n\nTail", "cell ".repeat(100)),
    ] {
        let document = Arc::new(parse(&source));
        let mut backend = RecordingText::default();
        let layout = layout_document(
            document.clone(),
            LayoutStyle::default(),
            LayoutLimits {
                paragraph_bytes: 64,
                ..Default::default()
            },
            &mut backend,
            || false,
        )
        .unwrap();
        assert!(
            layout
                .decorations
                .iter()
                .any(|decoration| matches!(decoration, Decoration::Fallback { .. }))
        );
        assert!(backend.calls.iter().all(|text| text.len() <= 64));
        assert!(backend.calls.iter().any(|text| text.contains("Tail")));
        assert!(
            document.display_text.matches("word").count() == 100
                || document.display_text.matches("cell").count() == 100
        );
    }
}

#[test]
fn a_card_draws_code_flat_with_compact_headings_and_a_quiet_module_path() {
    let hover = "```rust\nstd::process::Command\n```\n\n```rust\npub fn status(&mut self) -> io::Result<ExitStatus>\n```\n\n---\n\nExecutes a command.\n\n# Examples\n\n```rust\nlet x = 1;\n```\n";
    let lay = |card: bool| {
        layout_document(
            Arc::new(parse(hover)),
            LayoutStyle {
                card,
                ..Default::default()
            },
            LayoutLimits::default(),
            &mut RecordingText::default(),
            || false,
        )
        .unwrap()
    };
    let (plain, card) = (lay(false), lay(true));
    let blocks = |layout: &DocumentLayout| {
        layout
            .decorations
            .iter()
            .filter(|decoration| matches!(decoration, Decoration::Code { .. }))
            .count()
    };
    let styles = |layout: &DocumentLayout| {
        layout
            .paragraphs
            .iter()
            .flat_map(|paragraph| paragraph.spans.iter().map(|span| span.style))
            .collect::<Vec<_>>()
    };
    // A document keeps its code blocks, copy buttons and full-size headings.
    assert_eq!(blocks(&plain), 3);
    assert!(styles(&plain).iter().any(|style| style.heading == 1));
    assert!(styles(&plain).iter().all(|style| !style.muted));
    // A card has no block behind its code and no heading above the smallest tier.
    assert_eq!(blocks(&card), 0);
    assert!(styles(&card).iter().any(|style| style.heading == 4));
    assert!(
        styles(&card)
            .iter()
            .all(|style| style.heading == 0 || style.heading == 4)
    );
    // Only the module path above the signature is quiet.
    let quiet = styles(&card).iter().filter(|style| style.muted).count();
    assert_eq!(quiet, 1);
    assert!(card.height < plain.height);
}

#[test]
fn an_image_that_is_still_loading_takes_no_room_but_a_failed_one_keeps_its_box() {
    let document = Arc::new(parse("before\n\n![alt](a.png)\n\nafter\n"));
    let place = |state: Option<ImageState>| {
        let image = document
            .nodes
            .iter()
            .position(|node| matches!(node.kind, NodeKind::Image { .. }))
            .map(NodeId)
            .unwrap();
        let states = state.map(|state| (image, state)).into_iter().collect();
        layout_with_images(
            document.clone(),
            LayoutStyle::default(),
            LayoutLimits::default(),
            &mut RecordingText::default(),
            || false,
            &states,
        )
        .unwrap()
    };
    let size = |layout: &DocumentLayout| {
        let rect = &layout.images[0].rect;
        (rect.right - rect.left, rect.bottom - rect.top)
    };
    let pending = place(None);
    let loaded = place(Some(ImageState::Loaded(ImageSize {
        width: 90.0,
        height: 20.0,
    })));
    let failed = place(Some(ImageState::Failed));
    // The host still sees the image, at its place, so it knows to load it.
    assert_eq!(pending.images.len(), 1);
    assert_eq!(size(&pending), (0.0, 0.0));
    assert_eq!(size(&loaded), (90.0, 20.0));
    assert_eq!(size(&failed).1, 64.0);
    assert!(loaded.height >= pending.height + 20.0);
    assert!(failed.height >= pending.height + 64.0);
    // The text around it lays out the same whatever the image is doing.
    let top = |layout: &DocumentLayout| layout.paragraphs.last().unwrap().y;
    assert!(top(&pending) < top(&loaded) && top(&loaded) < top(&failed));
}

#[test]
fn only_the_first_column_and_header_row_mark_the_tables_outer_edges() {
    let document = Arc::new(parse("|a|b|\n|---|---|\n|c|d|\n|e|f|\n"));
    let layout = layout_document(
        document,
        LayoutStyle::default(),
        LayoutLimits::default(),
        &mut RecordingText::default(),
        || false,
    )
    .unwrap();
    let cells = layout
        .decorations
        .iter()
        .filter_map(|decoration| match decoration {
            Decoration::TableCell {
                header,
                first_column,
                ..
            } => Some((*header, *first_column)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        cells,
        [
            (true, true),
            (true, false),
            (false, true),
            (false, false),
            (false, true),
            (false, false)
        ]
    );
}

#[test]
fn keyboard_links_and_copy_code_share_the_pure_reader_transaction() {
    let frame = frame(
        parse("[one](a.md) [two](#title)\n\n\x60\x60\x60rust\nlet 中 = 1;\n\x60\x60\x60"),
        240.0,
    );
    let mut state = InteractionState::default();
    let first = next_link(&frame.document, None, 1, false).unwrap();
    reduce(
        &mut state,
        &frame,
        ReaderEvent::FocusLink {
            key: frame.key,
            node: first,
        },
    );
    assert_eq!(
        active_link(&frame.document, state.selection, 1),
        Some(first)
    );
    let second = next_link(&frame.document, state.selection, 1, false).unwrap();
    assert_ne!(first, second);
    reduce(
        &mut state,
        &frame,
        ReaderEvent::FocusLink {
            key: frame.key,
            node: second,
        },
    );
    assert_eq!(
        next_link(&frame.document, state.selection, 1, true),
        Some(first)
    );
    let code = frame
        .document
        .nodes
        .iter()
        .position(|node| matches!(node.kind, NodeKind::CodeBlock { .. }))
        .map(NodeId)
        .unwrap();
    let (_, effect) = reduce(
        &mut state,
        &frame,
        ReaderEvent::CopyNode {
            key: frame.key,
            node: code,
        },
    );
    assert_eq!(effect.copy.as_deref(), Some("let 中 = 1;\n"));
}

#[test]
fn layout_publication_can_advance_without_dropping_same_parse_logical_input() {
    let key = FrameKey {
        parse: 7,
        layout: 1,
    };
    let next = FrameKey {
        parse: 7,
        layout: 2,
    };
    let event = ReaderEvent::PointerDrag {
        key,
        hit: Some(Hit {
            display: DisplayByte(3),
            link: None,
        }),
        point: (20.0, 300.0),
    };
    assert!(matches!(event.on_frame(next),Some(ReaderEvent::PointerDrag {key,..}) if key==next));
    assert!(
        ReaderEvent::Copy { key }
            .on_frame(FrameKey {
                parse: 8,
                layout: 2
            })
            .is_none()
    );
}

#[test]
fn readme_style_html_header_becomes_centered_blocks_and_images() {
    let source = "<p align=\"center\">\n  <img src=\"logo.png\" alt=\"Loog\" width=\"96\">\n</p>\n\n<h1 align=\"center\">Loog</h1>\n\n<p align=\"center\">\n  A <b>fast</b> editor.\n</p>\n\nPlain **text**.\n";
    let doc = parse(source);
    assert_eq!(
        doc.display_text,
        "Loog\nLoog\nA fast editor.\nPlain text.\n"
    );
    let kinds = doc
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(index, node)| match &node.kind {
            NodeKind::Paragraph => Some(("paragraph", NodeId(index))),
            NodeKind::Heading { .. } => Some(("heading", NodeId(index))),
            NodeKind::Image { .. } => Some(("image", NodeId(index))),
            _ => None,
        })
        .collect::<Vec<_>>();
    let shown = kinds
        .iter()
        .map(|(name, id)| format!("{name}:{:?}", doc.alignment(*id)))
        .collect::<Vec<_>>();
    assert_eq!(
        shown,
        [
            "paragraph:Center",
            "image:Center",
            "heading:Center",
            "paragraph:Center",
            "paragraph:Left",
        ]
    );
    assert_eq!(doc.headings.len(), 1);
    assert!(doc.diagnostics.is_empty());
    for node in &doc.nodes {
        assert!(source.is_char_boundary(node.source.start.0));
        assert!(source.is_char_boundary(node.source.end.0));
        assert!(node.source.start.0 <= node.source.end.0);
    }
    for (at, _) in doc.display_text.char_indices() {
        assert!(doc.source_at(DisplayByte(at)).is_some());
    }
}

#[test]
fn html_outside_the_subset_stays_literal_with_a_diagnostic() {
    let doc = parse("<table><tr><td>x</td></tr></table>\n\nafter\n");
    assert!(doc.display_text.contains("<table>"));
    assert_eq!(doc.diagnostics.len(), 1);
}

#[test]
fn html_with_multibyte_text_keeps_exact_source_mapping() {
    let source = "<h2 align=\"center\">概览（说明）</h2>\n";
    let doc = parse(source);
    assert_eq!(doc.display_text, "概览（说明）\n");
    let at = source.find('说').unwrap();
    let mapped = doc.source_at(DisplayByte(doc.display_text.find('说').unwrap()));
    assert_eq!(mapped.map(|range| range.start.0), Some(at));
}

#[test]
fn markdown_scrollbar_gestures_are_view_local_and_survive_raster_frames() {
    let mut f = frame(
        parse(&format!(
            "```text\n{}\n```\n\n```text\n{}\n```",
            "wide ".repeat(100),
            "second ".repeat(100)
        )),
        240.0,
    );
    assert_eq!(f.layout.horizontal.len(), 2);
    let a = f.layout.horizontal[0].node;
    let b = f.layout.horizontal[1].node;
    let mut first = InteractionState::default();
    let mut second = InteractionState::default();
    reduce(
        &mut first,
        &f,
        ReaderEvent::ScrollbarDown {
            key: f.key,
            axis: ScrollAxis::Horizontal(a),
            grab: 7.0,
            position: None,
        },
    );
    reduce(
        &mut second,
        &f,
        ReaderEvent::ScrollbarDown {
            key: f.key,
            axis: ScrollAxis::Horizontal(b),
            grab: 19.0,
            position: None,
        },
    );
    let prior = f.key;
    f.key.layout += 1;
    let drag = ReaderEvent::ScrollbarDrag {
        key: prior,
        axis: ScrollAxis::Horizontal(a),
        position: 57.0,
        travel: 100.0,
    }
    .on_frame(f.key)
    .unwrap();
    reduce(&mut first, &f, drag);
    assert_eq!(first.horizontal[&a], f.layout.max_scroll(a) * 0.5);
    assert_eq!(second.scrollbar.unwrap().grab, 19.0);
    reduce(
        &mut first,
        &f,
        ReaderEvent::ScrollbarDrag {
            key: f.key,
            axis: ScrollAxis::Horizontal(b),
            position: 100.0,
            travel: 100.0,
        },
    );
    assert!(!first.horizontal.contains_key(&b));
    reduce(
        &mut first,
        &f,
        ReaderEvent::ScrollbarUp {
            key: f.key,
            axis: ScrollAxis::Horizontal(a),
        },
    );
    assert!(first.scrollbar.is_none());
    let before = first.horizontal[&a];
    reduce(
        &mut first,
        &f,
        ReaderEvent::ScrollbarDrag {
            key: f.key,
            axis: ScrollAxis::Horizontal(a),
            position: 99.0,
            travel: 100.0,
        },
    );
    assert_eq!(first.horizontal[&a], before);
    reduce(&mut second, &f, ReaderEvent::Focus(false));
    assert!(second.scrollbar.is_none());
    reduce(
        &mut first,
        &f,
        ReaderEvent::ScrollbarDown {
            key: f.key,
            axis: ScrollAxis::Horizontal(a),
            grab: 0.0,
            position: Some(f32::NAN),
        },
    );
    f.key.parse += 1;
    reduce(
        &mut first,
        &f,
        ReaderEvent::Scroll {
            key: f.key,
            delta: 0.0,
        },
    );
    assert!(first.scrollbar.is_none());
    assert!(first.horizontal.is_empty());
}

#[test]
fn markdown_selection_scrolls_at_code_block_edges_and_clamps_offsets() {
    let mut f = frame(
        parse(&format!("```text\n{}\n```\n", "中e\u{301}😀 ".repeat(100))),
        260.0,
    );
    let block = f.layout.horizontal[0].clone();
    let y = (block.rect.top + block.rect.bottom) * 0.5;
    f.geometry = Arc::from([ParagraphGeometry {
        paragraph: 0,
        glyphs: Arc::from([]),
    }]);
    let mut state = InteractionState::default();
    reduce(
        &mut state,
        &f,
        ReaderEvent::PointerDown {
            key: f.key,
            hit: Some(Hit {
                display: DisplayByte(0),
                link: None,
            }),
            extend: false,
        },
    );
    let point = (block.rect.right + 20.0, y);
    let (vertical, horizontal) = drag_scroll_delta(&f.layout, f.viewport, point);
    assert_eq!(vertical, 0.0);
    assert_eq!(horizontal, Some((block.node, 5.0)));
    for _ in 0..3 {
        reduce(
            &mut state,
            &f,
            ReaderEvent::PointerDrag {
                key: f.key,
                hit: None,
                point,
            },
        );
    }
    assert_eq!(state.horizontal[&block.node], 15.0);
    for _ in 0..8 {
        reduce(
            &mut state,
            &f,
            ReaderEvent::PointerDrag {
                key: f.key,
                hit: None,
                point: (block.rect.left - 20.0, y),
            },
        );
    }
    assert_eq!(state.horizontal[&block.node], 0.0);
    let (_, horizontal) = drag_scroll_delta(
        &f.layout,
        f.viewport,
        (block.rect.right + 30.0, block.rect.bottom + 1.0),
    );
    assert!(horizontal.is_none());
    assert_eq!(
        drag_scroll_delta(&f.layout, f.viewport, (f32::NAN, y)),
        (0.0, None)
    );
    reduce(
        &mut state,
        &f,
        ReaderEvent::PointerUp {
            key: f.key,
            hit: None,
        },
    );
    assert!(state.drag_point.is_none());
    assert!(state.pressed.is_none());
}

#[test]
fn han_text_meeting_latin_gets_extra_advance_only_on_the_glyph_before_the_boundary() {
    let text = "中文ab数字 x";
    let span = StyleSpan {
        bytes: 0..text.len(),
        style: InlineStyle::default(),
    };
    let spans = separate_scripts(text, vec![span]);
    // Every byte stays covered once, in order, so display offsets are unchanged.
    let mut next = 0;
    for span in &spans {
        assert_eq!(span.bytes.start, next);
        next = span.bytes.end;
    }
    assert_eq!(next, text.len());
    let spaced = spans
        .iter()
        .filter(|span| span.style.spaced)
        .map(|span| &text[span.bytes.clone()])
        .collect::<Vec<_>>();
    // 文|a and b|数 are boundaries; "字 x" has a real space and gets none.
    assert_eq!(spaced, ["文", "b"]);
}
