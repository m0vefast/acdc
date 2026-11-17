//! A discrete heading gets an ID as a section does: its explicit ID, or one
//! generated from its title, registered and made unique in document order
//! together with sections (asciidoctor `Parser.next_block`, floating titles).

use acdc_parser::{
    Options,
    ast::{BlockKind, BlockRef, IdOrigin},
    parse,
};

type Error = Box<dyn std::error::Error>;

fn heading_ids(source: &str) -> Result<Vec<(String, Option<(String, IdOrigin)>)>, Error> {
    let parsed = parse(source, &Options::default())?;
    Ok(parsed
        .document()
        .blocks()
        .filter_map(|block| match block.kind() {
            BlockKind::DiscreteHeading(h) => Some((
                "discrete".to_string(),
                h.id().map(|id| (id.as_str().to_string(), id.origin())),
            )),
            BlockKind::Section(s) => Some((
                "section".to_string(),
                s.id().map(|id| (id.as_str().to_string(), id.origin())),
            )),
            _ => None,
        })
        .collect())
}

fn row(kind: &str, id: &str, origin: IdOrigin) -> (String, Option<(String, IdOrigin)>) {
    (kind.to_string(), Some((id.to_string(), origin)))
}

#[test]
fn discrete_headings_get_unique_generated_ids_in_document_order() -> Result<(), Error> {
    let source = "[discrete]\n== Hello\n\n[discrete]\n== Hello\n\n== Hello\n\n<<_hello_2>>\n";
    assert_eq!(
        heading_ids(source)?,
        [
            row("discrete", "_hello", IdOrigin::Generated),
            row("discrete", "_hello_2", IdOrigin::Generated),
            row("section", "_hello_3", IdOrigin::Generated),
        ]
    );
    let parsed = parse(source, &Options::default())?;
    let references = parsed.document().references();
    for id in ["_hello", "_hello_2", "_hello_3"] {
        assert!(references.contains(id), "{id} is not registered");
    }
    Ok(())
}

#[test]
fn a_discrete_heading_keeps_its_explicit_id() -> Result<(), Error> {
    for source in [
        "[discrete#x]\n== Hello\n\n== Hello\n",
        "[[x]]\n[discrete]\n== Hello\n\n== Hello\n",
    ] {
        let ids = heading_ids(source)?;
        assert_eq!(
            ids.iter().map(|(kind, id)| (kind.as_str(), id.as_ref().map(|(id, _)| id.as_str()))).collect::<Vec<_>>(),
            [("discrete", Some("x")), ("section", Some("_hello"))],
            "{source:?}"
        );
    }
    Ok(())
}

#[test]
fn a_discrete_heading_inside_a_block_is_registered() -> Result<(), Error> {
    let source = "****\n[discrete]\n== Inside\n****\n";
    let parsed = parse(source, &Options::default())?;
    assert!(parsed.document().references().contains("_inside"));
    let Some(BlockKind::Sidebar(sidebar)) = parsed.document().blocks().next().map(BlockRef::kind) else {
        return Err("expected a sidebar".into());
    };
    let Some(BlockKind::DiscreteHeading(heading)) = sidebar.blocks().next().map(BlockRef::kind) else {
        return Err("expected a discrete heading".into());
    };
    assert_eq!(heading.id().map(|id| id.as_str()), Some("_inside"));
    Ok(())
}
