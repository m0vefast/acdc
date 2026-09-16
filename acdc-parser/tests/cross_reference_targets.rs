//! Cross reference and link macro targets, as asciidoctor 2.0.26 reads them.

use acdc_parser::{InlineMacro, InlineNode, Options, parse_inline};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn unexpected(message: &str, actual: impl std::fmt::Debug) -> Box<dyn std::error::Error> {
    std::io::Error::other(format!("{message}, got {actual:?}")).into()
}

fn xref_target(input: &str) -> Result<String, Box<dyn std::error::Error>> {
    let parsed = parse_inline(input, &Options::default())?;
    let [InlineNode::Macro(InlineMacro::CrossReference(xref))] = parsed.inlines() else {
        return Err(unexpected(&format!("{input}: expected one cross reference"), parsed.inlines()));
    };
    Ok(xref.target.to_string())
}

#[test]
fn shorthand_targets_follow_asciidoctor() -> TestResult {
    for (input, target) in [
        ("<<#_local>>", "#_local"),
        ("<<.hidden.adoc#x>>", ".hidden.adoc#x"),
        ("<<../x.adoc#y>>", "../x.adoc#y"),
        ("<<01-intro.adoc#x>>", "01-intro.adoc#x"),
        ("<<my guide.adoc#x>>", "my guide.adoc#x"),
        ("<<设计.adoc#x>>", "设计.adoc#x"),
        ("<<:x>>", ":x"),
        ("<<x y,Text>>", "x y"),
    ] {
        assert_eq!(xref_target(input)?, target, "{input}");
    }
    Ok(())
}

#[test]
fn a_shorthand_target_may_not_start_with_whitespace() -> TestResult {
    let parsed = parse_inline("<< x>>", &Options::default())?;
    assert!(
        !parsed
            .inlines()
            .iter()
            .any(|inline| matches!(inline, InlineNode::Macro(InlineMacro::CrossReference(_)))),
        "{:?}",
        parsed.inlines()
    );
    Ok(())
}

#[test]
fn xref_macro_targets_follow_asciidoctor() -> TestResult {
    for (input, target) in [
        ("xref:../x.adoc#y[]", "../x.adoc#y"),
        ("xref:guide.adoc#v1.2[]", "guide.adoc#v1.2"),
        ("xref:my guide.adoc#x[]", "my guide.adoc#x"),
        ("xref:设计.adoc#x[]", "设计.adoc#x"),
    ] {
        assert_eq!(xref_target(input)?, target, "{input}");
    }
    Ok(())
}

#[test]
fn link_macro_targets_follow_asciidoctor() -> TestResult {
    for (input, target) in [
        ("link:Meeting%20Notes.pdf[]", "Meeting%20Notes.pdf"),
        ("link:设计.pdf[]", "设计.pdf"),
        ("link:../up.pdf[]", "../up.pdf"),
    ] {
        let parsed = parse_inline(input, &Options::default())?;
        let [InlineNode::Macro(InlineMacro::Link(link))] = parsed.inlines() else {
            return Err(unexpected(&format!("{input}: expected one link"), parsed.inlines()));
        };
        assert_eq!(link.target.to_string(), target, "{input}");
    }
    let parsed = parse_inline("link:a b.pdf[]", &Options::default())?;
    assert!(
        !parsed
            .inlines()
            .iter()
            .any(|inline| matches!(inline, InlineNode::Macro(InlineMacro::Link(_)))),
        "a link target may not contain whitespace: {:?}",
        parsed.inlines()
    );
    Ok(())
}
