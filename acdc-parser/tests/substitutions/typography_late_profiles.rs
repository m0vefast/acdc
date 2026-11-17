use acdc_parser::{CompatibilityMode, Options, SymbolKind, ast::InlineKind, parse};

type Error = Box<dyn std::error::Error>;

// Asciidoctor column: asciidoctor 2.0.26, measured 2026-11-16, the same in all four contexts. A
// passthrough is substituted alone, so its edge meets no word outside it.
const NONE: Vec<SymbolKind> = Vec::new();

#[rstest::rstest]
#[case("pass:r,a[{word}']s", NONE, vec![])]
#[case("pass:a,r[{word}']s", NONE, vec![SymbolKind::CurvedApostrophe])]
#[case("Sampass:r,a['{tail}]", NONE, vec![])]
#[case("Sampass:a,r['{tail}]", NONE, vec![SymbolKind::CurvedApostrophe])]
#[case("pass:r,a[{mark}]", NONE, vec![])]
#[case("pass:a,r[{mark}]", vec![SymbolKind::Copyright], vec![SymbolKind::Copyright])]
#[case("pass:r,a[left{dash}]right", NONE, vec![])]
#[case("pass:a,r[left{dash}]right", NONE, vec![SymbolKind::WordBoundedEmDash])]
#[case("pass:r,a[Sam']s", NONE, vec![SymbolKind::CurvedApostrophe])]
#[case("Sampass:r,a['s]", NONE, vec![SymbolKind::CurvedApostrophe])]
#[case("pass:r,a[--]", vec![SymbolKind::SpacedEmDash], vec![SymbolKind::SpacedEmDash])]
#[case("pass:r,a[(C) {word}]", vec![SymbolKind::Copyright], vec![SymbolKind::Copyright])]
#[case("pass:r,a[(C){word}']s", vec![SymbolKind::Copyright], vec![SymbolKind::Copyright])]
#[case("pass:r,a['{word} Sam']s", NONE, vec![SymbolKind::CurvedApostrophe])]
#[case("pass:r,a[{word}'s and Sam']s", NONE, vec![SymbolKind::CurvedApostrophe])]
#[case(r"Sampass:r,a[\'s]", NONE, vec![])]
#[case(r"pass:r,a[Sam\']s", NONE, vec![])]
#[case(r"pass:r,a[{word}\']s", NONE, vec![])]
#[case("pass:r,a[pré']été", NONE, vec![SymbolKind::CurvedApostrophe])]
#[case("pass:r,a[word_--]tail", NONE, vec![SymbolKind::WordBoundedEmDash])]
#[case("pass:r,a[---]tail", NONE, vec![])]
#[case("pass:r,a[Sam'{empty}]s", NONE, vec![])]
#[case("pass:a,r[Sam'{empty}]s", NONE, vec![SymbolKind::CurvedApostrophe])]
#[case("pass:c,r,a[-> {word}]", vec![SymbolKind::RightArrow], vec![SymbolKind::RightArrow])]
#[case("pass:r,a,c[-> {word}]", NONE, vec![])]
#[case("pass:r,a[-&gt; {word}]", vec![SymbolKind::RightArrow], vec![SymbolKind::RightArrow])]
#[case("pass:r,a[{arrow}]", NONE, vec![])]
#[case("pass:a,c,r[{arrow}]", NONE, vec![])]
#[case("pass:a,r[{arrow}]", vec![SymbolKind::RightArrow], vec![SymbolKind::RightArrow])]
#[case(r"pass:r,a[\-&gt; {word}]", NONE, vec![])]
#[case(r"pass:r,a[-> {word}\]]", NONE, vec![])]
fn profiles_do_not_run_replacements_after_later_attributes(
    #[values(CompatibilityMode::Asciidoctor, CompatibilityMode::Draft)] mode: CompatibilityMode,
    #[values("{}", "**{}**", "footnote:n[{}]", "link:outer.html[{}]")] context: &str,
    #[case] body: &str,
    #[case] asciidoctor: Vec<SymbolKind>,
    #[case] draft: Vec<SymbolKind>,
) -> Result<(), Error> {
    let expected = if mode == CompatibilityMode::Asciidoctor {
        asciidoctor
    } else {
        draft
    };
    let source = format!(
        ":word: Sam\n:tail: s\n:mark: (C)\n:dash: --\n:arrow: ->\n\nEarlier.\n\nα🚀 {}",
        context.replace("{}", body)
    );
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
    assert_eq!(kinds, expected, "{source}");
    assert_eq!(parsed.warnings(), []);
    Ok(())
}

#[rstest::rstest]
#[case(r"pass:r,a,m[link:target.html[(C)\]]", true, false, false, 1)]
#[case(r"pass:r,a,m[footnote:n[(C)\]]", false, true, false, 1)]
#[case(r"pass:r,m[footnote:n[(C)\]]", false, true, false, 1)]
#[case("pass:r,a,q[*{word} (C)*]", false, false, true, 1)]
#[case(r"pass:r,a,q,m[*link:target.html[(C)\]*]", true, false, true, 1)]
#[case(r"pass:m,r[footnote:n[(C)\]]", false, true, false, 0)]
fn resolved_symbols_keep_later_macro_and_formatting_structure(
    #[values(CompatibilityMode::Asciidoctor, CompatibilityMode::Draft)] mode: CompatibilityMode,
    #[case] body: &str,
    #[case] link: bool,
    #[case] note: bool,
    #[case] bold: bool,
    #[case] copyrights: usize,
) -> Result<(), Error> {
    let source = format!(":word: Sam\n\n{body}");
    let parsed = parse(
        &source,
        &Options::builder().with_compatibility_mode(mode).build()?,
    )?;
    let kinds = parsed
        .document()
        .content_occurrences()
        .flat_map(|field| field.content().inlines())
        .map(acdc_parser::ast::InlineRef::kind)
        .collect::<Vec<_>>();
    assert_eq!(kinds.iter().filter(|kind| matches!(kind, InlineKind::Symbol(symbol) if symbol.kind() == SymbolKind::Copyright)).count(), copyrights);
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| matches!(kind, InlineKind::Link(_)))
            .count(),
        usize::from(link)
    );
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| matches!(kind, InlineKind::Footnote(_)))
            .count(),
        usize::from(note)
    );
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| matches!(kind, InlineKind::Bold(_)))
            .count(),
        usize::from(bold)
    );
    assert_eq!(parsed.document().footnotes().count(), usize::from(note));
    assert_eq!(parsed.warnings(), []);
    Ok(())
}
