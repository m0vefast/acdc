//! A hostile `k*` duplication count must be rejected BEFORE anything expands it.
//!
//! `grid_reflow` materializes one cell copy per `duplication_count` and has no
//! bound of its own. Glyph's row model runs the table resource check on the way
//! OUT of `parse_rows_with_positions`, i.e. after that expansion, so a table
//! carrying an explicit `[cols=N]` used to allocate ~1e9 cells and get the
//! process OOM-killed (measured: SIGKILL) before the check could reject it.
//! Upstream validates the cell stream before grouping and rejects in
//! microseconds; this pins Glyph back to that behaviour.
//!
//! The assertion is on TIME as well as outcome: a rejection that still expanded
//! first would pass an outcome-only check while remaining a denial of service.

use std::time::{Duration, Instant};

fn assert_rejected_fast(src: &str) {
    let opts = acdc_parser::Options::default();
    let started = Instant::now();
    let result = acdc_parser::parse(src, &opts);
    let elapsed = started.elapsed();

    let Err(error) = result else {
        panic!("hostile duplication must be rejected, src={src:?}");
    };
    let message = format!("{error}");
    assert!(
        message.contains("duplication") || message.contains("column") || message.contains("row"),
        "rejection must name the bound that was exceeded, got {message:?}"
    );
    // Upstream answers in tens of microseconds. A whole second is far above any
    // plausible machine variance and far below the seconds-to-minutes an actual
    // expansion of 1e9 cells takes, so this separates "rejected early" from
    // "expanded, then rejected" without being flaky.
    assert!(
        elapsed < Duration::from_secs(1),
        "rejected only after {elapsed:?} — the guard must run BEFORE expansion, \
         not after. src={src:?}"
    );
}

#[test]
fn hostile_duplication_with_explicit_cols_is_rejected_before_expansion() {
    assert_rejected_fast("[cols=\"2*\"]\n|===\n999999999*| x\n|===\n");
    assert_rejected_fast("[cols=\"2*\"]\n|===\n1000000*| x\n|===\n");
}

#[test]
fn hostile_duplication_without_cols_is_rejected_before_expansion() {
    assert_rejected_fast("|===\n999999999*| x\n|===\n");
    assert_rejected_fast("|===\n1000000*| x\n|===\n");
}

#[test]
fn duplication_within_the_bound_still_parses() {
    let opts = acdc_parser::Options::default();
    acdc_parser::parse("|===\n50*| x\n50*| y\n|===\n", &opts).expect("50* is well under the bound");
    acdc_parser::parse("|===\n100*| x\n|===\n", &opts).expect("100* is exactly the bound");
}

/// Every hostile table dimension, not just the one that was found broken.
///
/// `duplication` was the vector that actually OOM-killed the process, but the
/// same shape of bug — a count the author controls driving an allocation that
/// happens before the bound is checked — applies to `colspan`, `rowspan` and
/// `cols=`. Each is exercised here so a future change to one cannot quietly
/// reopen what the fix to another closed. A malformed specifier that degrades
/// to ordinary cell content is a correct outcome; an unbounded ALLOCATION is
/// not, which is why the assertion is on time rather than on accept/reject.
///
/// How this fails when it fails: the elapsed-time assertion cannot fire, because
/// a parse that is busy materializing a billion cells never returns to be
/// measured. Verified by reverting the guard — the test HANGS until the harness
/// timeout kills it (or the OS does, with SIGKILL). That is still a red build,
/// and it is the only failure mode available: a Rust thread cannot be cancelled,
/// so no in-process watchdog could turn it into a clean assertion.
#[test]
fn every_hostile_table_dimension_is_bounded() {
    let opts = acdc_parser::Options::default();
    for src in [
        "[cols=\"2*\"]\n|===\n999999999*| x\n|===\n",
        "|===\n999999999*| x\n|===\n",
        "[cols=\"2*\"]\n|===\n999999999+| x\n|===\n",
        "|===\n999999999+| x\n|===\n",
        "[cols=\"2*\"]\n|===\n.999999999+| x\n|===\n",
        "|===\n.999999999+| x\n|===\n",
        "|===\n999999+.999999+| x\n|===\n",
        "[cols=\"999999999*\"]\n|===\n| x\n|===\n",
        "[cols=999999999]\n|===\n| x\n|===\n",
        "[cols=\"2*\"]\n|===\n1000*| x\n1000*| y\n1000*| z\n|===\n",
        "[cols=\"4*\"]\n|===\n99999*2+| x\n|===\n",
    ] {
        let started = Instant::now();
        let result = acdc_parser::parse(src, &opts);
        let elapsed = started.elapsed();
        // Whatever the verdict, it must be reached without materializing the
        // requested count. Measured on a healthy tree: all of these answer in
        // under 3 ms; the broken build was killed by the OS instead.
        assert!(
            elapsed < Duration::from_secs(2),
            "{src:?} took {elapsed:?} — a bound is being checked AFTER the \
             allocation it is supposed to prevent"
        );
        // An accepted malformed specifier must degrade to ordinary content, not
        // to a giant table.
        if let Ok(parsed) = result {
            let json = serde_json::to_string(parsed.document()).expect("serialize");
            assert!(
                json.len() < 64 * 1024,
                "{src:?} was accepted and produced {} bytes of document — a \
                 malformed specifier must degrade to cell content",
                json.len()
            );
        }
    }
}
