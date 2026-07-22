//! P0 regression pin: a rowspan that covers a FULL grid row must not
//! cascade-drop the rest of the table body.
//!
//! `grid_reflow` (table.rs) models asciidoctor's row fill: a grid row wholly
//! covered by a rowspan from above consumes+discards exactly ONE filler cell
//! (bounded — total rows <= cell count) and EMITS an empty phantom row. The
//! document.rs builder SEES that phantom row, ages its own rowspan tracker on it
//! (staying in lockstep with grid_reflow), and drops it from output — so the
//! covering `rowspan` overlaps the next real row, which then lands on the NORMAL
//! path (occupancy is exact, so it is not falsely over-counted). Genuine
//! over-wide / overflow rows still DROP (matching asciidoctor). Earlier
//! regressions either dropped the whole body (aging desync) or emitted over-wide
//! rows (over-generalized over-count EMIT) — these tests pin both directions.

use acdc_parser::{Options, SafeMode, parse};

fn parse_table_json(src: &str) -> String {
    let opts = Options::builder().with_safe_mode(SafeMode::Unsafe).build();
    let result = parse(src, &opts).expect("parse succeeds");
    serde_json::to_string(result.document()).expect("serialize doc")
}

#[test]
fn single_col_rowspan_does_not_cascade_drop_trailing_rows() {
    // 1-col table, `Alpha` rowspan 2 covers rows 0+1. `Beta` collides with the
    // span (asciidoctor drops it too), `Gamma` lands on the row after the span.
    // Pre-fix acdc dropped BOTH Beta and Gamma (cascade) → only Alpha survived.
    let json = parse_table_json("[cols=1]\n|===\n.2+|Alpha\n|Beta\n|Gamma\n|===\n");
    assert!(
        json.contains("Gamma"),
        "Gamma (row after the rowspan band) must survive — cascade-drop bug. json={json}"
    );
}

#[test]
fn tall_rowspan_does_not_cascade_drop_trailing_rows() {
    // `Alpha` rowspan 3 covers rows 0..2; `Delta` is the first free row after.
    let json = parse_table_json("[cols=1]\n|===\n.3+|Alpha\n|Beta\n|Gamma\n|Delta\n|===\n");
    assert!(
        json.contains("Delta"),
        "Delta (row after a rowspan-3 band) must survive. json={json}"
    );
}

#[test]
fn side_by_side_full_width_rowspans_do_not_cascade_drop() {
    // Two adjacent rowspan-2 cells jointly cover the full width of one grid row.
    // asciidoctor keeps the trailing `Echo`/`Foxtrot` row; pre-fix acdc dropped it.
    let json = parse_table_json(
        "[cols=2]\n|===\n.2+|Alpha .2+|Bravo\n|Charlie |Delta\n|Echo |Foxtrot\n|===\n",
    );
    assert!(
        json.contains("Echo") || json.contains("Foxtrot"),
        "the row after two side-by-side rowspans must survive. json={json}"
    );
}

#[test]
fn full_width_rowspan_overlap_row_is_emitted() {
    // `2.2+|A` (colspan==ncols, rowspan 2) fully covers the grid row below it;
    // `Beta` is the discarded phantom filler; `Cee|Dee` form the real overlap row
    // that asciidoctor emits under A's rowspan (verified `[A:c2r2] [Cee,Dee]`).
    // Regression guard: the two rowspan trackers must stay synced so this row is
    // NOT falsely over-counted (which dropped Cee/Dee) NOR emitted as an empty
    // phantom <tr> that inflates the row count.
    let json = parse_table_json("[cols=\"2*\"]\n|===\n2.2+|A\n|Beta\n|Cee\n|Dee\n|===\n");
    assert!(
        json.contains("Cee") && json.contains("Dee"),
        "the overlap row after a full-width rowspan must survive. json={json}"
    );
}

#[test]
fn footer_survives_rowspan_reaching_last_grid_row() {
    // A rowspan whose coverage reaches the final grid row makes grid_reflow emit
    // a trailing empty phantom row. `options="footer"` must still bind to the
    // real last row (asciidoctor: tfoot=`mid`), NOT be lost to the phantom row
    // that lands on `raw_rows.len()-1`.
    let json = parse_table_json(
        "[cols=\"1\",options=\"footer\"]\n|===\n| top\n.2+| mid\n| filler\n|===\n",
    );
    assert!(json.contains("mid"), "mid survives");
    assert!(
        !json.contains("\"footer\":null"),
        "footer must be populated (mid), not lost to the phantom row. json={json}"
    );
}

#[test]
fn footer_promotes_last_surviving_row_when_last_row_dropped() {
    // Over-count last row (`|A|B 2+|CC` = 4-in-3) is dropped; asciidoctor promotes
    // the previous row `[d,e,f]` to <tfoot> (verified). The footer must not vanish
    // (footer=None) nor keep the malformed row — mirror the header's model.
    let json = parse_table_json(
        "[cols=\"3*\",options=\"footer\"]\n|===\n|a |b |c\n|d |e |f\n|A |B 2+|CC\n|===\n",
    );
    assert!(
        !json.contains("\"footer\":null"),
        "footer promoted to the last surviving row, not lost. json={json}"
    );
    assert!(!json.contains("CC"), "the over-wide last row is dropped");
    assert!(
        json.contains("\"d\"") && json.contains("\"e\"") && json.contains("\"f\""),
        "d/e/f survive"
    );
}

#[test]
fn footer_promotes_last_surviving_row_when_last_row_overflows() {
    // Single-cell overflow last row (`5+|WIDE` in a 3-col table) is dropped;
    // asciidoctor promotes `[d,e,f]` to <tfoot> (verified).
    let json = parse_table_json(
        "[cols=\"3*\",options=\"footer\"]\n|===\n|a |b |c\n|d |e |f\n5+|WIDE\n|===\n",
    );
    assert!(
        !json.contains("\"footer\":null"),
        "footer not lost to the overflow-dropped last row. json={json}"
    );
    assert!(!json.contains("WIDE"), "the overflow last row is dropped");
}

#[test]
fn asciidoc_cell_with_internal_blank_keeps_post_blank_content() {
    // An `a|` cell with an INTERNAL blank line + a following cell
    // (`a|P1`⏎⏎`P2 |B`): asciidoctor keeps `P1`+blank+`P2` in the a| cell and `B`
    // as the 2nd cell (cols=2). The collector must NOT terminate the row at the
    // blank and drop `P2` — the a| cell stays open across the blank until the next
    // `|` (cell-open-across-blank / cell-streaming, not line-structure heuristic).
    let json = parse_table_json("|===\na|P1\n\nP2 |B\n|===\n");
    let doc: serde_json::Value = serde_json::from_str(&json).unwrap();
    let rows = &doc["blocks"][0]["content"]["rows"];
    assert_eq!(
        rows.as_array().map(std::vec::Vec::len),
        Some(1),
        "one 2-col row. json={json}"
    );
    let cols = rows[0]["columns"].as_array().unwrap();
    assert_eq!(
        cols.len(),
        2,
        "row has 2 cells: the a| cell and `B`. json={json}"
    );
    // Both P1 and P2 live inside the FIRST (a|) cell; B is the second cell.
    let cell0 = serde_json::to_string(&cols[0]).unwrap();
    assert!(
        cell0.contains("P1") && cell0.contains("P2"),
        "P1 and P2 are both in the a| cell (P2 not dropped at the blank). cell0={cell0}"
    );
    assert!(
        serde_json::to_string(&cols[1]).unwrap().contains("\"B\""),
        "the second cell is `B`"
    );
}

#[test]
fn asciidoc_cell_multiple_internal_blanks_and_following_row() {
    // Multi-paragraph a| cell (`a|P1`⏎⏎`P2`⏎⏎`P3 |B`) → 1 row [a|(P1,P2,P3), B].
    let json = parse_table_json("|===\na|P1\n\nP2\n\nP3 |B\n|===\n");
    let doc: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(
        doc["blocks"][0]["content"]["rows"]
            .as_array()
            .map(std::vec::Vec::len),
        Some(1)
    );
    let c0 = serde_json::to_string(&doc["blocks"][0]["content"]["rows"][0]["columns"][0]).unwrap();
    assert!(
        c0.contains("P1") && c0.contains("P2") && c0.contains("P3"),
        "all 3 paras in the a| cell. c0={c0}"
    );

    // An a| cell with a blank followed by a genuine NEW row must NOT over-collect
    // the new row into the a| cell (grid_reflow re-splits by ncols). Expect 2 rows.
    let json2 = parse_table_json("[cols=2]\n|===\na|P1\n\nP2 |B\n|C |D\n|===\n");
    let doc2: serde_json::Value = serde_json::from_str(&json2).unwrap();
    let rows2 = doc2["blocks"][0]["content"]["rows"].as_array().unwrap();
    assert_eq!(
        rows2.len(),
        2,
        "the a| row + a genuine new row = 2 rows. json2={json2}"
    );
    assert!(
        serde_json::to_string(&rows2[1]).unwrap().contains("\"C\""),
        "C/D form the 2nd row"
    );
}

#[test]
fn bare_integer_cols_is_column_count_not_width() {
    // asciidoctor shorthand: `cols=2` (a bare integer) = 2 COLUMNS, not one
    // width-2 column. `|a|b|c|d` → 2 rows of 2 cells. Previously acdc read the
    // bare `2` as a single column's width → 1 column → 4 one-cell rows.
    let json = parse_table_json("[cols=2]\n|===\n|a |b |c |d\n|===\n");
    let row_starts = json.matches("\"columns\":[{\"content\"").count();
    assert_eq!(
        row_starts, 2,
        "cols=2 → 2 rows of 2 cells (2 columns), not 4×1. json={json}"
    );
    assert!(
        json.contains("\"a\"") && json.contains("\"d\""),
        "all cells present"
    );
}

#[test]
fn absurd_bare_cols_count_is_bounded_no_oom() {
    // P1 (OOM guard): a bare `[cols=N]` (and `cols="N*"`) with an absurd N would
    // eagerly `vec!`-allocate N ColumnFormats and pad each incomplete row to N
    // cells → gigabytes / crash on a pasted/typo `.adoc`. It must be BOUNDED
    // (capped at MAX_TABLE_COLS=1000) while still parsing — authored cells kept.
    for src in [
        "[cols=100000000]\n|===\n|a |b\n|===\n",
        "[cols=\"100000000*\"]\n|===\n|a |b\n|===\n",
    ] {
        let json = parse_table_json(src);
        let doc: serde_json::Value = serde_json::from_str(&json).unwrap();
        // The (single, incomplete) row is padded to the CAPPED width, never N.
        let ncols = doc["blocks"][0]["content"]["rows"][0]["columns"]
            .as_array()
            .map_or(0, std::vec::Vec::len);
        assert!(
            (2..=1000).contains(&ncols),
            "row width must be bounded by the cap, not N. ncols={ncols} src={src:?}"
        );
        assert!(
            json.contains("\"a\"") && json.contains("\"b\""),
            "cells kept"
        );
    }
}

#[test]
fn zero_declared_columns_falls_back_to_implicit_not_empty_table() {
    // A declared ZERO column count — bare `cols=0`/`cols="0"` OR `cols="0*"` — is
    // not a real 0-col grid; asciidoctor IGNORES it and uses the implicit width
    // (3). Pre-fix, `0*` set ncols=Some(0) → the builder DROPPED the whole body
    // (data loss), and bare `0` fell to a width-0 branch → 1 corrupted column.
    // All three must render the content at the implicit 3-column count.
    for src in [
        "[cols=\"0*\"]\n|===\n|a |b |c\n|===\n",
        "[cols=0]\n|===\n|a |b |c\n|===\n",
        "[cols=\"0\"]\n|===\n|a |b |c\n|===\n",
    ] {
        let json = parse_table_json(src);
        let doc: serde_json::Value = serde_json::from_str(&json).unwrap();
        let cols = doc["blocks"][0]["content"]["rows"][0]["columns"]
            .as_array()
            .expect("body row present, not dropped");
        assert_eq!(
            cols.len(),
            3,
            "implicit 3-col table, not a drop/corruption. src={src:?}"
        );
        assert!(json.contains("\"a\"") && json.contains("\"b\"") && json.contains("\"c\""));
    }
}

#[test]
fn overwide_colspan_row_without_rowspan_is_dropped() {
    // P1: pure colspan overshoot (NO rowspan). `|cc 2+|dd` is 1+2 = 3 columns in
    // a 2-col table → asciidoctor DROPS the whole row (verified: only `[aa,bb]`,
    // `[ee,ff]`). It must NOT be emitted as an over-wide 3-column <tr>.
    let json = parse_table_json("[cols=\"2*\"]\n|===\n|aa |bb\n|cc 2+|dd\n|ee |ff\n|===\n");
    assert!(
        json.contains("aa") && json.contains("ff"),
        "clean rows survive"
    );
    assert!(
        !json.contains("cc") && !json.contains("dd"),
        "the over-wide (3-in-2) row must be dropped, matching asciidoctor. json={json}"
    );
}

#[test]
fn overwide_header_row_is_dropped_not_promoted() {
    // P1: an over-wide FIRST row must be dropped BEFORE the header split, so the
    // next well-formed row becomes the header (asciidoctor: thead=[aa,bb], no
    // malformed `[hh1, hh2:c2]` header). Guards against header corruption.
    let json = parse_table_json("[cols=\"2*\",options=header]\n|===\n|hh1 2+|hh2\n|aa |bb\n|===\n");
    assert!(
        !json.contains("hh1") && !json.contains("hh2"),
        "the malformed over-wide first row must not become the header. json={json}"
    );
    assert!(
        json.contains("aa") && json.contains("bb"),
        "next row promoted to header"
    );
}

#[test]
fn duplication_straddling_smaller_cols_wraps_not_overwide() {
    // A `N*` dup cell whose width exceeds the explicit `[cols=N]` must WRAP its
    // copies across grid rows (asciidoctor: `[a,b]` / `[b,b]`), NOT form one
    // over-wide 4-column row and NOT drop the whole row (never lose the copies).
    let json = parse_table_json("[cols=\"2*\"]\n|===\n|aa 3*|bb\n|===\n");
    // 4 cells total (aa + 3× bb) preserved across two 2-col rows.
    assert_eq!(
        json.matches("\"bb\"").count(),
        3,
        "all 3 dup copies survive. json={json}"
    );
    assert!(json.contains("aa"), "the leading cell survives");
}

#[test]
fn mid_column_rowspan_crossed_by_colspan_keeps_cells() {
    // P1 (well-formed, asciidoctor 0 warnings): a rowspan in a MIDDLE column
    // (`.3+|b` at col1) covering rows below, with colspan cells in those rows.
    // asciidoctor's count-model closes each covered row when
    // colspan-sum + rowspan-width == ncols → `[a,b:r3,c]` `[de:c2]` `[gh:c2]`.
    // A positional skip-model packed `[de,gh]` into one over-wide row (de's
    // colspan crossed b's covered column) → the builder dropped BOTH de and gh
    // (silent data loss). The count-model keeps them.
    let json = parse_table_json("[cols=\"3*\"]\n|===\n|a .3+|b |c\n2+|de\n2+|gh\n|===\n");
    assert!(
        json.contains("de") && json.contains("gh"),
        "de/gh must survive (each in its own covered row), not be dropped. json={json}"
    );
}

#[test]
fn mid_column_rowspan_crossed_by_mixed_colspans_keeps_cells() {
    // Repro B: `.3+|b` at col1 in a 4-col table; covered rows mix colspan+plain.
    // asciidoctor: `[a,b:r3,c,d]` `[ef:c2,g]` `[h,ij:c2]` — all cells kept.
    let json = parse_table_json("[cols=\"4*\"]\n|===\n|a .3+|b |c |d\n2+|ef |g\n|h 2+|ij\n|===\n");
    assert!(
        json.contains("ef")
            && json.contains("\"g\"")
            && json.contains("\"h\"")
            && json.contains("ij"),
        "ef/g/h/ij must all survive the mid-column rowspan crossing. json={json}"
    );
}

#[test]
fn colspan_straddling_rowspan_free_columns_is_dropped() {
    // P1: `3+|Wide` (colspan 3) under a rowspan covering one column of a 3-col
    // table can only use 2 free columns → asciidoctor DROPS it as overflow
    // (verified: only `[Aye:r2, Bee, Cee]`). The overflow boundary is colspan vs
    // FREE columns (ncols - occupied), not colspan vs ncols.
    let json = parse_table_json("[cols=\"3*\"]\n|===\n.2+|Aye\n|Bee |Cee\n3+|Wide\n|===\n");
    assert!(
        json.contains("Aye") && json.contains("Bee"),
        "the rowspan row survives"
    );
    assert!(
        !json.contains("Wide"),
        "a colspan straddling the free columns under a rowspan must drop. json={json}"
    );
}

#[test]
fn blank_absorbed_into_open_simple_cell_when_continuation_follows() {
    // asciidoctor's cell model (NOT a| specific): a blank line inside an UNFILLED
    // row is an intra-cell paragraph break when the next non-blank line is a
    // CONTINUATION (leading content, no leading separator). `[cols=3] |a |b`⏎⏎
    // `c |d` → `[a, "b\n\nc", d]` (1 row, 3 cols) — `c` joins the open simple cell
    // `b`. The old line-structure heuristic terminated the row at the blank and
    // dropped the pre-`|` leading content `c` (silent data loss vs asciidoctor).
    let json = parse_table_json("[cols=3]\n|===\n|a |b\n\nc |d\n|===\n");
    let doc: serde_json::Value = serde_json::from_str(&json).unwrap();
    let rows = doc["blocks"][0]["content"]["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "one 3-col row. json={json}");
    let cols = rows[0]["columns"].as_array().unwrap();
    assert_eq!(cols.len(), 3, "3 cells. json={json}");
    let cell1 = serde_json::to_string(&cols[1]).unwrap();
    assert!(
        cell1.contains("\"b\"") && cell1.contains("\"c\""),
        "`c` joins the open simple cell `b` across the blank (not dropped). cell1={cell1}"
    );
    assert!(
        serde_json::to_string(&cols[2]).unwrap().contains("\"d\""),
        "`d` is the 3rd cell"
    );
}

#[test]
fn blank_before_separator_led_line_terminates_row_no_residue() {
    // The mirror of the above: when the next non-blank line STARTS with a
    // separator (`|c |d`), the blank is a genuine row terminator — it must NOT be
    // absorbed into the previous row's last cell (which would leave a trailing
    // `\n` residue, esp. visible for `l|` literal cells). `[cols=2] |x`⏎`l|lit`⏎⏎
    // `|c |d` → `[[x, lit], [c, d]]` with `lit` unpolluted by the separator blank.
    let json = parse_table_json("[cols=2]\n|===\n|x\nl|lit\n\n|c |d\n|===\n");
    let doc: serde_json::Value = serde_json::from_str(&json).unwrap();
    let rows = doc["blocks"][0]["content"]["rows"].as_array().unwrap();
    assert_eq!(
        rows.len(),
        2,
        "two rows (blank is a terminator). json={json}"
    );
    let lit = serde_json::to_string(&rows[0]["columns"][1]).unwrap();
    assert!(
        lit.contains("lit") && !lit.contains("lit\\n"),
        "the l| cell keeps `lit` with no trailing-newline residue from the separator blank. lit={lit}"
    );
    assert!(
        serde_json::to_string(&rows[1]).unwrap().contains("\"c\""),
        "c/d form the 2nd row"
    );
}

#[test]
fn per_column_style_is_not_baked_into_header_cells() {
    // asciidoctor applies per-column ALIGNMENT to header cells but NEVER
    // per-column STYLE (`m`/`s`/`e`/`l`/`a`). `[%header,cols="m,s"]` → header
    // cells render plain; body cells get monospace/strong. The parser must NOT
    // bake col_format.style onto the header row (Glyph reads TableColumn.style
    // via asg-to-mdast, so a baked style would wrongly style the header).
    let json = parse_table_json("[%header,cols=\"m,s\"]\n|===\n|H1 |H2\n\n|x |y\n|===\n");
    let doc: serde_json::Value = serde_json::from_str(&json).unwrap();
    let hcols = doc["blocks"][0]["content"]["header"]["columns"]
        .as_array()
        .unwrap();
    assert!(
        hcols[0].get("style").is_none() && hcols[1].get("style").is_none(),
        "header cells carry NO per-column style. header={hcols:?}"
    );
    let bcols = doc["blocks"][0]["content"]["rows"][0]["columns"]
        .as_array()
        .unwrap();
    assert_eq!(
        bcols[0]["style"], "monospace",
        "body col 0 keeps the `m` style"
    );
    assert_eq!(
        bcols[1]["style"], "strong",
        "body col 1 keeps the `s` style"
    );
}

#[test]
fn implicit_header_fires_only_from_first_physical_line_plus_blank() {
    // asciidoctor implicit-header rule: fires iff the FIRST PHYSICAL line is
    // immediately followed by a blank AND the next non-blank line starts a NEW
    // row. `|a |b`⏎⏎`|c |d` → header=[a,b], body=[c,d].
    let json = parse_table_json("|===\n|a |b\n\n|c |d\n|===\n");
    let doc: serde_json::Value = serde_json::from_str(&json).unwrap();
    let hdr = serde_json::to_string(&doc["blocks"][0]["content"]["header"]).unwrap();
    assert!(
        hdr.contains("\"a\"") && hdr.contains("\"b\""),
        "first row [a,b] is the header. json={json}"
    );
    let rows = doc["blocks"][0]["content"]["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "one body row [c,d]. json={json}");
    assert!(serde_json::to_string(&rows[0]).unwrap().contains("\"c\""));
}

#[test]
fn no_implicit_header_when_first_row_spans_multiple_physical_lines() {
    // The merge-corruption regression: `|x`⏎`l|lit`⏎⏎`|c |d` — the collector
    // MERGES `|x`+`l|lit` into one logical first row `[x, lit]`, then a blank
    // follows. asciidoctor does NOT promote it (the first PHYSICAL line `|x` was
    // followed by `l|lit`, not a blank) → NO header, tbody=[x,lit],[c,d]. Guards
    // against re-evaluating the header per merged row.
    let json = parse_table_json("[cols=2]\n|===\n|x\nl|lit\n\n|c |d\n|===\n");
    let doc: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(
        doc["blocks"][0]["content"]["header"].is_null(),
        "no implicit header — the first PHYSICAL line was not followed by a blank. json={json}"
    );
    assert_eq!(
        doc["blocks"][0]["content"]["rows"]
            .as_array()
            .map(std::vec::Vec::len),
        Some(2),
        "[x,lit] and [c,d] are both body rows. json={json}"
    );
}
