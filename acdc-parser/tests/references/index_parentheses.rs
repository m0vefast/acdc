use acdc_parser::{
    Options,
    ast::{BlockKind, BlockRef, InlineKind, InlineRef},
    parse,
};

type Error = Box<dyn std::error::Error>;

fn label<'doc>(nodes: impl IntoIterator<Item = InlineRef<'doc>>) -> Result<String, Error> {
    nodes
        .into_iter()
        .map(|node| {
            if let InlineKind::Plain(text) = node.kind() {
                Ok(text.text())
            } else if let InlineKind::Raw(text) = node.kind() {
                Ok(text.text())
            } else if let InlineKind::Symbol(symbol) = node.kind() {
                Ok(symbol.kind().text())
            } else {
                Err(format!("unexpected index label node: {node:?}").into())
            }
        })
        .collect()
}

// Rendered text can hide a parenthesis moved outside the index term. asciidoctor 2.0.26
// (`InlineIndextermMacroRx`, then `encl_text`): the label runs to the last `))` no `)` follows,
// and its closing `)` stands after the term.
#[test]
fn index_parentheses_keep_complete_catalog_labels_and_source_spans() -> Result<(), Error> {
    for depth in 1..=8 {
        for extra in 0..=4 {
            for prefix in ["", "α Before "] {
                let authored = format!(
                    "(({}))",
                    format_args!("fn{}café{}", "(".repeat(depth), ")".repeat(depth))
                );
                let content = format!("fn{}café{}", "(".repeat(depth), ")".repeat(depth + extra - 1));
                let markup = format!("(({content})))");
                let suffix = " after.";
                let source = format!("{prefix}{authored}{}{suffix}", ")".repeat(extra));
                assert!(source.ends_with(&format!("{markup}{suffix}")));
                let parsed = parse(&source, &Options::default())?;
                let [BlockKind::Paragraph(paragraph)] = &*parsed
                    .document()
                    .blocks()
                    .map(BlockRef::kind)
                    .collect::<Vec<_>>()
                else {
                    return Err("expected paragraph".into());
                };
                let term = paragraph
                    .content()
                    .inlines()
                    .find_map(|node| {
                        if let InlineKind::IndexTerm(term) = node.kind() {
                            Some(term)
                        } else {
                            None
                        }
                    })
                    .ok_or("missing index term")?;
                assert!(term.is_visible(), "{source}");
                assert_eq!(label(term.term().inlines())?, content, "{source}");
                assert_eq!(label(term.catalog().term().inlines())?, content, "{source}");
                // The term spans `((label`; the moved `)` and the closing `))` follow it.
                let span = term.location();
                let last = content.chars().next_back().map_or(0, char::len_utf8);
                assert_eq!(span.absolute_start(), prefix.len(), "{source}");
                assert_eq!(span.absolute_end(), prefix.len() + 2 + content.len() - last, "{source}");
                assert_eq!(&source[prefix.len() + 2 + content.len()..][..3], ")))", "{source}");
                assert_eq!(span.start().column() as usize, prefix.chars().count() + 1);
                assert_eq!(
                    span.end().column() as usize,
                    prefix.chars().count() + 2 + content.chars().count()
                );
                let [InlineKind::Plain(text)] = &*term
                    .term()
                    .inlines()
                    .map(InlineRef::kind)
                    .collect::<Vec<_>>()
                else {
                    return Err("expected complete plain label".into());
                };
                // `absolute_end` is the start of the last character, which may be multibyte.
                let label_start = prefix.len() + 2;
                assert_eq!(text.location().absolute_start(), label_start, "{source}");
                assert_eq!(
                    text.location().absolute_end(),
                    label_start + content.len() - content.chars().next_back().map_or(0, char::len_utf8),
                    "{source}"
                );
                let after = paragraph
                    .content()
                    .inlines()
                    .skip_while(|node| !matches!(node.kind(), InlineKind::IndexTerm(_)))
                    .skip(1)
                    .map(|node| label(std::iter::once(node)))
                    .collect::<Result<Vec<_>, _>>()?;
                assert_eq!(after, [")", suffix], "{source}");
                let paren = paragraph
                    .content()
                    .inlines()
                    .skip_while(|node| !matches!(node.kind(), InlineKind::IndexTerm(_)))
                    .nth(1)
                    .ok_or("missing moved parenthesis")?;
                let InlineKind::Plain(paren) = paren.kind() else {
                    return Err("expected plain parenthesis".into());
                };
                let at = prefix.len() + 2 + content.len();
                assert_eq!(
                    (paren.location().absolute_start(), paren.location().absolute_end()),
                    (at, at),
                    "{source}"
                );
            }
        }
    }
    Ok(())
}

#[test]
fn index_parentheses_preserve_adjacent_terms_and_unbalanced_prose() -> Result<(), Error> {
    for (source, expected) in [
        ("((first)) and ((second))", ["first", "second"]),
        ("((Open (literal)) and ((Next))", ["Open (literal", "Next"]),
        // asciidoctor 2.0.26: the label ends at the first `))` no `)` follows.
        ("((fn((x)) tail)) and ((Next))", ["fn((x", "Next"]),
    ] {
        let parsed = parse(source, &Options::default())?;
        let [BlockKind::Paragraph(paragraph)] = &*parsed
            .document()
            .blocks()
            .map(BlockRef::kind)
            .collect::<Vec<_>>()
        else {
            return Err("expected paragraph".into());
        };
        let terms = paragraph
            .content()
            .inlines()
            .filter_map(|node| {
                if let InlineKind::IndexTerm(term) = node.kind() {
                    Some(label(term.term().inlines()))
                } else {
                    None
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        assert_eq!(terms, expected, "{source}");
    }
    Ok(())
}

#[test]
fn index_parentheses_keep_registration_time_attribute_values() -> Result<(), Error> {
    for (subs, registered, span) in [
        ("macros,attributes,replacements", "Late {symbol}", "((Late {symbol}))"),
        // asciidoctor 2.0.26 `encl_text`: the expanded label's closing `)` stands after the term,
        // and the later replacements join it back into `®`.
        ("attributes,macros,replacements", "Late (R", "((Late {symbol}"),
    ] {
        let source = format!(":symbol: (R)\n\n[subs=\"{subs}\"]\n((Late {{symbol}})) after.");
        let parsed = parse(&source, &Options::default())?;
        let [BlockKind::Paragraph(paragraph)] = &*parsed
            .document()
            .blocks()
            .map(BlockRef::kind)
            .collect::<Vec<_>>()
        else {
            return Err("expected paragraph".into());
        };
        let [InlineKind::IndexTerm(term), ..] = &*paragraph
            .content()
            .inlines()
            .map(InlineRef::kind)
            .collect::<Vec<_>>()
        else {
            return Err("expected visible index term".into());
        };
        assert_eq!(label(term.term().inlines())?, "Late ®", "{source}");
        assert_eq!(
            label(term.catalog().term().inlines())?,
            registered,
            "{source}"
        );
        assert_eq!(
            source.get(term.location().absolute_start()..=term.location().absolute_end()),
            Some(span),
            "{source}"
        );
    }
    Ok(())
}
