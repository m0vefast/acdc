use acdc_parser::{CompatibilityMode, Options, SymbolKind, ast::InlineKind, parse};

type Error = Box<dyn std::error::Error>;

// Asciidoctor column: asciidoctor 2.0.26, measured 2026-11-16 in all four contexts (they agree
// except the leading/trailing `--` case, which only draws dashes unwrapped). Replacements run
// before macros, so no dash joins across a macro or anchor.
#[rstest::rstest]
#[case("prefix-indexterm2:[-]tail", vec![], vec![SymbolKind::WordBoundedEmDash])]
#[case("left-[[between]]-right", vec![], vec![SymbolKind::WordBoundedEmDash])]
#[case("left-indexterm:[hidden]-right", vec![], vec![SymbolKind::WordBoundedEmDash])]
#[case("left indexterm2:[--] right", vec![], vec![SymbolKind::SpacedEmDash])]
#[case(
    "left indexterm2:[-]indexterm2:[-] right",
    vec![],
    vec![SymbolKind::SpacedEmDash]
)]
#[case("indexterm2:[--] right", vec![], vec![SymbolKind::SpacedEmDash])]
#[case("left indexterm2:[--]", vec![], vec![SymbolKind::SpacedEmDash])]
#[case(
    "-- indexterm2:[--] --",
    vec![SymbolKind::SpacedEmDash; 2],
    vec![SymbolKind::SpacedEmDash; 2]
)]
#[case(
    "left indexterm2:[--] -- indexterm2:[--] right",
    vec![SymbolKind::SpacedEmDash],
    vec![SymbolKind::SpacedEmDash]
)]
#[case("left-pass:[-]right", vec![], vec![])]
#[case(
    "left indexterm2:[pass:r[--]] right",
    vec![SymbolKind::SpacedEmDash],
    vec![SymbolKind::SpacedEmDash]
)]
#[case("left-indexterm2:[*two*]-right", vec![], vec![])]
#[case(r"left\-[[escaped]]-right", vec![], vec![])]
#[case("left-indexterm2:[--]right", vec![], vec![])]
#[case("left-[[profile]]-pass:[right]", vec![], vec![SymbolKind::WordBoundedEmDash])]
fn split_dashes_preserve_profiles_and_earlier_replacements(
    #[values(CompatibilityMode::Asciidoctor, CompatibilityMode::Draft)] mode: CompatibilityMode,
    #[values("{}", "**{}**", "footnote:n[{}]", "link:outer.html[{}]")] context: &str,
    #[case] body: &str,
    #[case] asciidoctor: Vec<SymbolKind>,
    #[case] draft: Vec<SymbolKind>,
) -> Result<(), Error> {
    if mode == CompatibilityMode::Asciidoctor {
        let expected = if context != "{}" && body == "-- indexterm2:[--] --" {
            vec![]
        } else {
            asciidoctor
        };
        return split_dash_kinds_match(mode, context, body, expected);
    }
    split_dash_kinds_match(mode, context, body, draft)
}

fn split_dash_kinds_match(
    mode: CompatibilityMode,
    context: &str,
    body: &str,
    expected: Vec<SymbolKind>,
) -> Result<(), Error> {
    let source = format!("Earlier.\n\nα🚀 {}", context.replace("{}", body));
    let parsed = parse(
        &source,
        &Options::builder().with_compatibility_mode(mode).build()?,
    )?;
    let kinds = parsed
        .document()
        .content_occurrences()
        .flat_map(|field| field.content().inlines())
        .filter_map(|node| {
            if let InlineKind::Symbol(symbol) = node.kind() {
                Some(symbol.kind())
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    let draft = mode == CompatibilityMode::Draft && context != "{}";
    let expected =
        if draft && (body == "indexterm2:[--] right" || body == "left indexterm2:[--]") {
            vec![]
        } else {
            expected
        };
    let expected = if draft && body == "-- indexterm2:[--] --" {
        vec![SymbolKind::SpacedEmDash]
    } else {
        expected
    };
    assert_eq!(kinds, expected, "{source}");
    assert_eq!(parsed.warnings(), []);
    Ok(())
}

#[rstest::rstest]
#[case("prefix-indexterm2:[-]tail", "-")]
#[case("left-[[a]]-right", "-")]
#[case("left indexterm2:[--] right", "--")]
#[case("left indexterm2:[-]indexterm2:[-] right", "-")]
#[case("left [[a]]-- right", "-- ")]
#[case("left --[[a]] right", " --")]
fn symbols_keep_only_their_owners_source_contribution(
    #[values(CompatibilityMode::Asciidoctor, CompatibilityMode::Draft)] mode: CompatibilityMode,
    #[case] body: &str,
    #[case] authored: &str,
) -> Result<(), Error> {
    let source = format!("Earlier.\n\nα🚀 {body}");
    let parsed = parse(
        &source,
        &Options::builder().with_compatibility_mode(mode).build()?,
    )?;
    let symbols = parsed
        .document()
        .content_occurrences()
        .flat_map(|field| field.content().inlines())
        .filter_map(|node| {
            if let InlineKind::Symbol(symbol) = node.kind() {
                Some(symbol)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    // asciidoctor 2.0.26 (measured 2026-11-16): replacements run before macros, so no token joins
    // across an index term or anchor.
    if mode == CompatibilityMode::Asciidoctor {
        assert!(symbols.is_empty(), "{source}");
        return Ok(());
    }
    let [symbol] = symbols.as_slice() else {
        return Err("expected one symbol".into());
    };
    let location = symbol.location();
    assert_eq!(
        source.get(location.absolute_start()..=location.absolute_end()),
        Some(authored),
        "{source}"
    );
    for term in parsed
        .document()
        .content_occurrences()
        .flat_map(|field| field.content().inlines())
        .filter_map(|node| {
            if let InlineKind::IndexTerm(term) = node.kind() {
                Some(term)
            } else {
                None
            }
        })
    {
        assert_eq!(
            term.catalog()
                .term()
                .inlines()
                .filter_map(|node| {
                    if let InlineKind::Symbol(symbol) = node.kind() {
                        Some(symbol.kind())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>(),
            if body == "left indexterm2:[--] right" {
                vec![SymbolKind::SpacedEmDash]
            } else {
                vec![]
            }
        );
        for child in term.term().inlines() {
            assert!(term.location().absolute_start() <= child.location().absolute_start());
            assert!(term.location().absolute_end() >= child.location().absolute_end());
        }
    }
    Ok(())
}

#[rstest::rstest]
#[case("left-{empty}indexterm2:[-]right")]
#[case("left-indexterm2:[-]{tail}")]
#[case("left-indexterm2:[{dash}]right")]
#[case("left-indexterm2:[-]{empty}right")]
fn split_tokens_obey_attribute_stage_order(
    #[values(CompatibilityMode::Asciidoctor, CompatibilityMode::Draft)] mode: CompatibilityMode,
    #[values("attributes,replacements,macros", "replacements,attributes,macros")] subs: &str,
    #[case] body: &str,
) -> Result<(), Error> {
    let source = format!(":empty:\n:tail: right\n:dash: -\n\n[subs=\"{subs}\"]\nα🚀 {body}.");
    let parsed = parse(
        &source,
        &Options::builder().with_compatibility_mode(mode).build()?,
    )?;
    let count = parsed
        .document()
        .content_occurrences()
        .flat_map(|field| field.content().inlines())
        .filter(|node| matches!(node.kind(), InlineKind::Symbol(_)))
        .count();
    // asciidoctor 2.0.26 (measured 2026-11-16): no dash joins across the index term in either order.
    assert_eq!(
        count,
        usize::from(mode == CompatibilityMode::Draft && subs.starts_with("attributes")),
        "{source}"
    );
    Ok(())
}
