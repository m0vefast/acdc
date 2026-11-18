//! Cross reference and link macro targets, as asciidoctor 2.0.26 reads them.

use acdc_parser::{Options, ast::InlineKind, parse_inline};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn xref_targets(input: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let parsed = parse_inline(input, &Options::default())?;
    Ok(parsed
        .content()
        .inlines()
        .filter_map(|inline| match inline.kind() {
            InlineKind::CrossReference(xref) => Some(xref.written_target().to_string()),
            _ => None,
        })
        .collect())
}

fn link_targets(input: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let parsed = parse_inline(input, &Options::default())?;
    Ok(parsed
        .content()
        .inlines()
        .filter_map(|inline| match inline.kind() {
            InlineKind::Link(link) => Some(link.target().to_string()),
            _ => None,
        })
        .collect())
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
        assert_eq!(xref_targets(input)?, [target], "{input}");
    }
    Ok(())
}

#[test]
fn a_shorthand_target_may_not_start_with_whitespace() -> TestResult {
    assert_eq!(xref_targets("<< x>>")?, Vec::<String>::new());
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
        assert_eq!(xref_targets(input)?, [target], "{input}");
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
        assert_eq!(link_targets(input)?, [target], "{input}");
    }
    assert_eq!(link_targets("link:a b.pdf[]")?, Vec::<String>::new(), "a link target may not contain whitespace");
    Ok(())
}
