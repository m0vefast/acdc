use std::path::Path;

use acdc_parser::{
    Options,
    ast::{BlockKind, BlockRef, InlineKind, InlineRef},
    parse,
};

type Error = Box<dyn std::error::Error>;

#[rstest::rstest]
fn quoted_ifeval_keeps_locations_after_nested_conditions(
    #[values(false, true)] included: bool,
) -> Result<(), Error> {
    let source = include_str!("../../fixtures/tests/ifeval_quoted_contexts.adoc");
    let input = if included {
        "include::ifeval_quoted_contexts.adoc[]"
    } else {
        source
    };
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/tests");
    let options = Options::builder()
        .with_safe_mode(acdc_parser::SafeMode::Unsafe)
        .with_base_dir(&fixtures)
        .build()?;
    let parsed = parse(input, &options)?;
    let Some(BlockKind::Section(section)) = parsed.document().blocks().last().map(BlockRef::kind)
    else {
        return Err("expected final section".into());
    };
    let Some(BlockKind::Paragraph(paragraph)) = section.blocks().last().map(BlockRef::kind) else {
        return Err("expected final paragraph".into());
    };
    let [InlineKind::Plain(text)] = &*paragraph
        .content()
        .inlines()
        .map(InlineRef::kind)
        .collect::<Vec<_>>()
    else {
        return Err("expected plain text".into());
    };
    let expected = "C11 Final content keeps its original source location.";
    assert_eq!(text.text(), expected);
    let line = u32::try_from(
        source
            .lines()
            .position(|line| line == expected)
            .ok_or("missing source paragraph")?
            + 1,
    )?;
    for location in [paragraph.location(), text.location()] {
        assert_eq!(location.start().line(), line);
        assert_eq!(location.end().line(), line);
        assert_eq!(location.start().column(), 1);
        assert_eq!(location.end().column(), u32::try_from(expected.len())?);
        if included {
            let path = location
                .start()
                .include_chain()
                .and_then(Iterator::last)
                .ok_or("missing included path")?;
            assert!(Path::new(path).ends_with("ifeval_quoted_contexts.adoc"));
            assert_eq!(
                location
                    .end()
                    .include_chain()
                    .map(Iterator::collect::<Vec<_>>),
                location
                    .start()
                    .include_chain()
                    .map(Iterator::collect::<Vec<_>>)
            );
        } else {
            assert_eq!(
                &source[location.absolute_start()..=location.absolute_end()],
                expected
            );
        }
    }
    Ok(())
}

#[rstest::rstest]
fn quoted_ifeval_keeps_caller_attribute_precedence(
    #[values(false, true)] locked: bool,
) -> Result<(), Error> {
    let source = include_str!("../../fixtures/tests/ifeval_quoted_contexts.adoc");
    // The fixture's quote-aware comparisons are Draft's reading; asciidoctor splits at the
    // first operator character (`asciidoctor_mode_evaluates_ifeval_as_asciidoctor`).
    let builder = Options::builder().with_compatibility_mode(acdc_parser::CompatibilityMode::Draft);
    let options = if locked {
        builder.with_attribute("state", "<early>").build()?
    } else {
        builder.with_default_attribute("state", "<early>").build()?
    };
    let parsed = parse(source, &options)?;
    // The blank line before the conditional ends the header; the accepted
    // assignment belongs to the body, even though its name says "header".
    assert!(!parsed.document().attributes().contains("header-result"));
    assert!(parsed.document().blocks().any(|block| matches!(
        block.kind(), BlockKind::DocumentAttribute(attribute) if attribute.name() == "header-result"
            && matches!(attribute.assignment(), acdc_parser::DocumentAttributeAssignment::Set(value)
                if value.text() == Some("accepted"))
    )));
    let old_value_matches = parsed
        .document()
        .blocks()
        .filter_map(|block| {
            if let BlockKind::Section(section) = block.kind() {
                Some(section.blocks())
            } else {
                None
            }
        })
        .flatten()
        .any(|block| {
            matches!(block.kind(), BlockKind::Paragraph(paragraph)
            if paragraph.content().inlines().any(|inline| matches!(inline.kind(),
                InlineKind::Plain(text) if text.text().starts_with("F01"))))
        });
    assert_eq!(old_value_matches, locked);
    Ok(())
}

/// Whether the body of `ifeval::[{expression}]` is kept.
fn ifeval_keeps(expression: &str, mode: acdc_parser::CompatibilityMode) -> Result<bool, Error> {
    let source = format!(":markup: <tag>\n\nifeval::[{expression}]\nKEPT\nendif::[]\n");
    let options = Options::builder().with_compatibility_mode(mode).build()?;
    let parsed = parse(&source, &options)?;
    Ok(parsed.document().blocks().count() > 0)
}

// asciidoctor 2.0.26 (measured 2026-11-16), `reader.rb` `preprocess_conditional_directive`
// and `resolve_expr_val`: the expression splits at its first operator (`EvalExpressionRx`,
// a lazy left operand), a quoted operand loses only its opening quote, references read
// the stored value, and an ordering between different types skips the body.
#[rstest::rstest]
#[case(r#""<tag>" == "<tag>""#, false)]
#[case("'<tag>' == '<tag>'", false)]
#[case(r#""a == b" == 'a == b'"#, true)]
#[case(r#""a < b" != "a > b""#, false)]
#[case(r#""a" == "a""#, true)]
#[case(r#""a" == 'a'"#, false)]
#[case(r#""abc" < "abd""#, true)]
#[case(r#""10" < '2'"#, true)]
#[case(r#""true" == 'true'"#, false)]
#[case("2 < 10", true)]
#[case("1.5 < 2", true)]
#[case("10 == 10.0", true)]
#[case("true == true", true)]
#[case("true < false", false)]
#[case(r#""x" < 2"#, false)]
#[case(r#""" == ''"#, false)]
#[case("{missing} == ''", false)]
#[case("'{markup}' == '<tag>'", false)]
#[case("x == x", true)]
#[case("abc == 0", true)]
#[case("3abc == 3", true)]
#[case(" 1 ==2", false)]
#[case(r#""same == value" != 'same == value'"#, true)]
#[case(r#"'z <= a' <= "a <= z""#, true)]
#[case(r#"'a >= z' >= "z >= a""#, true)]
fn asciidoctor_mode_evaluates_ifeval_as_asciidoctor(
    #[case] expression: &str,
    #[case] kept: bool,
) -> Result<(), Error> {
    assert_eq!(
        ifeval_keeps(expression, acdc_parser::CompatibilityMode::Asciidoctor)?,
        kept,
        "{expression}"
    );
    Ok(())
}

#[rstest::rstest]
#[case(r#""<tag>" == "<tag>""#, true)]
#[case(r#""same == value" != 'same == value'"#, false)]
#[case("2 < 10", true)]
fn draft_mode_keeps_its_ifeval_reading(#[case] expression: &str, #[case] kept: bool) -> Result<(), Error> {
    assert_eq!(ifeval_keeps(expression, acdc_parser::CompatibilityMode::Draft)?, kept, "{expression}");
    Ok(())
}
