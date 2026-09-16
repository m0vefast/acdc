//! Whether a cross reference was written as `<<…>>` or `xref:…[]`. asciidoctor reads
//! the two forms differently — `<<guide.adoc>>` is an id, `xref:guide.adoc[]` a
//! document — so the model says which one the author wrote.

use acdc_parser::{InlineMacro, InlineNode, Options, XrefForm, parse_inline};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn each_cross_reference_records_the_form_it_was_written_in() -> TestResult {
    for (input, form, json) in [
        ("<<guide.adoc>>", XrefForm::Shorthand, r#""form":"shorthand""#),
        ("<<guide.adoc#x,Guide>>", XrefForm::Shorthand, r#""form":"shorthand""#),
        ("xref:guide.adoc[]", XrefForm::Macro, r#""form":"macro""#),
        ("xref:#x[Here]", XrefForm::Macro, r#""form":"macro""#),
    ] {
        let parsed = parse_inline(input, &Options::default())?;
        let [InlineNode::Macro(InlineMacro::CrossReference(xref))] = parsed.inlines() else {
            return Err(std::io::Error::other(format!(
                "{input}: expected one cross reference, got {:?}",
                parsed.inlines()
            ))
            .into());
        };
        assert_eq!(xref.form, form, "{input}");
        let serialized = serde_json::to_string(parsed.inlines())?;
        assert!(serialized.contains(json), "{input}: {serialized}");
    }
    Ok(())
}
