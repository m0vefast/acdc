use acdc_parser::{
    CompatibilityMode, Options,
    ast::{BlockKind, BlockRef, InlineKind},
    parse,
};

type Error = Box<dyn std::error::Error>;

#[rstest::rstest]
fn verse_inline_locations_and_catalogs_follow_the_source(
    #[values("\n", "\r\n")] newline: &str,
) -> Result<(), Error> {
    let source = "= Verse\n:value: expanded\n\n[verse]\n____\nÉva *Bold* {value}\n  link:https://example.org[Link]\n\nanchor:target[]footnote:note[Verse note.] ((Term))\n____\n\nReuse footnote:note[].\n"
        .replace('\n', newline);
    let parsed = parse(&source, &Options::default())?;
    let Some(BlockKind::Verse(block)) = parsed.document().blocks().next().map(BlockRef::kind)
    else {
        return Err("expected verse block".into());
    };
    let nodes = block.content();
    assert!(
        nodes
            .inlines()
            .any(|node| matches!(node.kind(), InlineKind::Bold(_)))
    );
    for expected in [
        "*Bold*",
        "link:https://example.org[Link]",
        "footnote:note[Verse note.]",
        "((Term))",
        "anchor:target[]",
    ] {
        assert!(
            nodes.inlines().any(|node| {
                let location = node.location();
                parsed
                    .preprocessed_source()
                    .get(location.absolute_start()..=location.absolute_end())
                    == Some(expected)
            }),
            "missing complete source span: {expected}"
        );
    }
    let [note] = &*parsed.document().footnotes().collect::<Vec<_>>() else {
        return Err("expected one reused footnote".into());
    };
    assert!(!note.content().is_empty());
    assert_eq!(note.number(), 1);
    Ok(())
}

// asciidoctor 2.0.26 (measured 2026-11-16): a verse block reads its lines verbatim, a
// slash line among them included; Draft drops the comment as it always has.
#[rstest::rstest]
#[case::asciidoctor(CompatibilityMode::Asciidoctor, "Nested verse.\n// Nested slash line")]
#[case::draft(CompatibilityMode::Draft, "Nested verse.")]
fn verse_block_slash_lines_follow_the_selected_mode(
    #[case] mode: CompatibilityMode,
    #[case] expected: &str,
) -> Result<(), Error> {
    let source = "[verse]\n____\nNested verse.\n// Nested slash line\n____\n\nAfter.\n";
    let parsed = parse(source, &Options::builder().with_compatibility_mode(mode).build()?)?;
    let Some(BlockKind::Verse(block)) = parsed.document().blocks().next().map(BlockRef::kind)
    else {
        return Err("expected verse block".into());
    };
    let text: String = block
        .content()
        .inlines()
        .filter_map(|node| match node.kind() {
            InlineKind::Plain(value) => Some(value.text().to_owned()),
            _ => None,
        })
        .collect();
    assert_eq!(text, expected);
    Ok(())
}

// The verse fence closes its own block: later fences still pair, so a comment against
// the next paragraph is dropped (asciidoctor 2.0.26, measured 2026-11-16).
#[test]
fn a_nested_verse_block_leaves_later_fences_paired() -> Result<(), Error> {
    let source = "[example]\n--\n[verse]\n____\nV.\n// kept\n____\n--\n\n[quote]\n--\nE.\n[pass]\n--\n\nOutside.\n// gone\n";
    let options = Options::builder()
        .with_compatibility_mode(CompatibilityMode::Asciidoctor)
        .build()?;
    let parsed = parse(source, &options)?;
    let Some(BlockKind::Paragraph(last)) = parsed.document().blocks().last().map(BlockRef::kind)
    else {
        return Err("expected a trailing paragraph".into());
    };
    let text: String = last
        .content()
        .inlines()
        .filter_map(|node| match node.kind() {
            InlineKind::Plain(value) => Some(value.text().to_owned()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Outside.");
    Ok(())
}

#[test]
fn verse_explicit_substitutions_follow_the_feature_contract() -> Result<(), Error> {
    let source = "[verse,subs=\"none\"]\n____\n*Bold* footnote:[Inactive.]\n____\n";
    let parsed = parse(source, &Options::default())?;
    assert_eq!(parsed.document().footnotes().count(), 0);
    Ok(())
}
