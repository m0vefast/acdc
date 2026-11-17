//! Seeded AsciiDoc documents shared by the checks that read the same corpus:
//! the Asciidoctor AST differential and the source-location consistency check.

use std::fmt::Write as _;

use proptest::{
    prelude::*,
    test_runner::{Config, RngAlgorithm, TestRng, TestRunner},
};

/// Mostly letters; one character in six is AsciiDoc markup or non-ASCII.
fn text() -> impl Strategy<Value = String> {
    let ch = prop_oneof![
        5 => prop::sample::select(vec!['a', 'b', 'e', 'n', 'o', 's', 't', 'x', ' ']),
        1 => prop::sample::select(vec![
            '*', '_', '`', '#', '^', '~', '+', '[', ']', '<', '>', '{', '}', '|', ':', '=', '-',
            '.', '/', '\\', '\'', '"', 'é', '中',
        ]),
    ];
    prop::collection::vec(ch, 1..8).prop_map(|cs| {
        let s: String = cs.into_iter().collect();
        let s = s.trim().to_string();
        if s.is_empty() { "x".to_string() } else { s }
    })
}

fn word() -> impl Strategy<Value = &'static str> {
    prop::sample::select(vec!["alpha", "beta", "Docs", "site", "plan", "Pricing", "Roadmap"])
}

fn inline() -> impl Strategy<Value = String> {
    prop_oneof![
        6 => text(),
        1 => word().prop_map(|w| format!("*{w}*")),
        1 => word().prop_map(|w| format!("_{w}_")),
        1 => word().prop_map(|w| format!("`{w}`")),
        1 => word().prop_map(|w| format!("#{w}#")),
        1 => word().prop_map(|w| format!("**{w}**")),
        1 => word().prop_map(|w| format!("^{w}^")),
        1 => word().prop_map(|w| format!("~{w}~")),
        1 => word().prop_map(|w| format!("https://a.com[{w}]")),
        1 => prop::sample::select(vec![
            "https://b.org/x", "link:notes/plan.adoc[plan]", "<<scope,the scope>>", "<<scope>>",
            "xref:scope[]", "[[a1]]", "footnote:[note]", "a -- b", "\"`q`\"",
        ]).prop_map(str::to_string),
    ]
}

fn inline_line() -> impl Strategy<Value = String> {
    prop::collection::vec(inline(), 1..5).prop_map(|parts| parts.join(" "))
}

fn paragraph() -> impl Strategy<Value = String> {
    prop::collection::vec(inline_line(), 1..3).prop_map(|lines| lines.join("\n"))
}

fn list(marker: &'static str) -> impl Strategy<Value = String> {
    prop::collection::vec((inline_line(), prop::bool::weighted(0.2), inline_line()), 1..4).prop_map(
        move |items| {
            let nested = marker.repeat(2);
            items
                .into_iter()
                .map(|(principal, nest, child)| {
                    let mut s = format!("{marker} {principal}");
                    if nest {
                        let _ = write!(s, "\n{nested} {child}");
                    }
                    s
                })
                .collect::<Vec<_>>()
                .join("\n")
        },
    )
}

/// Items with any mix of markers, blank lines and continuations: Asciidoctor
/// nests by marker identity (`is_sibling_list_item?`), so each marker change,
/// return to an earlier marker, or gap is a nesting decision.
fn list_shape() -> impl Strategy<Value = String> {
    let marker = prop::sample::select(vec![
        "*", "**", "***", "-", ".", "..", "...", "1.", "2.", "a.", "b.", "A.", "i)", "ii)", "I)",
    ]);
    let gap = prop::sample::select(vec!["\n", "\n", "\n", "\n\n", "\n+\n"]);
    prop::collection::vec((marker, word(), gap), 1..7).prop_map(|items| {
        let mut s = String::new();
        for (i, (marker, word, gap)) in items.iter().enumerate() {
            if i > 0 {
                s.push_str(if *gap == "\n+\n" { "\n+\npara\n" } else { gap });
            }
            let _ = write!(s, "{marker} {word}");
        }
        s
    })
}

fn leaf_block() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => paragraph(),
        1 => list("*"),
        1 => list("."),
        1 => list_shape(),
        1 => prop::collection::vec((word(), inline_line()), 1..3).prop_map(|items| {
            items.into_iter().map(|(t, d)| format!("{t}:: {d}")).collect::<Vec<_>>().join("\n")
        }),
        1 => text().prop_map(|t| format!("----\n{t}\n----")),
        1 => text().prop_map(|t| format!("....\n{t}\n....")),
        1 => (prop::sample::select(vec!["NOTE", "TIP", "WARNING"]), inline_line())
            .prop_map(|(k, t)| format!("{k}: {t}")),
        1 => prop::collection::vec((inline_line(), inline_line()), 1..3).prop_map(|rows| {
            let body: Vec<String> = rows.into_iter().map(|(a, b)| format!("| {a} | {b}")).collect();
            format!("|===\n{}\n|===", body.join("\n"))
        }),
        1 => Just("'''".to_string()),
    ]
}

/// A block, optionally preceded by an anchor or a block title, possibly inside a
/// compound block.
fn block() -> impl Strategy<Value = String> {
    let leaf = leaf_block().boxed();
    let compound = leaf.prop_recursive(2, 8, 3, |inner| {
        (
            prop::sample::select(vec![("====", ""), ("****", ""), ("____", ""), ("--", ""), ("====", "[NOTE]\n")]),
            prop::collection::vec(inner, 1..3),
        )
            .prop_map(|((delim, style), blocks)| format!("{style}{delim}\n{}\n{delim}", blocks.join("\n\n")))
    });
    (
        prop::option::weighted(0.15, prop::sample::select(vec!["[[blk]]", "[#blk]"])),
        prop::option::weighted(0.15, word()),
        compound,
    )
        .prop_map(|(anchor, title, body)| {
            let mut s = String::new();
            if let Some(a) = anchor {
                s.push_str(a);
                s.push('\n');
            }
            if let Some(t) = title {
                let _ = writeln!(s, ".{t}");
            }
            s.push_str(&body);
            s
        })
}

fn section() -> impl Strategy<Value = String> {
    (
        prop::sample::select(vec!["==", "===", "=="]),
        prop::option::weighted(0.2, Just("[[scope]]")),
        inline_line(),
        prop::collection::vec(block(), 0..3),
    )
        .prop_map(|(level, anchor, title, blocks)| {
            let mut s = String::new();
            if let Some(a) = anchor {
                let _ = writeln!(s, "{a}");
            }
            let _ = write!(s, "{level} {title}");
            for b in blocks {
                let _ = write!(s, "\n\n{b}");
            }
            s
        })
}

pub(super) fn diff_document() -> impl Strategy<Value = String> {
    (
        prop::option::weighted(0.6, inline_line()),
        prop::collection::vec(block(), 0..3),
        prop::collection::vec(section(), 0..3),
    )
        .prop_map(|(title, preamble, sections)| {
            let mut parts = Vec::new();
            if let Some(t) = title {
                parts.push(format!("= {t}"));
            }
            parts.extend(preamble);
            parts.extend(sections);
            let mut doc = parts.join("\n\n");
            doc.push('\n');
            doc
        })
}

/// The document tree a seed draws: the same for every consumer of the corpus.
pub(super) fn seeded_tree<S: Strategy>(strategy: &S, seed: u64) -> S::Tree {
    let mut bytes = [0u8; 32];
    bytes[..8].copy_from_slice(&seed.to_le_bytes());
    let mut runner = TestRunner::new_with_rng(Config::default(), TestRng::from_seed(RngAlgorithm::ChaCha, &bytes));
    strategy.new_tree(&mut runner).expect("generate a document")
}

pub(super) fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().map_or(default, |v| v.parse().unwrap_or_else(|_| panic!("{name}={v} is not a number")))
}

/// Every parser fixture source, in name order.
pub(super) fn fixture_sources() -> Vec<(String, String)> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let mut files: Vec<std::path::PathBuf> = ["tests", "asg"]
        .iter()
        .flat_map(|dir| std::fs::read_dir(root.join(dir)).into_iter().flatten().flatten().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "adoc"))
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|p| {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            (name, std::fs::read_to_string(&p).unwrap_or_default())
        })
        .collect()
}
