//! Inline locations name the source bytes each node is drawn from (inclusive ends): a
//! passthrough's text is its content without the closing `+`, and a formatted span with an
//! attribute list (`[.role]*x*`, `[#id]_x_`) starts at the list, as asciidoctor reads it as
//! part of the span. An editor maps a caret through these: one byte off writes in the wrong place.

use acdc_parser::{Block, InlineNode, Location, Options, SafeMode};

type Span = (&'static str, usize, usize);

fn span(l: &Location) -> (usize, usize) {
    (l.absolute_start, l.absolute_end)
}

fn spans(nodes: &[InlineNode], out: &mut Vec<(String, usize, usize)>) {
    for n in nodes {
        let (kind, loc, kids) = match n {
            InlineNode::PlainText(t) => {
                let (s, e) = span(&t.location);
                out.push((format!("plain {}", t.content), s, e));
                continue;
            }
            InlineNode::RawText(r) => {
                let (s, e) = span(&r.location);
                out.push((format!("raw {}", r.content), s, e));
                continue;
            }
            InlineNode::BoldText(b) => ("bold", &b.location, &b.content),
            InlineNode::ItalicText(i) => ("italic", &i.location, &i.content),
            InlineNode::MonospaceText(m) => ("mono", &m.location, &m.content),
            InlineNode::HighlightText(h) => ("mark", &h.location, &h.content),
            InlineNode::Macro(acdc_parser::InlineMacro::Link(l)) => ("link", &l.location, &l.text),
            _ => {
                out.push((format!("{n:?}").chars().take(20).collect(), 0, 0));
                continue;
            }
        };
        let (s, e) = span(loc);
        out.push((kind.into(), s, e));
        spans(kids, out);
    }
}

/// What a Markdown-style quote's content records beside the block tree — its footnotes, the
/// warnings it raises, its sections in the table of contents — stands where its bytes do.
#[test]
fn a_quotes_side_records_stand_where_their_bytes_do() {
    let options = Options::builder().with_safe_mode(SafeMode::Unsafe).build();
    let mut failures = Vec::new();

    let doc = "Intro text here.\n\n> A claim.footnote:[Source]";
    let parsed = acdc_parser::parse(doc, &options).expect("parse");
    let want = doc.find("footnote:").unwrap_or(usize::MAX);
    let got: Vec<usize> = parsed
        .document()
        .footnotes
        .iter()
        .map(|f| f.location.absolute_start)
        .collect();
    if got != [want] {
        failures.push(format!("{doc:?} footnotes at {got:?}, want [{want}]"));
    }

    let doc = "Intro.\n\n> a {counter:n} b";
    let parsed = acdc_parser::parse(doc, &options).expect("parse");
    let at = doc.find('{').unwrap_or(usize::MAX);
    let got: Vec<(usize, usize, u32, u32)> = parsed
        .warnings()
        .iter()
        .filter_map(|w| w.source_location())
        .map(|l| {
            (
                l.location.absolute_start,
                l.location.absolute_end,
                l.location.start.line,
                l.location.start.column,
            )
        })
        .collect();
    if got != [(at, at + "{counter:n}".len(), 3, 5)] {
        failures.push(format!(
            "{doc:?} warnings at {got:?}, want [({at}, {}, 3, 5)]",
            at + "{counter:n}".len()
        ));
    }

    let doc = "Intro.\n\n> == Inner\n";
    let parsed = acdc_parser::parse(doc, &options).expect("parse");
    let at = doc.find("==").unwrap_or(usize::MAX);
    let got: Vec<usize> = parsed
        .document()
        .toc_entries
        .iter()
        .map(|t| t.location.absolute_start)
        .collect();
    if got != [at] {
        failures.push(format!("{doc:?} toc entries at {got:?}, want [{at}]"));
    }

    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Line and column (1-based, in characters) of byte `at` in `src`, counted from the source.
fn position_of(src: &str, at: usize) -> (u32, u32) {
    let before = &src[..at];
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
    (
        u32::try_from(line).unwrap_or(u32::MAX),
        u32::try_from(col).unwrap_or(u32::MAX),
    )
}

fn line_cols(nodes: &[InlineNode], src: &str, out: &mut Vec<String>) {
    for n in nodes {
        let (name, loc, kids): (&str, &Location, &[InlineNode]) = match n {
            InlineNode::PlainText(t) => (t.content, &t.location, &[]),
            InlineNode::RawText(r) => (r.content, &r.location, &[]),
            InlineNode::BoldText(b) => ("bold", &b.location, &b.content),
            InlineNode::ItalicText(i) => ("italic", &i.location, &i.content),
            InlineNode::MonospaceText(m) => ("mono", &m.location, &m.content),
            _ => continue,
        };
        let got = (
            (loc.start.line, loc.start.column),
            (loc.end.line, loc.end.column),
        );
        let want = (
            position_of(src, loc.absolute_start),
            position_of(src, loc.absolute_end),
        );
        if got != want {
            out.push(format!(
                "{name:?} at {}..{}: got {got:?}, want {want:?}",
                loc.absolute_start, loc.absolute_end
            ));
        }
        line_cols(kids, src, out);
    }
}

/// A node's line and column are where its bytes stand: a passthrough or the text before one
/// may run over a line end, and a character before it may be wider than a byte.
#[test]
fn inline_line_and_column_are_where_their_bytes_stand() {
    let options = Options::builder().with_safe_mode(SafeMode::Unsafe).build();
    let docs = [
        "a +code+ b",
        "x ++a\nbc++ y",
        "x +++a\nb+++ z",
        "one\ntwo +x+ end",
        "x pass:[a\nb] y",
        "中+a+文+b+",
        "测试 ++ab++ 后",
        "**++a\nb++**",
        "x pass:q[*a*\nb] y",
        ":a: x\n\n{a}",
        "intro\n\n*b* a +x+ c",
    ];
    let mut failures = Vec::new();
    for doc in docs {
        let parsed = acdc_parser::parse(doc, &options).expect("parse");
        let mut bad = Vec::new();
        for block in &parsed.document().blocks {
            let Block::Paragraph(p) = block else {
                panic!("{doc}: not a paragraph: {block:?}")
            };
            line_cols(&p.content, doc, &mut bad);
        }
        if !bad.is_empty() {
            failures.push(format!("{doc:?}\n  {}", bad.join("\n  ")));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn inline_locations_name_their_source_bytes() {
    let options = Options::builder().with_safe_mode(SafeMode::Unsafe).build();
    #[rustfmt::skip]
    let rows: &[(&str, &[Span])] = &[
        ("a *bold* b", &[("plain a ", 0, 1), ("bold", 2, 7), ("plain bold", 3, 6), ("plain  b", 8, 9)]),
        ("a +code+ b", &[("plain a ", 0, 1), ("raw code", 3, 6), ("plain  b", 8, 9)]),
        ("a `+code+` b", &[("plain a ", 0, 1), ("mono", 2, 9), ("raw code", 4, 7), ("plain  b", 10, 11)]),
        ("a [.role]*bold* b", &[("plain a ", 0, 1), ("bold", 2, 14), ("plain bold", 10, 13), ("plain  b", 15, 16)]),
        ("a [#id]_em_ b", &[("plain a ", 0, 1), ("italic", 2, 10), ("plain em", 8, 9), ("plain  b", 11, 12)]),
        ("a [.r]`code` b", &[("plain a ", 0, 1), ("mono", 2, 11), ("plain code", 7, 10), ("plain  b", 12, 13)]),
        ("a [.line-through]#gone# b", &[("plain a ", 0, 1), ("mark", 2, 22), ("plain gone", 18, 21), ("plain  b", 23, 24)]),
        ("q", &[("plain q", 0, 0)]),
        ("x `a` y", &[("plain x ", 0, 1), ("mono", 2, 4), ("plain a", 3, 3), ("plain  y", 5, 6)]),
        ("x *a* y", &[("plain x ", 0, 1), ("bold", 2, 4), ("plain a", 3, 3), ("plain  y", 5, 6)]),
        ("x **a** y", &[("plain x ", 0, 1), ("bold", 2, 6), ("plain a", 4, 4), ("plain  y", 7, 8)]),
        ("**bb**q**cc**", &[("bold", 0, 5), ("plain bb", 2, 3), ("plain q", 6, 6), ("bold", 7, 12), ("plain cc", 9, 10)]),
        // A reference removed to nothing (a counter, an attribute set with no value or an
        // empty one): the node right after it starts at its own first byte.
        ("n {counter:n}*then*", &[("plain n ", 0, 1), ("bold", 13, 18), ("plain then", 14, 17)]),
        ("{counter:a}{counter:b}x", &[("plain x", 22, 22)]),
        (":e:\n\n{e}*b*", &[("bold", 8, 10), ("plain b", 9, 9)]),
        // The text around a passthrough: in any paragraph, ending where it ends, after a
        // reference that was expanded or removed.
        ("intro\n\n*b* a +x+ c", &[("plain intro", 0, 4), ("bold", 7, 9), ("plain b", 8, 8), ("plain  a ", 10, 12), ("raw x", 14, 14), ("plain  c", 16, 17)]),
        ("a +x+ b *c*", &[("plain a ", 0, 1), ("raw x", 3, 3), ("plain  b ", 5, 7), ("bold", 8, 10), ("plain c", 9, 9)]),
        (":a: hello world\n\n{a} +x+ tail", &[("plain hello world ", 17, 20), ("raw x", 22, 22), ("plain  tail", 24, 28)]),
        ("{counter:n} +x+ tail", &[("plain  ", 11, 11), ("raw x", 13, 13), ("plain  tail", 15, 19)]),
        // A passthrough's content is where its source stands: after `pass:` and its
        // substitutions, whatever they are; content its substitutions changed is the
        // whole span it was read from.
        ("pass:q[hello]", &[("raw hello", 7, 11)]),
        ("pass:quotes[*b*]", &[("bold", 12, 14), ("plain b", 13, 13)]),
        ("pass:q,a[*b*]", &[("bold", 9, 11), ("plain b", 10, 10)]),
        (":x: hi\n\npass:a[{x}]", &[("raw hi", 15, 17)]),
        (":x: a value longer than ten\n\npass:a[{x}]", &[("raw a value longer than ten", 36, 38)]),
        // An escaped marker is its backslash and the marker, and ends there.
        ("a \\*b\\* c", &[("plain a ", 0, 1), ("plain *", 2, 3), ("plain b", 4, 4), ("plain *", 5, 6), ("plain  c", 7, 8)]),
        ("a \\^x^ b", &[("plain a ", 0, 1), ("plain ^x^", 2, 5), ("plain  b", 6, 7)]),
        // A link's text of white space only ends at its own last character, not the `]`.
        ("link:u[ ] z", &[("link", 0, 8), ("plain  ", 7, 7), ("plain  z", 9, 10)]),
        ("link:u[  ]", &[("link", 0, 9), ("plain   ", 7, 8)]),
    ];
    let mut failures = Vec::new();
    // A Markdown-style quote's lines: every location counts the `> ` prefix.
    #[rustfmt::skip]
    let quoted: &[(&str, &[Span])] = &[
        ("> a *bc* d", &[("plain a ", 2, 3), ("bold", 4, 7), ("plain bc", 5, 6), ("plain  d", 8, 9)]),
        ("> _ab_ *[.r]#cd#* z", &[("italic", 2, 5), ("plain ab", 3, 4), ("plain  ", 6, 6), ("bold", 7, 16), ("mark", 8, 15), ("plain cd", 13, 14), ("plain  z", 17, 18)]),
        ("> x\n> a *bc* d", &[("plain x\na ", 2, 7), ("bold", 8, 11), ("plain bc", 9, 10), ("plain  d", 12, 13)]),
        ("> _测试_ *b* z", &[("italic", 2, 9), ("plain 测试", 3, 6), ("plain  ", 10, 10), ("bold", 11, 13), ("plain b", 12, 12), ("plain  z", 14, 15)]),
        // Lines of block metadata above the quote come before its first `>`.
        (".Title\n> hello", &[("plain hello", 9, 13)]),
        ("[.lead]\n> *b*", &[("bold", 10, 12), ("plain b", 11, 11)]),
        ("[[q]]\n.T\n> a\n> *b*", &[("plain a\n", 11, 12), ("bold", 15, 17), ("plain b", 16, 16)]),
    ];
    for (doc, want) in quoted {
        let parsed = acdc_parser::parse(doc, &options).expect("parse");
        let Some(Block::DelimitedBlock(q)) = parsed.document().blocks.first() else {
            panic!("{doc}: not a quote: {:?}", parsed.document().blocks.first())
        };
        let acdc_parser::DelimitedBlockType::DelimitedQuote(blocks) = &q.inner else {
            panic!("{doc}: not a quote block")
        };
        let Some(Block::Paragraph(p)) = blocks.first() else {
            panic!("{doc}: no paragraph")
        };
        let mut got = Vec::new();
        spans(&p.content, &mut got);
        let want: Vec<(String, usize, usize)> = want
            .iter()
            .map(|(k, s, e)| ((*k).to_string(), *s, *e))
            .collect();
        if got != want {
            failures.push(format!("{doc:?}\n  got  {got:?}\n  want {want:?}"));
        }
    }
    for (doc, want) in rows {
        let parsed = acdc_parser::parse(doc, &options).expect("parse");
        let mut got = Vec::new();
        for block in &parsed.document().blocks {
            let Block::Paragraph(p) = block else {
                panic!("{doc}: not a paragraph: {block:?}")
            };
            spans(&p.content, &mut got);
        }
        let want: Vec<(String, usize, usize)> = want
            .iter()
            .map(|(k, s, e)| ((*k).to_string(), *s, *e))
            .collect();
        if got != want {
            failures.push(format!("{doc:?}\n  got  {got:?}\n  want {want:?}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
