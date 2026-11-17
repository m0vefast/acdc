//! A generated section ID is read from the title as Asciidoctor converts it
//! (`Section.generate_id` over the substituted title, `InvalidSectionIdCharsRx`):
//! replacements are entities and vanish with the spaces they replaced, a
//! footnote draws its number, a cross-reference without text draws its target's
//! title once that target is registered, and a trailing ` [[id]]` names the
//! section (`Parser.parse_section_title`, `InlineSectionAnchorRx`). Every
//! expected ID is Asciidoctor 2.0.26's output for the same source.

use acdc_parser::{
    Options,
    ast::{BlockKind, BlockRef},
    parse,
};

type Error = Box<dyn std::error::Error>;

fn section_ids(source: &str) -> Result<Vec<String>, Error> {
    let parsed = parse(source, &Options::default())?;
    Ok(parsed
        .document()
        .blocks()
        .filter_map(|block| match block.kind() {
            BlockKind::Section(section) => Some(section.resolve_id().into_owned()),
            _ => None,
        })
        .collect())
}

#[test]
fn generated_ids_follow_the_converted_title() -> Result<(), Error> {
    let rows: &[(&str, &[&str])] = &[
        ("== _alpha_ s #Docs# a -- b", &["_alpha_s_docs_ab"]),
        ("== a--b and c -- d", &["_ab_and_cd"]),
        ("== Copyright (C) 2024 ... done", &["_copyright_2024_done"]),
        ("== it's here", &["_its_here"]),
        ("== bebs [[a1]]", &["a1"]),
        ("== ata taaaen中 [[a1]]", &["a1"]),
        ("== with ref [[r1,Ref Text]]", &["r1"]),
        ("== escaped \\[[x1]]", &["_escaped_x1"]),
        ("== se e footnote:[note]", &["_se_e_1"]),
        ("== a footnote:[x] b footnote:[y]", &["_a_1_b_2"]),
        (
            "[[scope]]\n== _Roadmap_\n\n== #alpha# #plan# <<scope>> oxxe",
            &["scope", "_alpha_plan_roadmap_oxxe"],
        ),
        ("== see <<later>> now\n\n[[later]]\n== Later", &["_see_later_now", "later"]),
        ("== see <<scope,Custom>> x\n\n[[scope]]\n== T", &["_see_custom_x", "scope"]),
        ("== *", &[""]),
    ];
    let mut failures = Vec::new();
    for (source, want) in rows {
        let got = section_ids(source)?;
        if got != *want {
            failures.push(format!("{source:?}\n  got  {got:?}\n  want {want:?}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}

/// A closing marker the same as the opening one ends the title
/// (`AtxSectionTitleRx`: `(?:[ \t]+\1)?$`); any other stays in it.
#[test]
fn a_matching_closing_marker_leaves_the_title() -> Result<(), Error> {
    let title_text = |source: &str| -> Result<(Option<String>, Option<String>), Error> {
        let parsed = parse(source, &Options::default())?;
        let text = |content: Option<acdc_parser::ast::ContentRef<'_>>| {
            content.map(|c| {
                c.inlines()
                    .filter_map(|node| match node.kind() {
                        acdc_parser::ast::InlineKind::Plain(t) => Some(t.text().to_string()),
                        _ => None,
                    })
                    .collect::<String>()
            })
        };
        let section = parsed.document().blocks().find_map(|block| match block.kind() {
            BlockKind::Section(section) => Some(section),
            _ => None,
        });
        Ok((text(parsed.document().title()), text(section.and_then(|s| s.title()))))
    };
    let rows: &[(&str, (Option<&str>, Option<&str>))] = &[
        ("= T x =\n\nbody\n", (Some("T x"), None)),
        ("== Sec ==\n\nbody\n", (None, Some("Sec"))),
        ("== Sec =\n\nbody\n", (None, Some("Sec ="))),
        ("== Sec ===\n\nbody\n", (None, Some("Sec ==="))),
        ("## Md ##\n\nbody\n", (None, Some("Md"))),
        ("= T = x\n\nbody\n", (Some("T = x"), None)),
    ];
    let mut failures = Vec::new();
    for (source, (doc, sec)) in rows {
        let got = title_text(source)?;
        let want = (doc.map(str::to_string), sec.map(str::to_string));
        if got != want {
            failures.push(format!("{source:?}\n  got  {got:?}\n  want {want:?}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}

/// The document title is read the same way: a trailing ` [[id]]` is the
/// document's ID (`Parser.parse_document_header` → `parse_section_title`).
#[rstest::rstest]
#[case("= st [[a1]]\n", "st", "a1")]
#[case("= s <o [[a1]]\n", "s <o", "a1")]
#[case("[[top]]\n= st [[a1]]\n", "st [[a1]]", "top")]
fn a_trailing_anchor_names_the_document(
    #[case] source: &str,
    #[case] title: &str,
    #[case] id: &str,
) -> Result<(), Error> {
    let parsed = parse(source, &Options::default())?;
    let text: String = parsed
        .document()
        .title()
        .ok_or("title")?
        .inlines()
        .map(|node| match node.kind() {
            acdc_parser::ast::InlineKind::Plain(text) => text.text().to_string(),
            acdc_parser::ast::InlineKind::Anchor(_) => "[[a1]]".to_string(),
            _ => String::new(),
        })
        .collect();
    assert_eq!(text, title);
    assert!(parsed.document().references().contains(id), "{id} is not registered");
    Ok(())
}

/// The trailing anchor is the section's ID, not part of its title.
#[test]
fn a_trailing_section_anchor_leaves_the_title() -> Result<(), Error> {
    let parsed = parse("== bebs [[a1]]\n", &Options::default())?;
    let Some(BlockKind::Section(section)) = parsed.document().blocks().next().map(BlockRef::kind) else {
        return Err("expected a section".into());
    };
    let title: String = section
        .title()
        .ok_or("title")?
        .inlines()
        .filter_map(|node| match node.kind() {
            acdc_parser::ast::InlineKind::Plain(text) => Some(text.text().to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(title, "bebs");
    assert!(parsed.document().references().contains("a1"));
    Ok(())
}
