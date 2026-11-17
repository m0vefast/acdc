use acdc_parser::{
    Options,
    ast::{BlockKind, BlockRef, InlineKind},
    parse,
};

type Error = Box<dyn std::error::Error>;

#[test]
fn invalid_code_boundaries_do_not_hide_attribute_references() -> Result<(), Error> {
    for (before, after) in [
        ("A", "B"),
        ("A", ""),
        ("", "B"),
        ("é", "日"),
        ("_", "X"),
        (":", ""),
        (";", ""),
        ("}", ""),
        ("", "\""),
    ] {
        for (value, expected) in [("café", "café"), ("", ""), ("{sp}", " ")] {
            for newline in ["\n", "\r\n"] {
                let body = format!("α {before}`{{word}}`{after} ω.");
                let source = format!(":word: {value}{newline}{newline}{body}");
                let parsed = parse(&source, &Options::default())?;
                let [BlockKind::Paragraph(paragraph)] = &*parsed
                    .document()
                    .blocks()
                    .map(BlockRef::kind)
                    .collect::<Vec<_>>()
                else {
                    return Err("expected one paragraph".into());
                };
                let Some((text, first, last)) =
                    crate::plain_run(paragraph.content().inlines(), parsed.preprocessed_source())
                else {
                    return Err(format!("expected literal backticks: {source:?}").into());
                };
                assert_eq!(text, body.replace("{word}", expected), "{source:?}");
                assert_eq!(
                    parsed
                        .preprocessed_source()
                        .get(first.absolute_start()..=last.absolute_end()),
                    Some(body.as_str())
                );
            }
        }
    }
    Ok(())
}

#[test]
fn invalid_code_edges_and_escapes_expand_attributes() -> Result<(), Error> {
    for (body, expected) in [
        ("`{word} `", "`café `"),
        ("` {word}`", "` café`"),
        ("`{word}\t`", "`café\t`"),
        ("`{word}\n`", "`café\n`"),
        ("\\`{word}`", "`café`"),
        ("\\``{word}``", "``café``"),
        ("A`\\{word}`B", "A`{word}`B"),
        ("A`{word\\}`B", "A`{word}`B"),
    ] {
        let source = format!(":word: café\n\nα {body} ω.");
        let parsed = parse(&source, &Options::default())?;
        let [BlockKind::Paragraph(paragraph)] = &*parsed
            .document()
            .blocks()
            .map(BlockRef::kind)
            .collect::<Vec<_>>()
        else {
            return Err("expected one paragraph".into());
        };
        let mut text = String::new();
        for node in paragraph.content().inlines() {
            if let InlineKind::Plain(plain) = node.kind() {
                text.push_str(plain.text());
            } else if let InlineKind::Raw(raw) = node.kind() {
                text.push_str(raw.text());
            } else {
                return Err(format!("expected literal code syntax: {source:?}: {node:?}").into());
            }
        }
        assert_eq!(text, format!("α {expected} ω."), "{source:?}");
    }
    Ok(())
}

#[test]
fn deferred_code_uses_the_later_valid_closer() -> Result<(), Error> {
    for rejected in ["`B", " `", "\t`", "\n`"] {
        let code_source = format!("`{{word}}{rejected} then +{{word}}+`");
        let source = format!(":word: café\n\nα {code_source}**End** ω.");
        let parsed = parse(&source, &Options::default())?;
        let [BlockKind::Paragraph(paragraph)] = &*parsed
            .document()
            .blocks()
            .map(BlockRef::kind)
            .collect::<Vec<_>>()
        else {
            return Err("expected one paragraph".into());
        };
        let code = paragraph
            .content()
            .inlines()
            .find_map(|node| {
                if let InlineKind::Monospace(code) = node.kind() {
                    Some(code)
                } else {
                    None
                }
            })
            .ok_or("missing code")?;
        let nodes = code.content().inlines().collect::<Vec<_>>();
        let (Some((text, first, last)), Some(InlineKind::Raw(raw))) = (
            crate::plain_run(nodes.iter().copied().take(nodes.len().saturating_sub(1)), &source),
            nodes.last().map(|node| node.kind()),
        ) else {
            return Err(format!(
                "expected ordinary text and protected reference: {:?}",
                code.content()
            )
            .into());
        };
        assert_eq!(text, format!("café{rejected} then "));
        assert_eq!(
            source.get(first.absolute_start()..=last.absolute_end()),
            Some(format!("{{word}}{rejected} then ").as_str())
        );
        assert_eq!(raw.text(), "{word}");
        assert_eq!(
            source.get(code.location().absolute_start()..=code.location().absolute_end()),
            Some(code_source.as_str())
        );
        assert_eq!(
            source.get(raw.location().absolute_start()..=raw.location().absolute_end()),
            Some("{word}")
        );
        let bold = paragraph
            .content()
            .inlines()
            .find(|node| matches!(node.kind(), InlineKind::Bold(_)))
            .ok_or("missing following bold span")?;
        let location = bold.location();
        assert_eq!(
            source.get(location.absolute_start()..=location.absolute_end()),
            Some("**End**")
        );
    }
    Ok(())
}

#[test]
fn expanded_references_do_not_create_new_code_boundaries() -> Result<(), Error> {
    for body in [
        "A`{empty}`B | A`{sp}`B | A`{unknown}`B.",
        "A`\\{word}`B | A`{word\\}`B.",
    ] {
        let source = format!(":word: café\n:empty:\n\n{body}");
        let parsed = parse(&source, &Options::default())?;
        let [BlockKind::Paragraph(paragraph)] = &*parsed
            .document()
            .blocks()
            .map(BlockRef::kind)
            .collect::<Vec<_>>()
        else {
            return Err("expected paragraph".into());
        };
        assert!(
            !paragraph
                .content()
                .inlines()
                .any(|node| matches!(node.kind(), InlineKind::Monospace(_))),
            "{body}: {:?}",
            paragraph.content()
        );
    }
    let source = ":word: café\n\n[red]`{word}`X | [red]`{word}`.";
    let parsed = parse(source, &Options::default())?;
    let [BlockKind::Paragraph(paragraph)] = &*parsed
        .document()
        .blocks()
        .map(BlockRef::kind)
        .collect::<Vec<_>>()
    else {
        return Err("expected paragraph".into());
    };
    let nodes = paragraph.content().inlines().collect::<Vec<_>>();
    let (Some(InlineKind::Monospace(code)), Some((tail, first, last))) = (
        nodes.first().map(|node| node.kind()),
        crate::plain_run(nodes.iter().copied().skip(1), source),
    ) else {
        return Err(format!(
            "expected one code span and its tail: {:?}",
            paragraph.content()
        )
        .into());
    };
    assert_eq!(tail, "café`.");
    assert_eq!(
        source.get(first.absolute_start()..=last.absolute_end()),
        Some("{word}`.")
    );
    assert_eq!(
        source.get(code.location().absolute_start()..=code.location().absolute_end()),
        Some("`{word}`X | [red]`")
    );
    Ok(())
}

#[test]
fn invalid_code_expansion_retains_include_origins() -> Result<(), Error> {
    let parsed = acdc_parser::parse_file(
        "fixtures/tests/subs_invalid_code_attributes.adoc",
        &Options::builder()
            .with_safe_mode(acdc_parser::SafeMode::Unsafe)
            .build()?,
    )?;
    let source = include_str!("../../fixtures/tests/includes/invalid_code_attributes.adoc");
    let (text, first, last) = parsed
        .document()
        .blocks()
        .find_map(|block| {
            let BlockKind::Section(section) = block.kind() else {
                return None;
            };
            section.blocks().find_map(|block| {
                let BlockKind::Paragraph(paragraph) = block.kind() else {
                    return None;
                };
                crate::plain_run(paragraph.content().inlines(), source)
                    .filter(|(text, _, _)| text.starts_with("B44 "))
            })
        })
        .ok_or("missing included content")?;
    assert_eq!(text, "B44 A`late`B.");
    assert_eq!(
        source.get(first.absolute_start()..=last.absolute_end()),
        Some("B44 A`{word}`B.")
    );
    assert!(
        first
            .start()
            .include_chain()
            .and_then(Iterator::last)
            .is_some_and(|file| file.ends_with("includes/invalid_code_attributes.adoc"))
    );
    assert_eq!(
        first.start().include_chain().map(Iterator::collect::<Vec<_>>),
        last.end().include_chain().map(Iterator::collect::<Vec<_>>)
    );
    Ok(())
}
