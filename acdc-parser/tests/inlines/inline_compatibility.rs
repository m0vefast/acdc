use acdc_parser::{
    Options,
    ast::{BlockKind, BlockRef, FootnoteDefinitionRef, InlineKind, InlineRef, ReferenceTargetRef},
    parse,
};

type Error = Box<dyn std::error::Error>;

/// asciidoctor 2.0.26 (measured 2026-11-16, `extract_attributes_from_text`): a link or xref
/// label read as an attribute list (it holds `=`) folds its line feeds into spaces; other
/// labels keep them.
#[test]
fn attribute_list_labels_fold_line_feeds() -> Result<(), Error> {
    let source = "[[d]]Dest.\n\nA link:https://e.org/a[First\nsecond,id=x] link:https://e.org/b[First\nsecond] link:https://e.org/c[\"Line\ntwo\",role=g] link:https://e.org/d[\"Q\nr\"] <<d,First\nsecond>> xref:d[First\nsecond] xref:d[First\nsecond,role=r].\n";
    for (mode, folded) in [
        (acdc_parser::CompatibilityMode::Asciidoctor, true),
        (acdc_parser::CompatibilityMode::Draft, false),
    ] {
        let parsed = parse(source, &Options::builder().with_compatibility_mode(mode).build()?)?;
        let Some(BlockKind::Paragraph(paragraph)) = parsed.document().blocks().nth(1).map(BlockRef::kind)
        else {
            return Err("missing paragraph".into());
        };
        let mut labels = Vec::new();
        for node in paragraph.content().inlines() {
            let content = match node.kind() {
                InlineKind::Link(link) => link.text(),
                InlineKind::CrossReference(xref) => xref.text(),
                _ => continue,
            };
            let mut text = String::new();
            for inner in content.inlines() {
                if let InlineKind::Plain(plain) = inner.kind() {
                    text.push_str(plain.text());
                }
            }
            labels.push(text);
        }
        let fold = |text: &'static str| if folded { text.replace('\n', " ") } else { text.to_owned() };
        assert_eq!(
            labels,
            [
                fold("First\nsecond"),
                "First\nsecond".to_owned(),
                fold("Line\ntwo"),
                "\"Q\nr\"".to_owned(),
                "First\nsecond".to_owned(),
                "First\nsecond".to_owned(),
                fold("First\nsecond"),
            ],
            "{mode:?}"
        );
    }
    Ok(())
}

/// asciidoctor 2.0.26 (measured 2026-11-16): a visible index term is the shortest label
/// before a `))` no `)` follows — `((fn((x)) tail))` is the term `fn((x` and then ` tail))`.
#[test]
fn visible_index_terms_end_at_the_first_closing_pair() -> Result<(), Error> {
    for (source, drawn) in [
        ("A ((fn((x)) tail)) after.", "A fn((x tail)) after."),
        ("B ((one)) two)).", "B one two))."),
        // Replacements run before macros: the label's neighbors are `[` `]`, so `--` stays.
        ("P01 prefixindexterm2:[--]tail", "P01 prefix--tail"),
        // Outside words see the macro source (`]`, `i`), and an escape no replacement used stays.
        ("P02 Saindexterm2:[m]'s", "P02 Sam's"),
        (r"P03 Samindexterm2:[\']s", r"P03 Sam\'s"),
        (r"P04 wordindexterm2:[\--]word", r"P04 word\--word"),
        (r"P05 word\-- end", "P05 word-- end"),
        (r"P06 word\--word", "P06 word--word"),
        (r"P07 a \--b", r"P07 a \--b"),
        // An inline anchor is its source too (asciidoctor 2.0.26, measured 2026-11-16).
        ("P08 Sam'anchor:digits[]123", "P08 Sam’123"),
        ("P09 Sam'anchor:late[]s", "P09 Sam’s"),
        ("P10 Samanchor:early[]'s", "P10 Sam's"),
        ("P11 left-anchor:between[]-right", "P11 left--right"),
        ("P12 left--indexterm2:[x]y", "P12 left—\u{200b}xy"),
        ("P13 Sam'((x))", "P13 Sam'x"),
        ("P14 Sam'indexterm2:[x]", "P14 Sam’x"),
        ("P15 Sam'[[mid]]s", "P15 Sam's"),
        ("P16 x((y))--z", "P16 xy--z"),
        // Macros first: an anchor is already its tag, a hidden term already nothing.
        ("[subs=\"macros,replacements\"]\nP18 left-anchor:x[]-right", "P18 left--right"),
        ("[subs=\"macros,replacements\"]\nP19 left--anchor:y[]right", "P19 left--right"),
        ("[subs=\"macros,replacements\"]\nP20 left--indexterm:[h]right", "P20 left—\u{200b}right"),
    ] {
        let parsed = parse(source, &Options::default())?;
        let Some(BlockKind::Paragraph(paragraph)) = parsed.document().blocks().next().map(BlockRef::kind) else {
            return Err("missing paragraph".into());
        };
        let mut text = String::new();
        for node in paragraph.content().inlines() {
            match node.kind() {
                InlineKind::Plain(plain) => text.push_str(plain.text()),
                InlineKind::Raw(raw) => text.push_str(raw.text()),
                InlineKind::Symbol(symbol) => text.push_str(symbol.text()),
                InlineKind::StandaloneCurvedApostrophe(_) => text.push('’'),
                InlineKind::Anchor(_) => {}
                InlineKind::IndexTerm(term) if !term.is_visible() => {}
                InlineKind::IndexTerm(term) => {
                    for inner in term.term().inlines() {
                        if let InlineKind::Plain(plain) = inner.kind() {
                            text.push_str(plain.text());
                        }
                    }
                }
                other => return Err(format!("unexpected node {other:?} in {source:?}").into()),
            }
        }
        assert_eq!(text, drawn, "{source:?}");
    }
    Ok(())
}

#[test]
fn index_registration_boundaries_survive_later_substitutions() -> Result<(), Error> {
    for subs in [
        "macros,attributes",
        "macros,quotes",
        "macros,attributes,quotes",
    ] {
        for label in [
            "((Start {close} End))",
            "indexterm2:[Start {bracket} End]",
            "({empty}(Joined))",
            "(({empty})) indexterm2:[{empty}]",
            "((Outer ((Inner))))",
        ] {
            for code in [false, true] {
                for index in ["", "\n[index]\n== Index\n"] {
                    let block = if code {
                        format!("[source,text,subs=\"{subs}\"]\n----\n{label}\n----")
                    } else {
                        format!("[subs=\"{subs}\"]\n{label}")
                    };
                    let source = format!(
                        "= Boundaries\n:close: ))\n:bracket: ]\n:empty:\n\n{block}\n{index}"
                    );
                    let parsed = parse(&source, &Options::default())?;
                    assert_ne!(parsed.document().blocks().count(), 0);
                }
            }
        }
    }
    Ok(())
}

#[test]
fn index_catalog_snapshots_do_not_register_footnotes_twice() -> Result<(), Error> {
    let source = include_str!("../../fixtures/tests/subs_index_registration_order.adoc");
    let parsed = parse(source, &Options::default())?;
    let notes = parsed.document().footnotes().collect::<Vec<_>>();
    assert_eq!(notes.len(), 2);
    for (note, expected) in notes
        .iter()
        .zip(["footnote:[Only once]", "footnote:[Second note]"])
    {
        assert_eq!(
            source.get(note.location().absolute_start()..=note.location().absolute_end()),
            Some(expected)
        );
    }
    assert_eq!(
        notes
            .iter()
            .copied()
            .map(FootnoteDefinitionRef::number)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    Ok(())
}

#[test]
fn ordered_index_footnotes_keep_one_definition_and_original_locations() -> Result<(), Error> {
    let source = include_str!("../../fixtures/tests/subs_index_stage_labels.adoc");
    let parsed = parse(source, &Options::default())?;
    let notes = parsed.document().footnotes().collect::<Vec<_>>();
    assert_eq!(notes.len(), 4);
    for (note, expected) in notes.iter().zip([
        "footnote:[One]",
        "footnote:[Two]",
        "footnote:[((Note {name})) and *literal* (C)]",
        "footnote:[*bold* ((Early {name}))]",
    ]) {
        assert_eq!(
            source.get(note.location().absolute_start()..=note.location().absolute_end()),
            Some(expected)
        );
    }
    Ok(())
}

#[test]
fn passthrough_index_attribute_values_point_to_their_reference() -> Result<(), Error> {
    for value in ["Expanded", "é", "🦀", "foobar", "a much longer replacement"] {
        for stages in ["attributes,macros", "macros,attributes"] {
            let source = format!(":name: {value}\n\npass:{stages}[((Term {{name}}))]\n");
            let parsed = parse(&source, &Options::default())?;
            let Some(BlockKind::Paragraph(paragraph)) =
                parsed.document().blocks().next().map(BlockRef::kind)
            else {
                return Err("missing paragraph".into());
            };
            let Some(InlineKind::IndexTerm(term)) =
                paragraph.content().inlines().next().map(InlineRef::kind)
            else {
                return Err("missing index term".into());
            };
            let raw = term
                .term()
                .inlines()
                .find_map(|node| {
                    if let InlineKind::Raw(raw) = node.kind() {
                        (raw.text() == value).then_some(raw)
                    } else {
                        None
                    }
                })
                .ok_or("missing expanded label")?;
            assert_eq!(
                source.get(raw.location().absolute_start()..=raw.location().absolute_end()),
                Some("{name}"),
                "{stages}: {value} at {:?}",
                raw.location(),
            );
        }
    }
    Ok(())
}

#[test]
fn disabled_attribute_substitutions_do_not_warn_about_counters() -> Result<(), Error> {
    for subs in ["quotes", "none", "normal,-attributes"] {
        let source = format!(
            "[subs=\"{subs}\"]\nLiteral {{counter:seq}}, {{counter:seq:4}}, and {{counter2:seq}} with *bold*.\n"
        );
        let parsed = parse(&source, &Options::default())?;
        assert!(
            parsed
                .warnings()
                .iter()
                .all(|warning| !warning.kind.to_string().contains("Counters")),
            "{:?}",
            parsed.warnings()
        );
    }
    Ok(())
}

#[test]
fn bibliography_footnotes_keep_their_source_and_catalog_entry() -> Result<(), Error> {
    let source = "[bibliography]\n* [[[note,footnote:[A note.] +]]] Footnote label.\n";
    let parsed = parse(source, &Options::default())?;
    let [footnote] = &*parsed.document().footnotes().collect::<Vec<_>>() else {
        return Err("expected one bibliography footnote".into());
    };
    assert_eq!(footnote.number(), 1);
    assert!(
        matches!(&*footnote.content().inlines().map(InlineRef::kind).collect::<Vec<_>>(), [InlineKind::Plain(text)] if text.text() == "A note.")
    );
    assert_eq!(
        source.get(footnote.location().absolute_start()..=footnote.location().absolute_end()),
        Some("footnote:[A note.]")
    );
    Ok(())
}

#[test]
fn bibliography_counter_citations_remain_literal() -> Result<(), Error> {
    let parsed = parse(
        "Cite <<literal-counter>>.\n\n[bibliography]\n* [[[literal-counter,{counter:seq} +]]] Entry.\n",
        &Options::default(),
    )?;
    let label = parsed
        .document()
        .references()
        .get("literal-counter")
        .and_then(ReferenceTargetRef::xreflabel)
        .ok_or("missing citation")?;
    let text = label
        .inlines()
        .map(|node| {
            if let InlineKind::Plain(text) = node.kind() {
                Ok(text.text())
            } else if let InlineKind::Raw(raw) = node.kind() {
                Ok(raw.text())
            } else {
                Err(Error::from("expected literal citation text"))
            }
        })
        .collect::<Result<String, Error>>()?;
    assert_eq!(text, "[{counter:seq} +]");
    Ok(())
}
