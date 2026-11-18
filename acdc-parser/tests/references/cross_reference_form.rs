//! Whether a cross reference was written as `<<…>>` or `xref:…[]`. asciidoctor reads
//! the two forms differently — `<<guide.adoc>>` is an id, `xref:guide.adoc[]` a
//! document — so the model says which one the author wrote.

use acdc_parser::{Options, XrefForm, ast::InlineKind, parse_inline};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn each_cross_reference_records_the_form_it_was_written_in() -> TestResult {
    for (input, form) in [
        ("<<guide.adoc>>", XrefForm::Shorthand),
        ("<<guide.adoc#x,Guide>>", XrefForm::Shorthand),
        ("xref:guide.adoc[]", XrefForm::Macro),
        ("xref:#x[Here]", XrefForm::Macro),
    ] {
        let parsed = parse_inline(input, &Options::default())?;
        let forms: Vec<XrefForm> = parsed
            .content()
            .inlines()
            .filter_map(|inline| match inline.kind() {
                InlineKind::CrossReference(xref) => Some(xref.form()),
                _ => None,
            })
            .collect();
        assert_eq!(forms, [form], "{input}");
    }
    Ok(())
}
