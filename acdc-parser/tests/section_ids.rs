//! Section and discrete heading ids are allocated the way asciidoctor allocates them:
//! one document-wide registry filled in parse order. An explicit id is kept as written;
//! a generated id takes the first free `_2`, `_3`, … suffix. What reserves a name is
//! exactly what asciidoctor catalogs while parsing — block ids (after the block's
//! content), anchors in paragraph text, the anchor that opens a list item, a
//! description list term, a PSV table cell or a bibliography entry, and the document
//! id. Every expected id below is asciidoctor 2.0.26's output for the same source.

use acdc_parser::{Options, WarningKind, parse};
use serde_json::Value;

/// `(name, id)` of every section and discrete heading in the serialized ASG, in
/// document order.
fn asg_headings(source: &str) -> Vec<(String, String)> {
    asg_headings_with(source, &Options::default())
}

fn asg_headings_with(source: &str, options: &Options) -> Vec<(String, String)> {
    let result = parse(source, options).expect("parse succeeds");
    let asg = serde_json::to_value(result.document()).expect("document serializes");
    let mut headings = Vec::new();
    collect_headings(&asg, &mut headings);
    headings
}

fn collect_headings(value: &Value, headings: &mut Vec<(String, String)>) {
    match value {
        Value::Object(map) => {
            if let Some(name @ ("section" | "heading")) = map.get("name").and_then(Value::as_str) {
                let id = map.get("id").and_then(Value::as_str).unwrap_or("<no id>");
                headings.push((name.to_string(), id.to_string()));
            }
            for child in map.values() {
                collect_headings(child, headings);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_headings(item, headings);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn ids(source: &str) -> Vec<String> {
    asg_headings(source).into_iter().map(|(_, id)| id).collect()
}

fn headings(source: &str) -> Vec<String> {
    asg_headings(source)
        .into_iter()
        .map(|(name, id)| format!("{name} {id}"))
        .collect()
}

#[test]
fn a_repeated_title_takes_the_first_free_suffix() {
    // `Intro 2` already owns `_intro_2`, so the third heading moves on to `_intro_3`.
    assert_eq!(
        ids("== Intro\n\n== Intro 2\n\n== Intro\n"),
        ["_intro", "_intro_2", "_intro_3"]
    );
}

#[test]
fn an_earlier_explicit_id_pushes_a_generated_id_to_a_suffix() {
    assert_eq!(
        ids("[[_explicit]]\n== Explicit Anchor\n\n== Explicit\n"),
        ["_explicit", "_explicit_2"]
    );
}

#[test]
fn a_later_explicit_id_does_not_renumber_an_earlier_generated_id() {
    assert_eq!(
        ids("== Explicit\n\n[[_explicit]]\n== Explicit Anchor\n"),
        ["_explicit", "_explicit"]
    );
}

#[test]
fn sections_and_discrete_headings_share_one_registry() {
    assert_eq!(
        headings("== Foo\n\n[discrete]\n== Foo\n\n== Foo\n"),
        ["section _foo", "heading _foo_2", "section _foo_3"]
    );
}

#[test]
fn nested_sections_share_one_registry() {
    assert_eq!(
        ids("== Parent A\n\n=== Details\n\n== Parent B\n\n=== Details\n"),
        ["_parent_a", "_details", "_parent_b", "_details_2"]
    );
}

#[cfg(feature = "setext")]
#[test]
fn setext_sections_share_one_registry() {
    // Setext titles are opt-in in acdc; asciidoctor always reads them.
    let options = Options::builder().with_setext().build();
    let ids: Vec<String> = asg_headings_with(
        "Setext Title\n------------\n\nSetext Title\n------------\n",
        &options,
    )
    .into_iter()
    .map(|(_, id)| id)
    .collect();
    assert_eq!(ids, ["_setext_title", "_setext_title_2"]);
}

#[test]
fn discrete_headings_in_asciidoc_table_cells_share_one_registry() {
    assert_eq!(
        ids("[cols=\"1a\"]\n|===\n|\n[discrete]\n== Floating\n|===\n\n[discrete]\n== Floating\n"),
        ["_floating", "_floating_2"]
    );
}

#[test]
fn titles_without_id_characters_share_one_registry() {
    assert_eq!(ids("== ---\n\n== ---\n"), ["", "_2"]);
}

#[test]
fn anchors_in_paragraph_text_reserve_their_ids() {
    for (source, expected) in [
        ("Some text [[_alpha]] here.\n\n== Alpha\n", "_alpha_2"),
        ("NOTE: admonition [[_kappa]] text.\n\n== Kappa\n", "_kappa_2"),
        (
            "[quote]\n____\nquoted [[_lambda]] text\n____\n\n== Lambda\n",
            "_lambda_2",
        ),
        ("====\npara [[_rho]] text\n====\n\n== Rho\n", "_rho_2"),
        (
            "[cols=\"1a\"]\n|===\n|\npara [[_phi]] text\n|===\n\n== Phi\n",
            "_phi_2",
        ),
    ] {
        assert_eq!(ids(source), [expected], "{source}");
    }
}

#[test]
fn block_ids_reserve_their_ids() {
    for (source, expected) in [
        ("[[_beta]]\n----\ncode\n----\n\n== Beta\n", "_beta_2"),
        ("[[_iota]]\n|===\n|cell\n|===\n\n== Iota\n", "_iota_2"),
        ("[[_upsilon]]\n'''\n\n== Upsilon\n", "_upsilon_2"),
    ] {
        assert_eq!(ids(source), [expected], "{source}");
    }
}

#[test]
fn a_block_id_is_reserved_after_the_block_content() {
    // The discrete heading inside the sidebar is cataloged first and keeps `_epsilon`;
    // asciidoctor then warns that the sidebar's own id is already in use.
    assert_eq!(
        headings("[[_epsilon]]\n****\n[discrete]\n== Epsilon\n****\n"),
        ["heading _epsilon"]
    );
}

#[test]
fn bibliography_anchors_and_the_document_id_reserve_their_ids() {
    assert_eq!(
        ids("[bibliography]\n== References\n\n* [[[_intro]]] Some book.\n\n== Intro\n"),
        ["_references", "_intro_2"]
    );
    // The document title itself generates no id, so `_document_title` stays free.
    assert_eq!(
        ids("[[_intro]]\n= Document Title\n\n== Intro\n\n== Document Title\n"),
        ["_intro_2", "_document_title"]
    );
}

/// Sources whose last heading id is not the expected one.
fn id_mismatches(cases: &[(&str, &str)]) -> Vec<String> {
    cases
        .iter()
        .filter_map(|(source, expected)| {
            let actual = ids(source);
            (actual.last().map(String::as_str) != Some(*expected))
                .then(|| format!("{source:?}: got {actual:?}, want {expected} last"))
        })
        .collect()
}

#[test]
fn an_anchor_opening_a_list_item_term_or_psv_cell_reserves_its_id() {
    let mismatches = id_mismatches(&[
        ("* [[_eta]] item\n\n== Eta\n", "_eta_2"),
        ("* [[_eta, Eta Text]] item\n\n== Eta\n", "_eta_2"),
        ("* [[_eta]]item\n\n== Eta\n", "_eta_2"),
        ("* outer\n** [[_eta]] inner\n\n== Eta\n", "_eta_2"),
        (". [[_eta]] item\n\n== Eta\n", "_eta_2"),
        ("----\ncode <1>\n----\n<1> [[_eta]] note\n\n== Eta\n", "_eta_2"),
        ("[bibliography]\n* [[[_eta]]] item\n\n== Eta\n", "_eta_2"),
        // An unstyled bulleted list directly in a bibliography section is a bibliography.
        ("[bibliography]\n== References\n\n* [[[_eta]]] book\n\n== Eta\n", "_eta_2"),
        ("* [[_eta]] item\n+\n[discrete]\n== Eta\n", "_eta_2"),
        ("[[_eta]]term:: desc\n\n== Eta\n", "_eta_2"),
        ("[[_eta]] term:: desc\n\n== Eta\n", "_eta_2"),
        ("[qanda]\n[[_eta]]Question?::\nAnswer\n\n== Eta\n", "_eta_2"),
        ("|===\n|[[_eta]]cell\n|===\n\n== Eta\n", "_eta_2"),
        ("|===\n| [[_eta]] cell\n|===\n\n== Eta\n", "_eta_2"),
        ("[cols=\"1e\"]\n|===\n|[[_eta]]cell\n|===\n\n== Eta\n", "_eta_2"),
        ("[cols=\"1h\"]\n|===\n|[[_eta]]cell\n|===\n\n== Eta\n", "_eta_2"),
        ("[cols=\"1v\"]\n|===\n|[[_eta]]cell\n|===\n\n== Eta\n", "_eta_2"),
        ("[cols=\"1a\"]\n|===\n|[[_eta]]cell\n|===\n\n== Eta\n", "_eta_2"),
        ("[%header]\n|===\n|[[_eta]]head\n\n|body\n|===\n\n== Eta\n", "_eta_2"),
        ("|===\n|[[_eta]]head\n\n|body\n|===\n\n== Eta\n", "_eta_2"),
        ("[%footer]\n|===\n|body\n\n|[[_eta]]foot\n|===\n\n== Eta\n", "_eta_2"),
        ("[format=psv]\n,===\n|[[_eta]]cell\n,===\n\n== Eta\n", "_eta_2"),
        ("[cols=\"1a\"]\n|===\n|\n!===\n![[_eta]]inner\n!===\n|===\n\n== Eta\n", "_eta_2"),
        // A header row cell is a normal cell whatever its column or cell style.
        ("[cols=\"1a\"]\n|===\n|[[_eta]]head\n\n|body\n|===\n\n== Eta\n", "_eta_2"),
        ("[cols=\"1l\"]\n|===\n|[[_eta]]head\n\n|body\n|===\n\n== Eta\n", "_eta_2"),
        ("[%header]\n|===\na|[[_eta]]head\n\n|body\n|===\n\n== Eta\n", "_eta_2"),
        // A footer row is a body row: an `a` cell reserves every anchor in its paragraphs.
        ("[cols=\"1a\",%footer]\n|===\n|body\n\n|foot [[_eta]]\n|===\n\n== Eta\n", "_eta_2"),
    ]);
    assert!(mismatches.is_empty(), "{mismatches:#?}");
}

#[test]
fn anchors_cataloged_only_at_conversion_do_not_reserve_ids() {
    let mismatches = id_mismatches(&[
        ("Some [#_delta]#formatted# text.\n\n== Delta\n", "_delta"),
        ("[verse]\n____\nverse [[_mu]] text\n____\n\n== Mu\n", "_mu"),
        (".Title [[_xi]]\n----\ncode\n----\n\n== Xi\n", "_xi"),
        ("== Eta\n\n* [[_eta]] item\n", "_eta"),
        ("* item [[_eta]] one\n\n== Eta\n", "_eta"),
        ("* [[[_eta]]] item\n\n== Eta\n", "_eta"),
        ("[bibliography]\n* [[_eta]] item\n\n== Eta\n", "_eta"),
        ("[bibliography]\n* Some [[[_eta]]] book\n\n== Eta\n", "_eta"),
        ("[bibliography]\n* book\n** [[[_eta]]] nested\n\n== Eta\n", "_eta"),
        ("[bibliography]\n== References\n\n* [[_eta]] book\n\n== Eta\n", "_eta"),
        ("[bibliography]\n== References\n\n=== Books\n\n* [[[_eta]]] book\n\n== Eta\n", "_eta"),
        ("[bibliography]\n== References\n\n====\n* [[[_eta]]] book\n====\n\n== Eta\n", "_eta"),
        ("[bibliography]\n== References\n\n[square]\n* [[[_eta]]] book\n\n== Eta\n", "_eta"),
        ("[bibliography]\n== References\n\n* book\n** [[[_eta]]] nested\n\n== Eta\n", "_eta"),
        ("[bibliography]\n== References\n\n. [[[_eta]]] book\n\n== Eta\n", "_eta"),
        ("* [x] [[_eta]] item\n\n== Eta\n", "_eta"),
        (". [x] [[_eta]] item\n\n== Eta\n", "_eta"),
        ("Term [[_eta]]:: description\n\n== Eta\n", "_eta"),
        ("term:: [[_eta]] desc\n\n== Eta\n", "_eta"),
        ("|===\n|cell [[_eta]] text\n|===\n\n== Eta\n", "_eta"),
        ("[cols=\"1l\"]\n|===\n|[[_eta]]cell\n|===\n\n== Eta\n", "_eta"),
        ("|===\nl|[[_eta]]cell\n|===\n\n== Eta\n", "_eta"),
        ("[format=csv]\n|===\n[[_eta]]cell\n|===\n\n== Eta\n", "_eta"),
        ("[format=dsv]\n|===\n[[_eta]]cell\n|===\n\n== Eta\n", "_eta"),
        ("[format=tsv]\n|===\n[[_eta]]cell\n|===\n\n== Eta\n", "_eta"),
        (",===\n[[_eta]]cell\n,===\n\n== Eta\n", "_eta"),
        (":===\n[[_eta]]cell\n:===\n\n== Eta\n", "_eta"),
        ("[cols=\"1a\"]\n|===\n|head [[_eta]]\n\n|body\n|===\n\n== Eta\n", "_eta"),
        ("[cols=\"1a\",%header]\n|===\n|head [[_eta]]\n\n|body\n|===\n\n== Eta\n", "_eta"),
    ]);
    assert!(mismatches.is_empty(), "{mismatches:#?}");
}

#[test]
fn toc_entries_carry_the_allocated_section_ids() {
    let result = parse(
        "== Intro\n\n[discrete]\n== Intro\n\n=== Intro\n\n== Intro\n",
        &Options::default(),
    )
    .expect("parse succeeds");
    let toc: Vec<&str> = result
        .document()
        .toc_entries
        .iter()
        .map(|entry| entry.id)
        .collect();
    // The discrete heading takes `_intro_2` but is not a TOC entry.
    assert_eq!(toc, ["_intro", "_intro_3", "_intro_4"]);
}

#[test]
fn cross_references_resolve_to_allocated_ids() {
    let result = parse(
        "== Intro\n\n== Intro\n\n[discrete]\n== Floating\n\nSee <<_intro_2>> and <<_floating>>.\n",
        &Options::default(),
    )
    .expect("parse succeeds");
    let unresolved: Vec<&WarningKind> = result
        .warnings()
        .iter()
        .map(|warning| &warning.kind)
        .filter(|kind| matches!(kind, WarningKind::UnresolvedReference { .. }))
        .collect();
    assert!(unresolved.is_empty(), "{unresolved:?}");
    let references = &result.document().references;
    assert!(references.contains_key("_intro_2"), "{references:?}");
    assert!(references.contains_key("_floating"), "{references:?}");
}
