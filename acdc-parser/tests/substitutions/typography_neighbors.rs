use acdc_parser::{CompatibilityMode, Options, Substitution, SymbolKind, ast::InlineKind, parse};

type Error = Box<dyn std::error::Error>;

// Asciidoctor column: asciidoctor 2.0.26, measured 2026-11-16 in all four contexts. Replacements
// run before macros, so a macro or anchor is still its source (`]`, `a`, `i`, `l`) to its words.
const APOS: Option<SymbolKind> = Some(SymbolKind::CurvedApostrophe);
const DASH: Option<SymbolKind> = Some(SymbolKind::WordBoundedEmDash);

#[rstest::rstest]
#[case("Sam'anchor:x[]s", APOS, APOS)]
#[case("Sam'indexterm:[hidden]s", APOS, APOS)]
#[case("Saindexterm2:[m]'s", None, APOS)]
#[case("indexterm2:[Sam]'s", None, APOS)]
#[case("Samindexterm2:['s]", None, APOS)]
#[case("Sam'indexterm2:[s]", APOS, APOS)]
#[case("left--anchor:x[]right", DASH, DASH)]
#[case("left--indexterm:[hidden]right", DASH, DASH)]
#[case("leftindexterm2:[--right]", None, DASH)]
#[case("indexterm2:[left--]right", None, DASH)]
#[case("leftindexterm2:[--]right", None, DASH)]
#[case("Sam'indexterm2:[*s*]", APOS, None)]
#[case("Saindexterm2:[*m*]'s", None, None)]
#[case("Sam'link:guide.html[s]", APOS, None)]
#[case("*Sam*'s", None, None)]
#[case("Sam'anchor:x[]123", APOS, None)]
#[case("Sam'anchor:x[]_s", APOS, None)]
#[case("Sam'anchor:x[]", APOS, None)]
#[case("left--anchor:x[]!", DASH, None)]
#[case(r"Sam\'anchor:x[]s", None, None)]
fn neighbors_supply_word_context_without_moving_owned_text(
    #[values(CompatibilityMode::Asciidoctor, CompatibilityMode::Draft)] mode: CompatibilityMode,
    #[values("{}", "**{}**", "footnote:n[{}]", "link:outer.html[{}]")] context: &str,
    #[case] body: &str,
    #[case] asciidoctor: Option<SymbolKind>,
    #[case] draft: Option<SymbolKind>,
) -> Result<(), Error> {
    let kind = if mode == CompatibilityMode::Asciidoctor {
        asciidoctor
    } else {
        draft
    };
    let source = format!("Earlier.\n\nα🚀 {}.", context.replace("{}", body));
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
    assert_eq!(
        symbols.iter().map(|s| s.kind()).collect::<Vec<_>>(),
        kind.into_iter().collect::<Vec<_>>(),
        "{source}"
    );
    if let [symbol] = symbols.as_slice() {
        let token = if kind == Some(SymbolKind::CurvedApostrophe) {
            "'"
        } else {
            "--"
        };
        let start = source.rfind(token).ok_or("authored token")?;
        assert_eq!(symbol.location().absolute_start(), start, "{source}");
        assert_eq!(
            symbol.location().absolute_end(),
            start + token.len() - 1,
            "{source}"
        );
    }
    assert_eq!(
        parsed.document().footnotes().count(),
        usize::from(context.starts_with("footnote:"))
    );
    assert_eq!(parsed.warnings(), []);
    Ok(())
}

#[rstest::rstest]
fn resolved_visible_labels_preserve_the_registration_snapshot(
    #[values(CompatibilityMode::Asciidoctor, CompatibilityMode::Draft)] mode: CompatibilityMode,
    #[values("replacements,macros", "macros,replacements")] subs: &str,
) -> Result<(), Error> {
    let source = format!("[subs=\"{subs}\"]\nleftindexterm2:[--]right");
    let parsed = parse(
        &source,
        &Options::builder().with_compatibility_mode(mode).build()?,
    )?;
    let term = parsed
        .document()
        .content_occurrences()
        .flat_map(|field| field.content().inlines())
        .find_map(|node| {
            if let InlineKind::IndexTerm(term) = node.kind() {
                Some(term)
            } else {
                None
            }
        })
        .ok_or("index occurrence")?;
    // asciidoctor 2.0.26 (measured 2026-11-16): replacements first leave the bracketed `--` alone
    // (`left--right`); macros first register `--` and the later replacements join it into a dash.
    let asciidoctor_first = mode == CompatibilityMode::Asciidoctor && subs == "replacements,macros";
    assert_eq!(
        term.term()
            .inlines()
            .any(|node| matches!(node.kind(), InlineKind::Symbol(_))),
        !asciidoctor_first
    );
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
        if subs == "replacements,macros" && mode == CompatibilityMode::Draft {
            vec![SymbolKind::SpacedEmDash]
        } else {
            vec![]
        }
    );
    assert_eq!(parsed.warnings(), []);
    Ok(())
}

// Asciidoctor column: asciidoctor 2.0.26, measured 2026-11-16, the same in both orders. The anchor
// is still its source (`a`, `]`) when replacements run.
#[rstest::rstest]
#[case("Sam'anchor:x[]{tail}", SymbolKind::CurvedApostrophe, "'", true)]
#[case("{head}anchor:x[]'s", SymbolKind::CurvedApostrophe, "'", false)]
#[case("left--anchor:x[]{tail}", SymbolKind::WordBoundedEmDash, "--", true)]
#[case("{head}anchor:x[]--right", SymbolKind::WordBoundedEmDash, "--", false)]
fn neighboring_attribute_values_obey_the_parent_stage_order(
    #[values(CompatibilityMode::Asciidoctor, CompatibilityMode::Draft)] mode: CompatibilityMode,
    #[values("attributes,replacements,macros", "replacements,attributes,macros")] subs: &str,
    #[case] body: &str,
    #[case] kind: SymbolKind,
    #[case] token: &str,
    #[case] asciidoctor: bool,
) -> Result<(), Error> {
    let source = format!(":head: Sam\n:tail: s\n\n[subs=\"{subs}\"]\nα🚀 {body}.");
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
    if mode == CompatibilityMode::Asciidoctor {
        let expected: &[SymbolKind] = if asciidoctor { &[kind] } else { &[] };
        assert_eq!(kinds, expected, "{source}");
    } else if subs.starts_with("attributes") {
        assert_eq!(kinds, [kind], "{source}");
    } else {
        assert_eq!(kinds, [], "{source}");
        assert!(parsed.document().content_occurrences().flat_map(|field|field.content().inlines())
            .any(|node| matches!(node.kind(),InlineKind::Raw(raw) if raw.text()==token && !raw.substitutions().any(|sub|sub==Substitution::Replacements))),"{source}");
    }
    assert_eq!(parsed.warnings(), []);
    Ok(())
}
