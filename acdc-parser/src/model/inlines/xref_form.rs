use serde::Serialize;

/// How an author wrote a cross reference, in every compatibility mode (unlike
/// `XrefSourceSyntax`, which legacy controls collapse to `Literal`). asciidoctor reads the
/// two forms differently: `<<guide.adoc>>` is an id, `xref:guide.adoc[]` a document.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum XrefForm {
    /// `<<target>>` or `<<target,text>>`, and references built outside the parser.
    #[default]
    Shorthand,
    /// `xref:target[text]`.
    Macro,
}
