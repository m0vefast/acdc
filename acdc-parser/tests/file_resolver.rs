//! End-to-end pin: with a custom FileResolver, `include::path[]` expands
//! the resolver-provided content instead of falling back to std::fs.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use acdc_parser::{DynFileResolver, FileResolver, FileResolverError, Options, parse};

/// In-memory file map. Trait requires Send+Sync so the inner map is
/// plain (no interior mutability).
struct InMemoryFiles {
    files: HashMap<PathBuf, Vec<u8>>,
}

impl InMemoryFiles {
    fn new(pairs: &[(&str, &str)]) -> Self {
        Self {
            files: pairs
                .iter()
                .map(|(k, v)| (PathBuf::from(k), v.as_bytes().to_vec()))
                .collect(),
        }
    }
}

impl FileResolver for InMemoryFiles {
    fn read(&self, path: &Path) -> Result<Cow<'_, [u8]>, FileResolverError> {
        match self.files.get(path) {
            Some(bytes) => Ok(Cow::Borrowed(bytes)),
            None => Err(FileResolverError::not_found(path)),
        }
    }
}

/// Resolver that always errors with `Io` (simulates a JS callback that
/// threw, a permission denied, or a transient backend failure).
struct AlwaysIoErr;
impl FileResolver for AlwaysIoErr {
    fn read(&self, path: &Path) -> Result<Cow<'_, [u8]>, FileResolverError> {
        Err(FileResolverError::io(
            path,
            std::io::Error::other("simulated backend failure"),
        ))
    }
}

#[test]
fn include_expands_via_resolver() {
    let resolver = DynFileResolver::new(InMemoryFiles::new(&[(
        "ch1.adoc",
        "== Chapter 1\n\nFirst paragraph of chapter 1.\n",
    )]));
    let opts = Options::builder()
        .with_file_resolver(resolver)
        .with_virtual_current_file("main.adoc")
        .build();
    let source = "= Main\n\nBefore.\n\ninclude::ch1.adoc[]\n\nAfter.\n";
    let result = parse(source, &opts).expect("parse ok");
    let blocks = &result.document().blocks;
    let has_chapter_heading = blocks.iter().any(|b| {
        if let acdc_parser::Block::Section(s) = b {
            return s.title.iter().any(|inline| {
                matches!(inline, acdc_parser::InlineNode::PlainText(p) if p.content == "Chapter 1")
            });
        }
        false
    });
    assert!(
        has_chapter_heading,
        "expected `Chapter 1` section after include expansion; got blocks: {blocks:#?}",
    );
}

#[test]
fn include_missing_file_warns_and_skips() {
    let resolver = DynFileResolver::new(InMemoryFiles::new(&[]));
    let opts = Options::builder()
        .with_file_resolver(resolver)
        .with_virtual_current_file("main.adoc")
        .build();
    let source = "= Main\n\ninclude::missing.adoc[]\n\nP-after.\n";
    let result = parse(source, &opts).expect("parse ok");
    let has_after = result.document().blocks.iter().any(|b| {
        if let acdc_parser::Block::Paragraph(p) = b {
            return p.content.iter().any(|inline| {
                matches!(inline, acdc_parser::InlineNode::PlainText(t) if t.content.contains("P-after"))
            });
        }
        false
    });
    assert!(
        has_after,
        "expected `P-after.` paragraph after dropped include"
    );
    assert!(
        !result.warnings().is_empty(),
        "expected at least one warning for the missing include",
    );
}

#[test]
fn include_with_lines_filter_via_resolver() {
    let resolver = DynFileResolver::new(InMemoryFiles::new(&[(
        "data.adoc",
        "line-1\nline-2\nline-3\nline-4\nline-5\n",
    )]));
    let opts = Options::builder()
        .with_file_resolver(resolver)
        .with_virtual_current_file("main.adoc")
        .build();
    let source = "= Doc\n\ninclude::data.adoc[lines=2..4]\n";
    let result = parse(source, &opts).expect("parse ok");
    let text: String = result
        .document()
        .blocks
        .iter()
        .filter_map(|b| {
            if let acdc_parser::Block::Paragraph(p) = b {
                Some(
                    p.content
                        .iter()
                        .filter_map(|i| {
                            if let acdc_parser::InlineNode::PlainText(t) = i {
                                Some(t.content.to_string())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" "),
                )
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("line-2"), "expected line-2, got: {text:?}");
    assert!(text.contains("line-3"), "expected line-3, got: {text:?}");
    assert!(text.contains("line-4"), "expected line-4, got: {text:?}");
    assert!(
        !text.contains("line-1"),
        "line-1 should be excluded, got: {text:?}"
    );
    assert!(
        !text.contains("line-5"),
        "line-5 should be excluded, got: {text:?}"
    );
}

#[test]
fn cyclic_include_bounded_by_depth_limit_no_stack_overflow() {
    // a.adoc → b.adoc → a.adoc → b.adoc → … — without depth guard this
    // recurses until the wasm stack overflows (trap → JS exception → dead
    // module). With the depth limit (64), it terminates with a warning.
    let resolver = DynFileResolver::new(InMemoryFiles::new(&[
        ("a.adoc", "== A\n\ninclude::b.adoc[]\n"),
        ("b.adoc", "== B\n\ninclude::a.adoc[]\n"),
    ]));
    let opts = Options::builder()
        .with_file_resolver(resolver)
        .with_virtual_current_file("main.adoc")
        .build();
    let source = "= Main\n\ninclude::a.adoc[]\n";
    let result = parse(source, &opts).expect("parse ok despite cycle");
    let depth_warning = result
        .warnings()
        .iter()
        .any(|w| w.kind.to_string().contains("include depth exceeded"));
    assert!(
        depth_warning,
        "expected a depth-exceeded warning; got: {:?}",
        result.warnings(),
    );
}

#[test]
fn io_error_from_resolver_surfaces_cause_string_in_warning() {
    // An Io error (JS callback threw, permission denied, …) is distinct
    // from NotFound; both warn and skip but the warning text MUST carry the
    // underlying cause so consumers can debug the failure. Pin both:
    //   1. The generic missing-file phrasing is NOT used.
    //   2. The resolver's source error message ("simulated backend failure")
    //      appears verbatim in the warning text via Error::source chain.
    let resolver = DynFileResolver::new(AlwaysIoErr);
    let opts = Options::builder()
        .with_file_resolver(resolver)
        .with_virtual_current_file("main.adoc")
        .build();
    // No `opts=optional` — should warn.
    let source = "= Main\n\ninclude::foo.adoc[]\n\nP-after.\n";
    let result = parse(source, &opts).expect("parse ok");
    let warning_texts: Vec<String> = result
        .warnings()
        .iter()
        .map(|w| w.kind.to_string())
        .collect();
    assert!(
        warning_texts
            .iter()
            .any(|t| t.contains("include read failed")),
        "expected `include read failed` phrasing for Io (non-NotFound), got: {warning_texts:?}",
    );
    assert!(
        !warning_texts.iter().any(|t| t.contains("file is missing")),
        "Io error must NOT be reported as generic missing-file, got: {warning_texts:?}",
    );
    assert!(
        warning_texts
            .iter()
            .any(|t| t.contains("simulated backend failure")),
        "expected the source cause string in the warning text, got: {warning_texts:?}",
    );
    // Document still renders.
    assert!(result.document().blocks.iter().any(|b| {
        if let acdc_parser::Block::Paragraph(p) = b {
            return p.content.iter().any(|inline| {
                matches!(inline, acdc_parser::InlineNode::PlainText(t) if t.content.contains("P-after"))
            });
        }
        false
    }));
}

#[test]
fn relative_dotdot_path_normalized_for_resolver_lookup() {
    // Resolver is keyed by `"part1/sib.adoc"` (normalized). The include
    // directive uses `"../part1/sib.adoc"` relative to a sibling file —
    // without lexical normalization the resolver would be queried with
    // `"part1/sub/../sib.adoc"` and miss. Confirms `..` collapse.
    let resolver = DynFileResolver::new(InMemoryFiles::new(&[(
        "part1/sib.adoc",
        "== Sibling\n\nSib body.\n",
    )]));
    let opts = Options::builder()
        .with_file_resolver(resolver)
        .with_virtual_current_file("part1/sub/main.adoc")
        .build();
    let source = "= Doc\n\ninclude::../sib.adoc[]\n";
    let result = parse(source, &opts).expect("parse ok");
    let has_sibling = result.document().blocks.iter().any(|b| {
        if let acdc_parser::Block::Section(s) = b {
            return s.title.iter().any(|inline| {
                matches!(inline, acdc_parser::InlineNode::PlainText(p) if p.content == "Sibling")
            });
        }
        false
    });
    assert!(
        has_sibling,
        "expected `Sibling` section after relative-dotdot resolution; warnings: {:?}",
        result.warnings(),
    );
}

#[test]
fn absolute_path_preserved_for_resolver_lookup() {
    // Glyph's real-app case: Swift sends `virtual_current_file =
    // "/Users/.../adoc/main.adoc"`. Relative include must join into an
    // absolute path that matches the resolver's absolute-path keys.
    let resolver = DynFileResolver::new(InMemoryFiles::new(&[(
        "/Users/me/vault/adoc/ch1.adoc",
        "== Chapter 1\n\nAbs body.\n",
    )]));
    let opts = Options::builder()
        .with_file_resolver(resolver)
        .with_virtual_current_file("/Users/me/vault/adoc/main.adoc")
        .build();
    let source = "= Main\n\ninclude::ch1.adoc[]\n";
    let result = parse(source, &opts).expect("parse ok");
    let has_chapter = result.document().blocks.iter().any(|b| {
        if let acdc_parser::Block::Section(s) = b {
            return s.title.iter().any(|inline| {
                matches!(inline, acdc_parser::InlineNode::PlainText(p) if p.content == "Chapter 1")
            });
        }
        false
    });
    assert!(
        has_chapter,
        "expected `Chapter 1` after absolute-path include; warnings: {:?}",
        result.warnings(),
    );
}

#[test]
fn nested_include_two_levels_resolves_via_resolver() {
    // outer.adoc includes inner.adoc — both go through the resolver.
    let resolver = DynFileResolver::new(InMemoryFiles::new(&[
        ("outer.adoc", "== Outer\n\ninclude::inner.adoc[]\n"),
        ("inner.adoc", "=== Inner\n\nInner body.\n"),
    ]));
    let opts = Options::builder()
        .with_file_resolver(resolver)
        .with_virtual_current_file("main.adoc")
        .build();
    let source = "= Main\n\ninclude::outer.adoc[]\n";
    let result = parse(source, &opts).expect("parse ok");
    let titles: Vec<String> = result
        .document()
        .blocks
        .iter()
        .filter_map(|b| {
            if let acdc_parser::Block::Section(s) = b {
                let inline_text: String = s
                    .title
                    .iter()
                    .filter_map(|i| {
                        if let acdc_parser::InlineNode::PlainText(t) = i {
                            Some(t.content.to_string())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("");
                Some(inline_text)
            } else {
                None
            }
        })
        .collect();
    assert!(
        titles.iter().any(|t| t == "Outer"),
        "expected `Outer` section; got titles: {titles:?}",
    );
}

#[test]
fn missing_virtual_current_file_with_resolver_warns_via_parse_result() {
    // Caller set a file_resolver but forgot virtual_current_file — every
    // include silently dropped previously (tracing::error! only). Now it
    // should surface a warning on ParseResult so consumers can spot the
    // misconfiguration.
    let resolver = DynFileResolver::new(InMemoryFiles::new(&[("ch1.adoc", "== Chapter 1\n")]));
    let opts = Options::builder().with_file_resolver(resolver).build();
    let source = "= Main\n\ninclude::ch1.adoc[]\n";
    let result = parse(source, &opts).expect("parse ok");
    assert!(
        result
            .warnings()
            .iter()
            .any(|w| w.kind.to_string().contains("no file context")),
        "expected `no file context` warning, got: {:?}",
        result.warnings(),
    );
}

#[test]
fn include_lines_negative_end_works() {
    let resolver = DynFileResolver::new(InMemoryFiles::new(&[(
        "data.adoc",
        "alpha\nbeta\ngamma\ndelta\nepsilon\n",
    )]));
    let opts = Options::builder()
        .with_file_resolver(resolver)
        .with_virtual_current_file("main.adoc")
        .build();
    // `lines=2..-1` should include lines 2 to end (beta, gamma, delta, epsilon).
    let source = "= Main\n\ninclude::data.adoc[lines=2..-1]\n";
    let result = parse(source, &opts).expect("parse ok");
    let serialized = serde_json::to_string(result.document()).expect("serialize");
    eprintln!("DOC: {serialized}");
    eprintln!("WARNINGS: {:?}", result.warnings());
    assert!(
        serialized.contains("beta"),
        "missing 'beta' in:\n{serialized}"
    );
    assert!(
        serialized.contains("epsilon"),
        "missing 'epsilon' in:\n{serialized}"
    );
    assert!(
        !serialized.contains("alpha"),
        "'alpha' should be excluded:\n{serialized}"
    );
}

#[test]
fn include_lines_positive_end_works() {
    let resolver = DynFileResolver::new(InMemoryFiles::new(&[(
        "data.adoc",
        "alpha\nbeta\ngamma\ndelta\nepsilon\n",
    )]));
    let opts = Options::builder()
        .with_file_resolver(resolver)
        .with_virtual_current_file("main.adoc")
        .build();
    let source = "= Main\n\ninclude::data.adoc[lines=2..4]\n";
    let result = parse(source, &opts).expect("parse ok");
    let serialized = serde_json::to_string(result.document()).expect("serialize");
    eprintln!("POS DOC: {serialized}");
    eprintln!("POS WARNINGS: {:?}", result.warnings());
    assert!(serialized.contains("beta"), "missing beta");
}

#[test]
fn include_lines_neg_n_counts_from_end() {
    let resolver =
        DynFileResolver::new(InMemoryFiles::new(&[("data.adoc", "L1\nL2\nL3\nL4\nL5\n")]));
    let opts = Options::builder()
        .with_file_resolver(resolver)
        .with_virtual_current_file("main.adoc")
        .build();
    // lines=2..-2 → lines 2 to second-to-last (L2, L3, L4).
    let result = parse("= M\n\ninclude::data.adoc[lines=2..-2]\n", &opts).expect("parse");
    let s = serde_json::to_string(result.document()).unwrap();
    assert!(s.contains("L2"), "missing L2: {s}");
    assert!(s.contains("L4"), "missing L4: {s}");
    assert!(!s.contains("L5"), "L5 should be excluded: {s}");
    assert!(!s.contains("L1"), "L1 should be excluded: {s}");
}

#[test]
fn include_lines_neg_n_out_of_range_skipped() {
    let resolver = DynFileResolver::new(InMemoryFiles::new(&[("data.adoc", "L1\nL2\n")]));
    let opts = Options::builder()
        .with_file_resolver(resolver)
        .with_virtual_current_file("main.adoc")
        .build();
    // -10 underflows; treat as no valid end → no content, no panic.
    let result = parse("= M\n\ninclude::data.adoc[lines=1..-10]\n", &opts);
    assert!(result.is_ok(), "must not panic");
}

#[test]
fn include_lines_end_past_eof_should_clamp() {
    // Asciidoctor behavior: end-line past EOF clamps to EOF, doesn't drop.
    // e.g. file has 30 lines, lines=10..50 → include lines 10..30 (clamped).
    let mut content = String::new();
    for i in 1..=30 {
        content.push_str(&format!("L{i}\n"));
    }
    let resolver = DynFileResolver::new(InMemoryFiles::new(&[("data.adoc", &content)]));
    let opts = Options::builder()
        .with_file_resolver(resolver)
        .with_virtual_current_file("main.adoc")
        .build();
    let result = parse("= M\n\ninclude::data.adoc[lines=10..50]\n", &opts).expect("parse");
    let s = serde_json::to_string(result.document()).unwrap();
    eprintln!("PAST-EOF: {s}");
    assert!(s.contains("L10"), "missing L10 (start of range): {s}");
    assert!(s.contains("L30"), "missing L30 (last actual line): {s}");
    assert!(
        !s.contains("L9"),
        "L9 (before range) should not appear: {s}"
    );
}

// ── original-source line mapping through include + conditional (location remap)
//
// The IncludeExpansion / conditional_drops line-map accessors were removed when
// upstream's location remap (0ad5c19) landed: the parser now rewrites every
// node's location to original-source coordinates itself, so an embedder reads
// the correct line directly off each block instead of post-compensating. This
// test pins that remap works through Glyph's `FileResolver` virtual-FS path
// (our fork-specific integration, not covered by upstream's own remap tests):
// content AFTER an include — including after a nested conditional that dropped
// lines inside the included file — reports its ORIGINAL root-source line.
#[test]
fn block_after_include_reports_original_root_source_line() {
    // The child drops 3 lines via a false ifdef, keeping 1 line. Without
    // remap, `After.` at root line 5 would drift by the preprocessor's
    // line shifts; with remap it must report line 5 exactly.
    let resolver = DynFileResolver::new(InMemoryFiles::new(&[(
        "child.adoc",
        "ifdef::missing[]\nhidden line\nendif::[]\nKept by child.\n",
    )]));
    let opts = Options::builder()
        .with_file_resolver(resolver)
        .with_virtual_current_file("main.adoc")
        .build();
    // Root source lines: 1=`= M`, 2=``, 3=`include::child.adoc[]`, 4=``, 5=`After.`
    let r = acdc_parser::parse("= M\n\ninclude::child.adoc[]\n\nAfter.\n", &opts).expect("parse");
    let after_block = r
        .document()
        .blocks
        .iter()
        .find(|b| {
            if let acdc_parser::Block::Paragraph(p) = b {
                p.content.iter().any(|i| {
                    matches!(i,
                        acdc_parser::InlineNode::PlainText(t) if t.content.contains("After.")
                    )
                })
            } else {
                false
            }
        })
        .expect("After. paragraph must exist in root doc");
    if let acdc_parser::Block::Paragraph(p) = after_block {
        assert_eq!(
            p.location.start.line, 5,
            "`After.` MUST report root-source line 5 (location remap through FileResolver)"
        );
    }
}
