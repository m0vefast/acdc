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
    assert!(has_after, "expected `P-after.` paragraph after dropped include");
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
    assert!(!text.contains("line-1"), "line-1 should be excluded, got: {text:?}");
    assert!(!text.contains("line-5"), "line-5 should be excluded, got: {text:?}");
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
        warning_texts.iter().any(|t| t.contains("include read failed")),
        "expected `include read failed` phrasing for Io (non-NotFound), got: {warning_texts:?}",
    );
    assert!(
        !warning_texts
            .iter()
            .any(|t| t.contains("file is missing")),
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
    let resolver = DynFileResolver::new(InMemoryFiles::new(&[(
        "ch1.adoc",
        "== Chapter 1\n",
    )]));
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
    assert!(serialized.contains("beta"), "missing 'beta' in:\n{serialized}");
    assert!(serialized.contains("epsilon"), "missing 'epsilon' in:\n{serialized}");
    assert!(!serialized.contains("alpha"), "'alpha' should be excluded:\n{serialized}");
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
    let resolver = DynFileResolver::new(InMemoryFiles::new(&[(
        "data.adoc",
        "L1\nL2\nL3\nL4\nL5\n",
    )]));
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
    let resolver = DynFileResolver::new(InMemoryFiles::new(&[(
        "data.adoc",
        "L1\nL2\n",
    )]));
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
    assert!(!s.contains("L9"), "L9 (before range) should not appear: {s}");
}

#[test]
fn include_expansions_reports_correct_line_counts_per_directive() {
    // Three includes with different filter shapes — each entry in
    // include_expansions must report (source_line, actual_expanded_lines)
    // matching what acdc fed into the preprocessed output.
    let ch1 = "L1\nL2\nL3\nL4\nL5\nL6\nL7\nL8\nL9\nL10\n";
    let ch2 = "// tag::a[]\nA-line-1\nA-line-2\n// end::a[]\nuntagged\n";
    let resolver = DynFileResolver::new(InMemoryFiles::new(&[
        ("ch1.adoc", ch1),
        ("ch2.adoc", ch2),
    ]));
    let opts = Options::builder()
        .with_file_resolver(resolver)
        .with_virtual_current_file("main.adoc")
        .build();
    // Lines: 1 = title, 2 = blank, 3 = include full (10 lines),
    //        4 = blank, 5 = include with lines= (3 lines: L2/L3/L4),
    //        6 = blank, 7 = include with tag= (2 lines: A-line-1, A-line-2)
    let src = "= M\n\ninclude::ch1.adoc[]\n\ninclude::ch1.adoc[lines=2..4]\n\ninclude::ch2.adoc[tag=a]\n";
    let r = acdc_parser::parse(src, &opts).expect("parse");
    let exps = r.include_expansions();
    eprintln!("include_expansions: {exps:#?}");
    assert_eq!(exps.len(), 3, "expected one entry per root-level include");
    // Source lines: 3, 5, 7 (1-based main-source line of each include::).
    assert_eq!(exps[0].source_line, 3);
    assert_eq!(exps[1].source_line, 5);
    assert_eq!(exps[2].source_line, 7);
    // Expanded line counts: full ch1 = 10, lines=2..4 = 3, tag=a = 2.
    assert_eq!(exps[0].expanded_lines, 10, "full include should be 10 lines");
    assert_eq!(exps[1].expanded_lines, 3, "lines=2..4 should be 3 lines");
    assert_eq!(exps[2].expanded_lines, 2, "tag=a should be 2 lines");
}

#[test]
fn include_expansions_reports_zero_for_missing_include() {
    // Missing include → directive line is dropped from preprocessed output.
    // The entry MUST still appear with expanded_lines=0 so consumers can
    // compensate: every line after the include shifts back by 1 (the
    // include line disappeared), and without an entry the consumer can't
    // translate post-expansion positions correctly.
    let resolver = DynFileResolver::new(InMemoryFiles::new(&[]));
    let opts = Options::builder()
        .with_file_resolver(resolver)
        .with_virtual_current_file("main.adoc")
        .build();
    let r = acdc_parser::parse("= M\n\ninclude::missing.adoc[]\n", &opts).expect("parse");
    let exps = r.include_expansions();
    assert_eq!(exps.len(), 1, "missing include must still produce an entry");
    assert_eq!(exps[0].source_line, 3);
    assert_eq!(exps[0].expanded_lines, 0);
}

// ── conditional_drops (ifdef / ifndef / ifeval / endif) ─────────────────────
//
// Same purpose as `include_expansions`: every preprocessor-induced source→
// output line shift must be reported so an embedder can translate
// post-expansion positions back to original source lines. Conditionals
// disappear from the output (the directive lines themselves always; their
// content too when the condition is false), so the embedder needs the list
// of dropped source line numbers to compensate the same way.

#[test]
fn conditional_drops_ifdef_false_drops_directive_content_and_endif() {
    // attribute `foo` not set → ifdef::foo[] block dropped entirely.
    // Source lines 3 (ifdef), 4 (content), 5 (endif) — 3 drops total so
    // every output line after maps back +3.
    let src = "= M\n\nifdef::foo[]\nhidden\nendif::[]\n\nAfter.\n";
    let opts = Options::builder().build();
    let r = acdc_parser::parse(src, &opts).expect("parse");
    assert_eq!(r.conditional_drops(), &[3usize, 4, 5][..]);
}

#[test]
fn conditional_drops_ifdef_true_keeps_content_records_directive_drop_only() {
    // `foo` is defined → ifdef block kept. Multi-line kept content is
    // pushed as one string ending in `\n`, which after `join("\n")`
    // contributes one extra (empty) output line; that extra line
    // silently swallows the endif drop, so we only record ifdef.
    // Source: 3=ifdef, 4=content, 5=endif → 3 source consumed,
    // 2 output produced (content + trailing empty) → 1 drop @ line 3.
    let src = "= M\n\nifdef::foo[]\nkept\nendif::[]\n\nAfter.\n";
    let opts = Options::builder()
        .with_attribute("foo", "")
        .build();
    let r = acdc_parser::parse(src, &opts).expect("parse");
    assert_eq!(r.conditional_drops(), &[3usize][..]);
}

#[test]
fn conditional_drops_single_line_ifdef_true_records_no_drop() {
    // Single-line `ifdef::foo[content]` form: 1 source line → 1 output
    // line when condition is true → no drops needed.
    let src = "= M\n\nifdef::foo[hello]\n\nAfter.\n";
    let opts = Options::builder()
        .with_attribute("foo", "")
        .build();
    let r = acdc_parser::parse(src, &opts).expect("parse");
    assert!(r.conditional_drops().is_empty());
}

#[test]
fn conditional_drops_single_line_ifdef_false_records_one_drop() {
    // Single-line form, false condition → 1 source line dropped.
    let src = "= M\n\nifdef::foo[hello]\n\nAfter.\n";
    let opts = Options::builder().build();
    let r = acdc_parser::parse(src, &opts).expect("parse");
    assert_eq!(r.conditional_drops(), &[3usize][..]);
}

#[test]
fn conditional_drops_ifndef_false_branch_records_drops() {
    // `ifndef::foo[]` is true when `foo` is NOT set. Inverted-sense
    // sibling of ifdef — same accounting rules apply.
    let src = "= M\n\nifndef::foo[]\nshown\nendif::[]\n\nAfter.\n";
    let opts = Options::builder()
        .with_attribute("foo", "")
        .build();
    let r = acdc_parser::parse(src, &opts).expect("parse");
    // foo IS set, so ifndef::foo[] block is dropped: 3 source lines.
    assert_eq!(r.conditional_drops(), &[3usize, 4, 5][..]);
}

#[test]
fn conditional_drops_ifeval_false_drops_like_ifdef_false() {
    // `ifeval::[]` shares the conditional dispatch path with ifdef/ifndef.
    // Pin that the drop accounting is identical: false ifeval drops all
    // consumed source lines (directive + content + endif).
    let src = "= M\n\nifeval::[1 == 2]\nunreachable\nendif::[]\n\nAfter.\n";
    let opts = Options::builder().build();
    let r = acdc_parser::parse(src, &opts).expect("parse");
    assert_eq!(r.conditional_drops(), &[3usize, 4, 5][..]);
}

#[test]
fn conditional_drops_ifeval_true_records_directive_drop_only() {
    // True ifeval keeps content; same kept-multiline trailing-`\n` quirk as
    // ifdef-true → only the directive line drops.
    let src = "= M\n\nifeval::[1 == 1]\nkept\nendif::[]\n\nAfter.\n";
    let opts = Options::builder().build();
    let r = acdc_parser::parse(src, &opts).expect("parse");
    assert_eq!(r.conditional_drops(), &[3usize][..]);
}

#[test]
fn conditional_drops_inside_included_file_NOT_propagated() {
    // CORRECTNESS INVARIANT (not a limitation): the nested preprocessor that
    // runs for an included file has its own `PreprocessorState`; only the
    // root document's `conditional_drops` reaches `PreprocessorResult`.
    // Conditionals INSIDE an included file drop lines from the merged output,
    // but those drops are already baked into the parent's
    // `IncludeExpansion::expanded_lines` (= post-drop line count). The
    // root-level translator computes a per-include shift of
    // `expanded_lines - 1`, which is correct regardless of how many lines
    // the nested preprocessor dropped — the post-drop count is what landed
    // in the merged output.
    //
    // Pin this as the API guarantee — consumers (Glyph wasm bridge +
    // JS translator) rely on `include_expansions[].expanded_lines` to
    // already account for nested-conditional drops. If a future refactor
    // double-counts by also bubbling nested drops up to
    // `conditional_drops`, this test breaks and the embedder side MUST
    // de-duplicate.
    let resolver = DynFileResolver::new(InMemoryFiles::new(&[(
        "child.adoc",
        "ifdef::missing[]\nhidden line\nendif::[]\nKept by child.\n",
    )]));
    let opts = Options::builder()
        .with_file_resolver(resolver)
        .with_virtual_current_file("main.adoc")
        .build();
    let r = acdc_parser::parse("= M\n\ninclude::child.adoc[]\n", &opts).expect("parse");
    // Root has ONE include expansion (the include directive), no conditional drops at root.
    assert_eq!(r.conditional_drops().len(), 0,
        "nested conditional drops are NOT propagated (known limitation)");
    // The include expansion reports the merged output line count. With the
    // ifdef:false eating 3 lines, the included file contributes 1 line ("Kept by child.").
    assert_eq!(r.include_expansions().len(), 1);
    assert_eq!(r.include_expansions()[0].source_line, 3);
    assert_eq!(r.include_expansions()[0].expanded_lines, 1);
}

#[test]
fn nested_conditional_drop_does_not_drift_block_line_after_include() {
    // Correctness proof for the invariant pinned in the test above. Build a
    // child with a false ifdef that swallows 3 lines, then a root document
    // with a paragraph AFTER the include. The paragraph's reported line
    // number must equal its ORIGINAL root-source line (not shifted by the
    // 3 lines the child dropped) — because `expanded_lines` already accounts
    // for the drop and the root translator's per-include shift uses the
    // post-drop count.
    let resolver = DynFileResolver::new(InMemoryFiles::new(&[(
        "child.adoc",
        "ifdef::missing[]\nhidden line\nendif::[]\nKept by child.\n",
    )]));
    let opts = Options::builder()
        .with_file_resolver(resolver)
        .with_virtual_current_file("main.adoc")
        .build();
    // Root source lines:
    //   1: `= M`
    //   2: ``
    //   3: `include::child.adoc[]`     → expands to 1 line ("Kept by child.")
    //   4: ``
    //   5: `After.`
    let r = acdc_parser::parse(
        "= M\n\ninclude::child.adoc[]\n\nAfter.\n",
        &opts,
    )
    .expect("parse");
    assert_eq!(r.include_expansions().len(), 1);
    assert_eq!(r.include_expansions()[0].source_line, 3);
    assert_eq!(
        r.include_expansions()[0].expanded_lines, 1,
        "expanded_lines must reflect post-drop count, baking in nested drops"
    );
    assert_eq!(
        r.conditional_drops().len(),
        0,
        "nested drops are NOT re-counted at root level"
    );
    // R1-review hardening: actually walk the parsed blocks and assert
    // `After.` reports its ORIGINAL root-source line (5). Without this the
    // test only pins envelope metadata — the surrounding contract — but not
    // the observable line-mapping. A regression that bubbles nested drops up
    // would still let the metadata-only check pass while shifting the block.
    let after_block = r.document().blocks.iter().find(|b| {
        if let acdc_parser::Block::Paragraph(p) = b {
            p.content.iter().any(|i| matches!(i,
                acdc_parser::InlineNode::PlainText(t) if t.content.contains("After.")
            ))
        } else { false }
    }).expect("After. paragraph must exist in root doc");
    if let acdc_parser::Block::Paragraph(p) = after_block {
        assert_eq!(
            p.location.start.line, 5,
            "`After.` MUST report root-source line 5, not the post-preprocess shifted line"
        );
    }
}

#[test]
fn conditional_drops_mixed_with_includes_sorted_by_source_line() {
    // Both kinds in one document — `include_expansions` (sourceLine=3)
    // and a false `ifdef` (lines 5/6/7). The wasm bridge merges and
    // sorts them, but acdc itself records them in distinct fields.
    // Pin both shapes so a future refactor that merges fields can't
    // silently drop one.
    let resolver = DynFileResolver::new(InMemoryFiles::new(&[(
        "ch.adoc",
        "ch-line\n",
    )]));
    let opts = Options::builder()
        .with_file_resolver(resolver)
        .with_virtual_current_file("main.adoc")
        .build();
    let src = "= M\n\ninclude::ch.adoc[]\n\nifdef::foo[]\nhidden\nendif::[]\n\nAfter.\n";
    let r = acdc_parser::parse(src, &opts).expect("parse");
    assert_eq!(r.include_expansions().len(), 1);
    assert_eq!(r.include_expansions()[0].source_line, 3);
    assert_eq!(r.include_expansions()[0].expanded_lines, 1);
    assert_eq!(r.conditional_drops(), &[5usize, 6, 7][..]);
}
