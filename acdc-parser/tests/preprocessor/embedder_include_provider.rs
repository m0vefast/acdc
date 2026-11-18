//! An embedder's own include source (Glyph serves includes from its vault and from a
//! JavaScript callback): what reaches the provider, what the document holds after,
//! and what the warnings tell when a source fails.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use acdc_parser::{
    IncludeLoader, IncludeSource, IncludeSourceError, IncludeSourceErrorKind, IncludeSourceTarget, Options,
    ParseResult, SafeMode,
    ast::{BlockKind, ContentRef, InlineKind},
    parse,
};

const BASE: &str = "/vault/docs";

fn files(pairs: &[(&str, &str)]) -> IncludeLoader {
    let files: HashMap<PathBuf, String> =
        pairs.iter().map(|(path, text)| (Path::new(BASE).join(path), (*text).to_string())).collect();
    IncludeLoader::custom(move |target: &IncludeSourceTarget| match target {
        IncludeSourceTarget::File(path) => files
            .get(path)
            .map(|text| IncludeSource::from_string(text.clone()))
            .ok_or_else(|| IncludeSourceError::new(IncludeSourceErrorKind::NotFound, format!("not found: {}", path.display()))),
        _ => Err(IncludeSourceError::new(IncludeSourceErrorKind::Unsupported, "unsupported include target")),
    })
}

fn parse_with(source: &str, loader: IncludeLoader, base: &str) -> ParseResult {
    let options = Options::builder()
        .with_safe_mode(SafeMode::Unsafe)
        .with_include_loader(loader)
        .with_base_dir(base)
        .build()
        .expect("options");
    parse(source, &options).expect("parse")
}

fn text(content: Option<ContentRef<'_>>) -> String {
    content
        .into_iter()
        .flat_map(ContentRef::inlines)
        .filter_map(|inline| match inline.kind() {
            InlineKind::Plain(plain) => Some(plain.text().to_string()),
            _ => None,
        })
        .collect()
}

/// `("section" | "p", text)` of every top-level block, sections recursively.
fn shape(result: &ParseResult) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for node in result.document().descendants() {
        let acdc_parser::ast::StructuralRef::Block(block) = node else { continue };
        match block.kind() {
            BlockKind::Section(_) => out.push(("section".to_string(), text(block.title()))),
            BlockKind::Paragraph(paragraph) => out.push(("p".to_string(), text(Some(paragraph.content())))),
            _ => {}
        }
    }
    out
}

fn paragraphs(result: &ParseResult) -> Vec<String> {
    shape(result).into_iter().filter(|(kind, _)| kind == "p").map(|(_, text)| text).collect()
}

fn warnings(result: &ParseResult) -> Vec<String> {
    result.warnings().iter().map(|warning| warning.kind.to_string()).collect()
}

fn pair(kind: &str, text: &str) -> (String, String) {
    (kind.to_string(), text.to_string())
}

#[test]
fn include_expands_through_the_provider() {
    let result = parse_with(
        "= Main\n\nBefore.\n\ninclude::ch1.adoc[]\n\nAfter.\n",
        files(&[("ch1.adoc", "== Chapter 1\n\nFirst paragraph of chapter 1.\n")]),
        BASE,
    );
    assert!(shape(&result).contains(&pair("section", "Chapter 1")), "{:?}", shape(&result));
}

#[test]
fn a_missing_include_warns_and_the_document_continues() {
    let result = parse_with("= Main\n\ninclude::missing.adoc[]\n\nP-after.\n", files(&[]), BASE);
    assert!(paragraphs(&result).iter().any(|text| text.contains("P-after")), "{:?}", shape(&result));
    assert!(warnings(&result).iter().any(|w| w.contains("include file not found")), "{:?}", warnings(&result));
}

#[test]
fn a_failing_provider_s_cause_reaches_the_warning() {
    let loader = IncludeLoader::custom(|_: &IncludeSourceTarget| {
        Err(IncludeSourceError::new(IncludeSourceErrorKind::Unavailable, "js resolver threw: simulated backend failure"))
    });
    let result = parse_with("= Main\n\ninclude::foo.adoc[]\n\nP-after.\n", loader, BASE);
    let warnings = warnings(&result);
    assert_eq!(
        warnings,
        [format!("include file not readable: {BASE}/foo.adoc (js resolver threw: simulated backend failure)")]
    );
    assert!(paragraphs(&result).iter().any(|text| text.contains("P-after")), "{:?}", shape(&result));
}

/// On the default test thread: the cycle runs to the default depth and returns.
#[test]
fn a_cyclic_include_is_bounded_by_the_depth_limit() {
    let result = parse_with(
        "= Main\n\ninclude::a.adoc[]\n",
        files(&[("a.adoc", "== A\n\ninclude::b.adoc[]\n"), ("b.adoc", "== B\n\ninclude::a.adoc[]\n")]),
        BASE,
    );
    let warnings = warnings(&result);
    assert!(warnings.iter().any(|w| w.contains("maximum include depth of 64 exceeded")), "{warnings:?}");
}

#[test]
fn a_relative_dotdot_target_reaches_the_provider_normalized() {
    let result = parse_with(
        "= Doc\n\ninclude::../sib.adoc[]\n",
        files(&[("part1/sib.adoc", "== Sibling\n\nSib body.\n")]),
        &format!("{BASE}/part1/sub"),
    );
    assert!(shape(&result).contains(&pair("section", "Sibling")), "{:?} {:?}", shape(&result), warnings(&result));
}

#[test]
fn nested_includes_resolve_through_the_provider() {
    let result = parse_with(
        "= Main\n\ninclude::outer.adoc[]\n",
        files(&[("outer.adoc", "== Outer\n\ninclude::inner.adoc[]\n"), ("inner.adoc", "=== Inner\n\nInner body.\n")]),
        BASE,
    );
    let shape = shape(&result);
    assert!(shape.contains(&pair("section", "Outer")) && shape.contains(&pair("section", "Inner")), "{shape:?}");
}

/// asciidoctor 2.0.26: a range selects its lines; any negative end runs to the end of
/// the file; an end past the end clamps to it.
#[test]
fn line_ranges_select_as_asciidoctor_selects() {
    let five = "L1\nL2\nL3\nL4\nL5\n";
    let thirty: String = (1..=30).map(|n| format!("L{n}\n")).collect();
    for (range, data, expected) in [
        ("2..4", five, "L2 L3 L4"),
        ("2..-1", five, "L2 L3 L4 L5"),
        ("2..-2", five, "L2 L3 L4 L5"),
        ("1..-10", five, "L1 L2 L3 L4 L5"),
        ("10..50", thirty.as_str(), &(10..=30).map(|n| format!("L{n}")).collect::<Vec<_>>().join(" ")),
    ] {
        let result = parse_with(&format!("= M\n\ninclude::data.adoc[lines={range}]\n"), files(&[("data.adoc", data)]), BASE);
        let words = paragraphs(&result).join(" ").split_whitespace().collect::<Vec<_>>().join(" ");
        assert_eq!(words, expected, "lines={range}");
    }
}

/// Content after an include whose file dropped lines in a false conditional keeps its
/// own line in the root source.
#[test]
fn a_block_after_an_include_reports_its_root_source_line() {
    let result = parse_with(
        "= M\n\ninclude::child.adoc[]\n\nAfter.\n",
        files(&[("child.adoc", "ifdef::missing[]\nhidden line\nendif::[]\nKept by child.\n")]),
        BASE,
    );
    let after = result
        .document()
        .blocks()
        .find(|block| matches!(block.kind(), BlockKind::Paragraph(p) if text(Some(p.content())).contains("After.")))
        .expect("the After. paragraph");
    let location = result.resolve_location(after.location());
    assert_eq!(location.file, None);
    assert_eq!(location.location.start.line, 5);
}
