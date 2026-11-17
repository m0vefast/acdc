//! Inline locations name the source bytes each node is drawn from (inclusive
//! ends). An editor maps a caret through these: one byte off writes in the
//! wrong place.

use acdc_parser::{
    Options, SafeMode,
    ast::{BlockKind, BlockRef, ContentRef, InlineKind, LocationRef},
    parse,
};

type Error = Box<dyn std::error::Error>;
type Span = (&'static str, usize, usize);

/// Every plain text node in the document with the source bytes its range names.
fn plain_texts(doc: &str) -> Result<Vec<(String, String)>, Error> {
    let parsed = parse(doc, &options()?)?;
    let mut out = Vec::new();
    for field in parsed.document().content_occurrences() {
        for node in field.content().inlines() {
            if let InlineKind::Plain(text) = node.kind() {
                let (s, e) = span(node.location());
                let last = doc[e..].chars().next().map_or(0, char::len_utf8);
                out.push((text.text().to_string(), doc[s..e + last].to_string()));
            }
        }
    }
    Ok(out)
}

/// A link macro records where its target is written, as a cross reference does.
#[test]
fn link_macros_locate_their_written_target() -> Result<(), Error> {
    let doc = ":base: https://example.org\n\nSee link:guide.adoc[], https://example.org/x[] and mailto:a@b.org[].\nThen link:{base}/y[Y] and https://example.org/z[*Z*].\n";
    let parsed = parse(doc, &options()?)?;
    let mut targets = Vec::new();
    for field in parsed.document().content_occurrences() {
        for node in field.content().inlines() {
            let location = match node.as_hyperlink() {
                Some(acdc_parser::ast::HyperlinkRef::Link(link)) => link.target_location(),
                Some(acdc_parser::ast::HyperlinkRef::UrlMacro(link)) => link.target_location(),
                Some(acdc_parser::ast::HyperlinkRef::Mailto(link)) => link.target_location(),
                _ => continue,
            };
            let (s, e) = span(location);
            targets.push(doc[s..=e].to_string());
        }
    }
    assert_eq!(
        targets,
        ["guide.adoc", "https://example.org/x", "mailto:a@b.org", "{base}/y", "https://example.org/z"]
    );
    Ok(())
}

/// A link the preprocessor draws for an include it does not read stands for the
/// whole directive, its target included.
#[test]
fn a_drawn_include_link_stands_for_its_directive() -> Result<(), Error> {
    let doc = "include::https://example.invalid/remote chapter.adoc[]\n";
    let parsed = parse(doc, &options()?)?;
    let mut spans = Vec::new();
    for field in parsed.document().content_occurrences() {
        for node in field.content().inlines() {
            if let Some(acdc_parser::ast::HyperlinkRef::Link(link)) = node.as_hyperlink() {
                spans.push((span_text(doc, node.location()), span_text(doc, link.target_location())));
            }
        }
    }
    let directive = doc.trim_end().to_string();
    assert_eq!(spans, [(directive.clone(), directive)]);
    Ok(())
}

/// Content parsed on its own starts where its first byte is, not at the
/// delimiter in front of it.
#[test]
fn sub_parsed_content_starts_at_its_first_byte() -> Result<(), Error> {
    for doc in ["\"Quoted words.\"\n-- Author\n", "\"Two\nlines.\"\n-- Author, Work\n"] {
        for (text, bytes) in plain_texts(doc)? {
            assert_eq!(text, bytes, "{doc:?}");
        }
    }
    Ok(())
}

/// In every content field, a text node is its source bytes or stands for one
/// whole attribute reference.
#[test]
fn attribute_values_split_at_their_references() -> Result<(), Error> {
    for doc in [
        ":w: Before\n\n[title=\"{w} / x\"]\nPara.\n",
        ":w: Before\n\n.{w} / x\nPara.\n",
        ":w: Before\n\n[quote,'{w} footnote:[Note.]']\n____\nQ.\n____\n",
        ":w: Before\n\n[id=s,reftext='{w} x']\nPara.\n\nSee <<s>>.\n",
        ":w: Before\n\n[[a,{w} Section]]\n== Title\n",
    ] {
        for (text, bytes) in plain_texts(doc)? {
            let reference = bytes
                .strip_prefix('{')
                .and_then(|name| name.strip_suffix('}'))
                .is_some_and(|name| !name.contains(['{', '}']));
            assert!(text == bytes || reference, "{doc:?}: {text:?} at {bytes:?}");
        }
    }
    Ok(())
}

/// Verbatim text is its source bytes or stands for one whole attribute
/// reference, and siblings never overlap.
#[test]
fn verbatim_text_splits_at_references() -> Result<(), Error> {
    let doc = ":e:\n\n[source,text,subs=\"attributes+,+quotes\"]\n----\nS `{e}` *café{sp}*.\n----\n";
    let parsed = parse(doc, &options()?)?;
    let mut spans = Vec::new();
    for field in parsed.document().content_occurrences() {
        for node in field.content().inlines() {
            if let InlineKind::Verbatim(text) = node.kind() {
                let (s, e) = span(node.location());
                let last = doc[e..].chars().next().map_or(0, char::len_utf8);
                spans.push((text.text().to_string(), doc[s..e + last].to_string(), s, e));
            }
        }
    }
    assert!(spans.len() > 1, "{spans:?}");
    for pair in spans.windows(2) {
        assert!(pair[1].2 > pair[0].3, "{spans:?}");
    }
    for (text, bytes, ..) in &spans {
        let reference = bytes.starts_with('{') && bytes.ends_with('}');
        assert!(text == bytes || reference, "{text:?} at {bytes:?}: {spans:?}");
    }
    Ok(())
}

/// Text frozen before attributes run (a footnote body captured by macros
/// ahead of attributes) keeps the reference literally and is not cut where
/// the reference would have expanded.
#[test]
fn frozen_text_is_not_cut_at_references() -> Result<(), Error> {
    let doc = ":name: Alpha\n\n[source,subs=\"specialcharacters,macros,attributes\"]\n----\nFirst footnote:late[Text {name}.]\n----\n";
    let parsed = parse(doc, &options()?)?;
    let mut bodies = Vec::new();
    for field in parsed.document().content_occurrences() {
        for node in field.content().inlines() {
            if let InlineKind::Footnote(note) = node.kind()
                && let Some(content) = note.content()
            {
                bodies.push(content.inlines().map(|inline| format!("{:?}", inline.kind()).chars().take(4).collect::<String>() + &span_text(doc, inline.location())).collect::<Vec<_>>());
            }
        }
    }
    assert_eq!(bodies, [["Raw(Text {name}."]]);
    Ok(())
}

fn span_text(doc: &str, location: LocationRef<'_>) -> String {
    let (s, e) = span(location);
    let last = doc[e..].chars().next().map_or(0, char::len_utf8);
    doc[s..e + last].to_string()
}

/// A reference without text inside the title it names does not loop.
#[test]
fn a_reference_in_its_own_title_terminates() -> Result<(), Error> {
    let _ = parse("[[s]]\n== A xref:s[] B\n", &options()?)?;
    let _ = parse("= <<s,t>>\n\n[[s]]\n== _a_ xref:s[] b\n\n== c xref:s[]\n", &options()?)?;
    // Asciidoctor: inside the expansion of its own target a reference reads
    // `[s]`, so the second title converts to `c a [s]`.
    let parsed = parse("[[s]]\n== a xref:s[]\n\n== c xref:s[]\n", &options()?)?;
    let ids: Vec<_> = parsed
        .document()
        .blocks()
        .filter_map(|block| match block.kind() {
            BlockKind::Section(section) => section.id().map(|id| id.as_str().to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(ids, ["s", "_c_a_s"]);
    Ok(())
}

/// A block with no text records no text node.
#[test]
fn an_empty_block_holds_no_text() -> Result<(), Error> {
    for block in ["////\n////", "[comment]\n--\n--", "++++\n++++", "----\n----", "....\n...."] {
        let doc = format!("A\n\n{block}\n\nB\n");
        let parsed = parse(&doc, &options()?)?;
        let mut nodes = Vec::new();
        for field in parsed.document().content_occurrences() {
            nodes.extend(field.content().inlines().map(|node| span(node.location())));
        }
        assert_eq!(nodes, [(0, 0), (doc.len() - 2, doc.len() - 2)], "{doc:?}");
    }
    Ok(())
}

/// Text the parser draws for a construct (an include not read in secure mode,
/// drawn as a link) is located at that construct's own source, not by the drawn
/// text's length.
#[test]
fn drawn_text_is_located_at_its_construct() -> Result<(), Error> {
    for directive in ["include::some-file.adoc[]", "include::f.adoc[lines=1..2,tag=x,indent=4]"] {
        drawn_include_is_located_at_its_directive(directive)?;
    }
    Ok(())
}

fn drawn_include_is_located_at_its_directive(directive: &str) -> Result<(), Error> {
    let doc = format!("Intro.\n\n{directive}\n\nAfter.\n");
    let parsed = parse(&doc, &Options::builder().with_safe_mode(SafeMode::Secure).build()?)?;
    let mut links = 0;
    for field in parsed.document().content_occurrences() {
        for node in field.content().inlines() {
            if let InlineKind::Link(_) = node.kind() {
                links += 1;
                let loc = node.location();
                assert_eq!((loc.start().line(), loc.start().column()), (3, 1));
                assert_eq!((loc.end().line(), loc.end().column() as usize), (3, directive.chars().count()));
            }
        }
    }
    assert_eq!(links, 1, "the unread include draws one link");

    // The line after the construct keeps its own place.
    let doc = format!("Intro.\n\n{directive}\nNext line.\n");
    let parsed = parse(&doc, &Options::builder().with_safe_mode(SafeMode::Secure).build()?)?;
    for field in parsed.document().content_occurrences() {
        for node in field.content().inlines() {
            if let InlineKind::Plain(text) = node.kind() {
                let (s, e) = span(node.location());
                let last = doc[e..].chars().next().map_or(0, char::len_utf8);
                assert_eq!(&doc[s..e + last], text.text(), "at {s}..={e}");
            }
        }
    }
    Ok(())
}

fn options() -> Result<Options<'static>, Error> {
    Ok(Options::builder().with_safe_mode(SafeMode::Unsafe).build()?)
}

fn span(location: LocationRef<'_>) -> (usize, usize) {
    (location.absolute_start(), location.absolute_end())
}

fn spans(content: ContentRef<'_>, out: &mut Vec<(String, usize, usize)>) {
    for node in content.inlines() {
        let (kind, attribute, kids) = match node.kind() {
            InlineKind::Plain(text) => {
                let (s, e) = span(node.location());
                out.push((format!("plain {}", text.text()), s, e));
                continue;
            }
            InlineKind::Raw(raw) => {
                let (s, e) = span(node.location());
                out.push((format!("raw {}", raw.text()), s, e));
                continue;
            }
            InlineKind::Bold(b) => ("bold", b.attribute_location(), b.content()),
            InlineKind::Italic(i) => ("italic", i.attribute_location(), i.content()),
            InlineKind::Monospace(m) => ("mono", m.attribute_location(), m.content()),
            InlineKind::Highlight(h) => ("mark", h.attribute_location(), h.content()),
            InlineKind::Link(l) => ("link", None, l.text()),
            InlineKind::CrossReference(x) => ("xref", None, x.text()),
            other => {
                let name = format!("{other:?}");
                let (s, e) = span(node.location());
                out.push((name.split(['(', ' ', '{']).next().unwrap_or_default().to_lowercase(), s, e));
                continue;
            }
        };
        if let Some(attribute) = attribute {
            let (s, e) = span(attribute);
            out.push(("attr".into(), s, e));
        }
        let (s, e) = span(node.location());
        out.push((kind.into(), s, e));
        spans(kids, out);
    }
}

fn owned(want: &[Span]) -> Vec<(String, usize, usize)> {
    want.iter()
        .map(|(k, s, e)| ((*k).to_string(), *s, *e))
        .collect()
}

fn first_location(content: Option<ContentRef<'_>>) -> Option<usize> {
    content?
        .inlines()
        .next()
        .map(|node| node.location().absolute_start())
}

/// What a Markdown-style quote's content records beside the block tree — its
/// footnotes, the warnings it raises, its sections in the table of contents —
/// stands where its bytes do.
#[test]
fn a_quotes_side_records_stand_where_their_bytes_do() -> Result<(), Error> {
    let options = options()?;
    let mut failures = Vec::new();

    let doc = "Intro text here.\n\n> A claim.footnote:[Source]";
    let parsed = parse(doc, &options)?;
    let want = doc.find("Source");
    let got: Vec<Option<usize>> = parsed
        .document()
        .footnotes()
        .map(|f| first_location(Some(f.content())))
        .collect();
    if got != [want] {
        failures.push(format!("{doc:?} footnotes at {got:?}, want [{want:?}]"));
    }

    let doc = "Intro.\n\n> a {counter:n} b";
    let parsed = parse(doc, &options)?;
    let at = doc.find('{').ok_or("no counter")?;
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
    let end = at + "{counter:n}".len() - 1;
    if got != [(at, end, 3, 5)] {
        failures.push(format!("{doc:?} warnings at {got:?}, want [({at}, {end}, 3, 5)]"));
    }

    let doc = "Intro.\n\n> == Inner\n";
    let parsed = parse(doc, &options)?;
    let want = doc.find("Inner");
    let got: Vec<Option<usize>> = parsed
        .document()
        .toc_entries()
        .map(|t| first_location(t.title()))
        .collect();
    if got != [want] {
        failures.push(format!("{doc:?} toc entries at {got:?}, want [{want:?}]"));
    }

    assert!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}

/// A Markdown-style quote's lines: every location counts the `> ` prefix of
/// its line, after any block metadata lines above the quote.
#[test]
fn quoted_inline_locations_count_the_quote_prefix() -> Result<(), Error> {
    let options = options()?;
    #[rustfmt::skip]
    let quoted: &[(&str, &[Span])] = &[
        ("> a *bc* d", &[("plain a ", 2, 3), ("bold", 4, 7), ("plain bc", 5, 6), ("plain  d", 8, 9)]),
        ("> _ab_ *[.r]#cd#* z", &[("italic", 2, 5), ("plain ab", 3, 4), ("plain  ", 6, 6), ("bold", 7, 16), ("attr", 8, 11), ("mark", 12, 15), ("plain cd", 13, 14), ("plain  z", 17, 18)]),
        ("> x\n> a *bc* d", &[("plain x\n", 2, 3), ("plain a ", 6, 7), ("bold", 8, 11), ("plain bc", 9, 10), ("plain  d", 12, 13)]),
        ("> _测试_ *b* z", &[("italic", 2, 9), ("plain 测试", 3, 6), ("plain  ", 10, 10), ("bold", 11, 13), ("plain b", 12, 12), ("plain  z", 14, 15)]),
        (".Title\n> hello", &[("plain hello", 9, 13)]),
        ("[.lead]\n> *b*", &[("bold", 10, 12), ("plain b", 11, 11)]),
        ("[[q]]\n.T\n> a\n> *b*", &[("plain a\n", 11, 12), ("bold", 15, 17), ("plain b", 16, 16)]),
    ];
    let mut failures = Vec::new();
    for (doc, want) in quoted {
        let parsed = parse(doc, &options)?;
        let Some(BlockKind::Quote(quote)) = parsed.document().blocks().next().map(BlockRef::kind)
        else {
            return Err(format!("{doc:?}: not a quote").into());
        };
        let Some(BlockKind::Paragraph(paragraph)) = quote.blocks().next().map(BlockRef::kind)
        else {
            return Err(format!("{doc:?}: no paragraph").into());
        };
        let mut got = Vec::new();
        spans(paragraph.content(), &mut got);
        let mut bad = Vec::new();
        line_cols(paragraph.content(), doc, &mut bad);
        if got != owned(want) || !bad.is_empty() {
            failures.push(format!(
                "{doc:?}\n  got  {got:?}\n  want {want:?}\n  {}",
                bad.join("\n  ")
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}

/// Spaces between a title marker and its title are not part of the title.
#[test]
fn title_text_starts_after_the_marker_spaces() -> Result<(), Error> {
    let options = options()?;
    let mut failures = Vec::new();
    for (doc, want) in [("=  Title\n\nbody\n", 3), ("= Title\n\nbody\n", 2)] {
        let parsed = parse(doc, &options)?;
        let got = first_location(parsed.document().title());
        if got != Some(want) {
            let mut nodes = Vec::new();
            if let Some(title) = parsed.document().title() {
                spans(title, &mut nodes);
            }
            failures.push(format!("{doc:?} title at {got:?}, want {want}: {nodes:?}"));
        }
    }
    for (doc, want) in [("==  x\n", 4), ("== x\n", 3)] {
        let parsed = parse(doc, &options)?;
        let Some(BlockKind::Section(section)) = parsed.document().blocks().next().map(BlockRef::kind)
        else {
            failures.push(format!("{doc:?}: not a section"));
            continue;
        };
        let got = first_location(section.title());
        if got != Some(want) {
            failures.push(format!("{doc:?} title at {got:?}, want {want}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}

/// Line and column (1-based, in characters) of byte `at` in `src`.
fn position_of(src: &str, at: usize) -> (u32, u32) {
    let before = src.get(..at).unwrap_or(src);
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
    (
        u32::try_from(line).unwrap_or(u32::MAX),
        u32::try_from(col).unwrap_or(u32::MAX),
    )
}

fn line_cols(content: ContentRef<'_>, src: &str, out: &mut Vec<String>) {
    for node in content.inlines() {
        let kids = match node.kind() {
            InlineKind::Bold(b) => Some(b.content()),
            InlineKind::Italic(i) => Some(i.content()),
            InlineKind::Monospace(m) => Some(m.content()),
            InlineKind::Highlight(h) => Some(h.content()),
            InlineKind::Plain(_) | InlineKind::Raw(_) => None,
            _ => continue,
        };
        let loc = node.location();
        let got = (
            (loc.start().line(), loc.start().column()),
            (loc.end().line(), loc.end().column()),
        );
        let want = (
            position_of(src, loc.absolute_start()),
            position_of(src, loc.absolute_end()),
        );
        if got != want {
            out.push(format!(
                "{:?} at {}..{}: got {got:?}, want {want:?}",
                node.kind(),
                loc.absolute_start(),
                loc.absolute_end()
            ));
        }
        if let Some(kids) = kids {
            line_cols(kids, src, out);
        }
    }
}

/// A node's line and column are where its bytes stand: a passthrough or the
/// text before one may run over a line end, and a character before it may be
/// wider than a byte.
#[test]
fn inline_line_and_column_are_where_their_bytes_stand() -> Result<(), Error> {
    let options = options()?;
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
        let parsed = parse(doc, &options)?;
        let mut bad = Vec::new();
        for block in parsed.document().blocks() {
            let BlockKind::Paragraph(p) = block.kind() else {
                return Err(format!("{doc:?}: not a paragraph").into());
            };
            line_cols(p.content(), doc, &mut bad);
        }
        if !bad.is_empty() {
            failures.push(format!("{doc:?}\n  {}", bad.join("\n  ")));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}

#[test]
fn inline_locations_name_their_source_bytes() -> Result<(), Error> {
    let options = options()?;
    #[rustfmt::skip]
    let rows: &[(&str, &[Span])] = &[
        ("a *bold* b", &[("plain a ", 0, 1), ("bold", 2, 7), ("plain bold", 3, 6), ("plain  b", 8, 9)]),
        ("a +code+ b", &[("plain a ", 0, 1), ("raw code", 3, 6), ("plain  b", 8, 9)]),
        ("a `+code+` b", &[("plain a ", 0, 1), ("mono", 2, 9), ("raw code", 4, 7), ("plain  b", 10, 11)]),
        ("a [.role]*bold* b", &[("plain a ", 0, 1), ("attr", 2, 8), ("bold", 9, 14), ("plain bold", 10, 13), ("plain  b", 15, 16)]),
        ("a [#id]_em_ b", &[("plain a ", 0, 1), ("attr", 2, 6), ("italic", 7, 10), ("plain em", 8, 9), ("plain  b", 11, 12)]),
        ("a [.r]`code` b", &[("plain a ", 0, 1), ("attr", 2, 5), ("mono", 6, 11), ("plain code", 7, 10), ("plain  b", 12, 13)]),
        ("a [.line-through]#gone# b", &[("plain a ", 0, 1), ("attr", 2, 16), ("mark", 17, 22), ("plain gone", 18, 21), ("plain  b", 23, 24)]),
        ("q", &[("plain q", 0, 0)]),
        ("x `a` y", &[("plain x ", 0, 1), ("mono", 2, 4), ("plain a", 3, 3), ("plain  y", 5, 6)]),
        ("x *a* y", &[("plain x ", 0, 1), ("bold", 2, 4), ("plain a", 3, 3), ("plain  y", 5, 6)]),
        ("x **a** y", &[("plain x ", 0, 1), ("bold", 2, 6), ("plain a", 4, 4), ("plain  y", 7, 8)]),
        ("**bb**q**cc**", &[("bold", 0, 5), ("plain bb", 2, 3), ("plain q", 6, 6), ("bold", 7, 12), ("plain cc", 9, 10)]),
        // A reference removed to nothing: the node right after it starts at its
        // own first byte.
        ("n {counter:n}*then*", &[("plain n ", 0, 1), ("bold", 13, 18), ("plain then", 14, 17)]),
        ("{counter:a}{counter:b}x", &[("plain x", 22, 22)]),
        (":e:\n\n{e}*b*", &[("bold", 8, 10), ("plain b", 9, 9)]),
        // The text around a passthrough, after a reference that was expanded or
        // removed.
        ("intro\n\n*b* a +x+ c", &[("plain intro", 0, 4), ("bold", 7, 9), ("plain b", 8, 8), ("plain  a ", 10, 12), ("raw x", 14, 14), ("plain  c", 16, 17)]),
        ("a +x+ b *c*", &[("plain a ", 0, 1), ("raw x", 3, 3), ("plain  b ", 5, 7), ("bold", 8, 10), ("plain c", 9, 9)]),
        (":a: hello world\n\n{a} +x+ tail", &[("plain hello world", 17, 19), ("plain  ", 20, 20), ("raw x", 22, 22), ("plain  tail", 24, 28)]),
        // Text a reference expands to is a node of its own, standing at the whole
        // reference; the text around it keeps its own bytes.
        (":n: Kevin\n\nHello {n}, welcome.", &[("plain Hello ", 11, 16), ("plain Kevin", 17, 19), ("plain , welcome.", 20, 29)]),
        (":e:\n\na {e} b", &[("plain a ", 5, 6), ("plain  b", 10, 11)]),
        (":l: pass:c[R & <D>]\n\nO: {l}.", &[("plain O: ", 21, 23), ("plain R ", 24, 26), ("raw &", 24, 26), ("plain  ", 24, 26), ("raw <", 24, 26), ("plain D", 24, 26), ("raw >", 24, 26), ("plain .", 27, 27)]),
        ("{counter:n} +x+ tail", &[("plain  ", 11, 11), ("raw x", 13, 13), ("plain  tail", 15, 19)]),
        // A passthrough's content is where its source stands, after `pass:` and
        // its substitutions; content its substitutions changed is the whole span
        // it was read from.
        ("pass:q[hello]", &[("raw hello", 7, 11)]),
        ("pass:quotes[*b*]", &[("bold", 12, 14), ("raw b", 13, 13)]),
        ("pass:q,a[*b*]", &[("bold", 9, 11), ("raw b", 10, 10)]),
        (":x: hi\n\npass:a[{x}]", &[("raw hi", 15, 17)]),
        (":x: a value longer than ten\n\npass:a[{x}]", &[("raw a value longer than ten", 36, 38)]),
        // An escaped marker is its backslash and the marker, and ends there.
        ("a \\*b\\* c", &[("plain a ", 0, 1), ("plain *", 2, 3), ("plain b\\* c", 4, 8)]),
        ("a \\^x^ b", &[("plain a ", 0, 1), ("plain ^x^", 2, 5), ("plain  b", 6, 7)]),
        // A link's text of white space only ends at its own last character.
        ("link:u[ ] z", &[("link", 0, 8), ("plain  ", 7, 7), ("plain  z", 9, 10)]),
        ("link:u[  ]", &[("link", 0, 9), ("plain   ", 7, 8)]),
        // An escaped macro is its backslash and its text; what follows keeps its
        // own bytes.
        // Removed indentation is not part of a node, and splits the text.
        (" a\n b", &[("plain a\n", 1, 2), ("plain b", 4, 4)]),
        (" <<<", &[("plain <<<", 1, 3)]),
        ("[paragraph]\n  x *y*", &[("plain x *y*", 14, 18)]),
        ("[subs=-callouts]\n a <1>\n b", &[("plain a <1>\n", 18, 23), ("plain b", 25, 25)]),
        // An escaping backslash a reference expands to is not text; the
        // escaped macro is its own bytes.
        (":s: \\\n\nS {s}xref:t[E] z", &[("plain S ", 7, 8), ("plain xref:t[E]", 12, 20), ("plain  z", 21, 22)]),
        // A line the preprocessor drops splits the text around it.
        ("A b\n// c\nD e", &[("plain A b\n", 0, 3), ("plain D e", 9, 11)]),
        ("S \\pass:[a +x+ b] c",&[("plain S ", 0, 1), ("plain pass:[a ", 2, 10), ("raw x", 12, 12), ("plain  b]", 14, 16), ("plain  c", 17, 18)]),
        // A label's escaped quotes, beside a passthrough, are their backslash and
        // their quote.
        ("P xref:t[\"pass:[a] \\\"q\\\" *b*\",role=h] e", &[("plain P ", 0, 1), ("xref", 2, 36), ("raw a", 16, 16), ("plain  \"q\" ", 18, 24), ("bold", 25, 27), ("plain b", 26, 26), ("plain  e", 37, 38)]),
    ];
    let mut failures = Vec::new();
    for (doc, want) in rows {
        let parsed = parse(doc, &options)?;
        let mut got = Vec::new();
        for block in parsed.document().blocks() {
            let BlockKind::Paragraph(p) = block.kind() else {
                return Err(format!("{doc:?}: not a paragraph").into());
            };
            spans(p.content(), &mut got);
        }
        if got != owned(want) {
            failures.push(format!("{doc:?}\n  got  {got:?}\n  want {want:?}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}
