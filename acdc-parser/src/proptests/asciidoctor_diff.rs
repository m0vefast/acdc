//! The acdc/asciidoctor AST differential.
//!
//! Each seed draws one document, both processors read it, and the two semantic
//! trees are compared: acdc's through its ASG export, Asciidoctor's through
//! `tests/asciidoctor_asg_adapter.rb`. A difference is shrunk to the smallest
//! document that still shows it and grouped by where it appears. A difference
//! named in `tests/asciidoctor_diff_divergences.json` is reported, not failed.
//!
//! ```text
//! ADOC_DIFF_SEED_FROM=1 ADOC_DIFF_SEEDS=500 ADOC_DIFF_REPORT=/tmp/adoc-diff.md \
//!   cargo test -p acdc-parser --features asg asciidoctor_diff -- --ignored --nocapture
//! ```
//!
//! Needs `asciidoctor` on PATH (`brew install asciidoctor`); without it the test fails.

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

use proptest::strategy::ValueTree;
use serde_json::{Map, Value};

use super::documents::{diff_document, env_u64, fixture_sources, seeded_tree};
use crate::{
    Options, SafeMode,
    asg::{Profile, export_document_with_profile},
    parse,
};

// ── The two readers ──────────────────────────────────────────────────────────

struct Asciidoctor {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

enum Oracle {
    Read(Value),
    /// Asciidoctor warned about the document or failed on it.
    NoReading,
}

impl Asciidoctor {
    fn spawn() -> Self {
        let adapter = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/asciidoctor_asg_adapter.rb");
        let mut child = adapter_command(&adapter)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("could not start the asciidoctor adapter (is asciidoctor installed?): {e}"));
        let stdin = child.stdin.take().expect("adapter stdin");
        let stdout = BufReader::new(child.stdout.take().expect("adapter stdout"));
        Self { child, stdin, stdout }
    }

    fn read(&mut self, src: &str) -> Oracle {
        let request = serde_json::json!({ "src": src }).to_string();
        writeln!(self.stdin, "{request}").expect("write to the asciidoctor adapter");
        let mut line = String::new();
        self.stdout.read_line(&mut line).expect("read from the asciidoctor adapter");
        let reply: Value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("adapter reply {line:?}: {e}"));
        let warned = reply["warnings"].as_array().is_some_and(|w| !w.is_empty());
        if reply.get("error").is_some() || warned {
            return Oracle::NoReading;
        }
        Oracle::Read(reply["asg"].clone())
    }
}

impl Drop for Asciidoctor {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

/// The Ruby that has the `asciidoctor` gem: the one the `asciidoctor` executable
/// on PATH runs under.
/// The Ruby that runs the adapter: `ADOC_DIFF_RUBY`, else `ruby` on PATH. It must
/// `require 'asciidoctor'` with the environment it inherits (`GEM_HOME` / `GEM_PATH`).
fn adapter_command(adapter: &Path) -> Command {
    let mut command = Command::new(std::env::var_os("ADOC_DIFF_RUBY").unwrap_or_else(|| "ruby".into()));
    command.arg(adapter);
    command
}

enum AcdcRead {
    Read(Value),
    ParseError(String),
    Unexported(String),
}

fn acdc(src: &str) -> AcdcRead {
    let options = Options::builder()
        .with_safe_mode(SafeMode::Unsafe)
        .build()
        .expect("options without caller attributes have nothing to reject");
    let parsed = match parse(src, &options) {
        Ok(parsed) => parsed,
        Err(e) => return AcdcRead::ParseError(e.to_string()),
    };
    match export_document_with_profile(parsed.document(), Profile::Acdc) {
        Ok(asg) => AcdcRead::Read(asg),
        Err(e) => AcdcRead::Unexported(e.to_string()),
    }
}

// ── Canonical form ───────────────────────────────────────────────────────────

const CHILD_LISTS: [&str; 5] = ["blocks", "items", "inlines", "principal", "title"];

/// The fields both trees are compared on: what a node is and what it holds.
/// Locations, metadata and presentation are left out.
fn canonical(node: &Value) -> Value {
    match node {
        Value::Array(items) => Value::Array(canonical_inlines_or_blocks(items)),
        Value::Object(map) => {
            let name = map.get("name").and_then(Value::as_str).unwrap_or_default();
            let mut out = Map::new();
            out.insert("name".into(), name.into());
            let keep: &[&str] = match name {
                "list" | "break" | "admonition" => &["variant"],
                "span" => &["variant"],
                "ref" => &["variant", "target"],
                "section" | "heading" => &["level", "id"],
                "image" | "audio" | "video" | "icon" => &["target"],
                "anchor" => &["id"],
                "text" | "charref" | "raw" => &["value"],
                _ => &[],
            };
            // Asciidoctor draws these as markup that carries only what they are.
            let leaf = matches!(name, "footnote" | "keyboard" | "button" | "menu" | "stem" | "icon" | "image" | "callout");
            // Asciidoctor's `doctitle` is the whole line, subtitle included.
            if name == "document"
                && let Some(header) = map.get("header")
                && let Some(Value::Array(title)) = header.get("title")
            {
                let mut whole = title.clone();
                if let Some(Value::Array(subtitle)) = header.get("subtitle") {
                    whole.push(serde_json::json!({ "name": "text", "value": ": " }));
                    whole.extend(subtitle.iter().cloned());
                }
                out.insert("title".into(), canonical(&Value::Array(whole)));
            }
            for key in keep {
                if let Some(v) = map.get(*key) {
                    out.insert((*key).into(), v.clone());
                }
            }
            // Asciidoctor's HTML draws an interdocument xref as a plain link to
            // the path with its output suffix (`outfilesuffix`).
            if name == "ref"
                && map.get("variant").and_then(Value::as_str) == Some("xref")
                && let Some(target) = map.get("target").and_then(Value::as_str)
            {
                let (path, fragment) = target.split_once('#').map_or((target, None), |(p, f)| (p, Some(f)));
                if path.ends_with(".adoc") || (fragment.is_some() && !path.is_empty()) {
                    let stem = path.strip_suffix(".adoc").unwrap_or(path);
                    let file = if Path::new(stem).extension().is_some() { stem.to_string() } else { format!("{stem}.html") };
                    let href = fragment.map_or_else(|| file.clone(), |f| format!("{file}#{f}"));
                    out.insert("target".into(), href.into());
                    out.insert("variant".into(), "link".into());
                }
            }
            if let Some(id) = map.get("id").filter(|_| !leaf && name != "section" && name != "anchor") {
                out.insert("id".into(), id.clone());
            }
            for key in CHILD_LISTS {
                if leaf && key != "title" {
                    continue;
                }
                if let Some(v) = map.get(key) {
                    out.insert(key.into(), canonical(v));
                }
            }
            if let Some(Value::Array(terms)) = map.get("terms") {
                out.insert("terms".into(), Value::Array(terms.iter().map(canonical).collect()));
            }
            if name == "table" {
                for key in ["header", "rows", "footer"] {
                    let rows = map.get(key).map(table_rows).unwrap_or_default();
                    if !rows.is_empty() {
                        out.insert(key.into(), Value::Array(rows));
                    }
                }
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// Table rows as lists of cells, each its spans and blocks. acdc writes a header
/// or footer as one `{"cells": …}` row and omits a span of 1; the adapter writes
/// rows of cells with every span.
fn table_rows(value: &Value) -> Vec<Value> {
    let row = |row: &Value| -> Value {
        let cells = row.get("cells").unwrap_or(row);
        Value::Array(
            cells
                .as_array()
                .into_iter()
                .flatten()
                .map(|cell| {
                    let span = |key: &str| cell.get(key).and_then(Value::as_u64).unwrap_or(1);
                    serde_json::json!({
                        "colspan": span("colspan"),
                        "rowspan": span("rowspan"),
                        "blocks": canonical(cell.get("blocks").unwrap_or(&Value::Array(Vec::new()))),
                    })
                })
                .collect(),
        )
    };
    match value {
        Value::Object(_) => vec![row(value)],
        Value::Array(rows) => rows.iter().map(row).collect(),
        _ => Vec::new(),
    }
}

/// Character references decoded and adjacent text joined, so `a&#8212;b` and
/// `a—b` read the same.
fn canonical_inlines_or_blocks(items: &[Value]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for item in items {
        push_canonical(item, &mut out);
    }
    out
}

/// Body attribute entries are not blocks in Asciidoctor, a hidden index term
/// draws nothing and a visible one draws its term.
fn push_canonical(item: &Value, out: &mut Vec<Value>) {
    match item.get("name").and_then(Value::as_str) {
        Some("attribute") => return,
        Some("indexterm") => {
            if item.get("visibility").and_then(Value::as_str) == Some("visible") {
                for term in item.get("term").and_then(Value::as_array).into_iter().flatten() {
                    push_canonical(term, out);
                }
            }
            return;
        }
        // Asciidoctor's `:double` / `:single` quoted text is drawn as the quote
        // characters around its content.
        Some("span") if item.get("variant").and_then(Value::as_str) == Some("quotation") => {
            let quotes = item.get("quotes").and_then(Value::as_array);
            let quote = |i: usize| quotes.and_then(|q| q.get(i)).cloned().unwrap_or_default();
            push_canonical(&serde_json::json!({ "name": "text", "value": quote(0) }), out);
            for child in item.get("inlines").and_then(Value::as_array).into_iter().flatten() {
                push_canonical(child, out);
            }
            push_canonical(&serde_json::json!({ "name": "text", "value": quote(1) }), out);
            return;
        }
        _ => {}
    }
    {
        let item = canonical(item);
        // The zero-width space after an ellipsis is the HTML replacement's, and
        // the line end after a hard break is the break's.
        let after_break = out.last().and_then(|v| v.get("name")).and_then(Value::as_str) == Some("break");
        let as_text = match item.get("name").and_then(Value::as_str) {
            Some("text" | "charref" | "raw") => item.get("value").and_then(Value::as_str).map(|v| {
                let text = decode_refs(v).replace("…\u{200b}", "…");
                if after_break { text.strip_prefix('\n').map(str::to_string).unwrap_or(text) } else { text }
            }),
            _ => None,
        };
        match (as_text, out.last_mut()) {
            (Some(t), _) if t.is_empty() => {}
            (Some(t), Some(Value::Object(last))) if last.get("name").and_then(Value::as_str) == Some("text") => {
                let joined = format!("{}{t}", last["value"].as_str().unwrap_or_default());
                last.insert("value".into(), joined.into());
            }
            (Some(t), _) => out.push(serde_json::json!({ "name": "text", "value": t })),
            (None, _) => out.push(item),
        }
    }
}

fn decode_refs(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        let decoded = tail.find(';').and_then(|end| {
            let body = &tail[1..end];
            let ch = if let Some(num) = body.strip_prefix("#x") {
                u32::from_str_radix(num, 16).ok().and_then(char::from_u32)
            } else if let Some(num) = body.strip_prefix('#') {
                num.parse().ok().and_then(char::from_u32)
            } else {
                match body {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    _ => None,
                }
            };
            ch.map(|c| (c, end + 1))
        });
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = &tail[len..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

// ── Comparison ───────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct Difference {
    path: String,
    acdc: Value,
    asciidoctor: Value,
}

impl Difference {
    /// Where the difference is, with indices dropped, and which field differs.
    fn signature(&self) -> String {
        let shape: Vec<&str> = self.path.split('/').filter(|s| s.parse::<usize>().is_err()).collect();
        let kind = |v: &Value| match v {
            Value::Object(m) => m.get("name").and_then(Value::as_str).unwrap_or("object").to_string(),
            Value::Null => "missing".into(),
            Value::Array(a) => format!("{} items", a.len().min(2)),
            other => other.to_string().chars().take(24).collect(),
        };
        format!("{} : acdc {} / asciidoctor {}", shape.join("/"), kind(&self.acdc), kind(&self.asciidoctor))
    }
}

fn first_difference(path: &str, a: &Value, b: &Value) -> Option<Difference> {
    let differ = || Some(Difference { path: path.to_string(), acdc: a.clone(), asciidoctor: b.clone() });
    match (a, b) {
        (Value::Object(am), Value::Object(bm)) => {
            if am.get("name") != bm.get("name") {
                return differ();
            }
            // A reference with no text of its own takes it at render time from its
            // target; Asciidoctor has already drawn it.
            let auto_text = am.get("name").and_then(Value::as_str) == Some("ref")
                && am.get("inlines").is_none_or(|v| v.as_array().is_some_and(Vec::is_empty));
            let mut keys: Vec<&String> = am.keys().chain(bm.keys()).collect();
            keys.sort();
            keys.dedup();
            for key in keys {
                if auto_text && key == "inlines" {
                    continue;
                }
                let (av, bv) = (am.get(key).unwrap_or(&Value::Null), bm.get(key).unwrap_or(&Value::Null));
                if let Some(d) = first_difference(&format!("{path}/{key}"), av, bv) {
                    return Some(d);
                }
            }
            None
        }
        (Value::Array(aa), Value::Array(ba)) => {
            for (i, (av, bv)) in aa.iter().zip(ba).enumerate() {
                if let Some(d) = first_difference(&format!("{path}/{i}"), av, bv) {
                    return Some(d);
                }
            }
            if aa.len() == ba.len() {
                return None;
            }
            let i = aa.len().min(ba.len());
            Some(Difference {
                path: format!("{path}/{i}"),
                acdc: aa.get(i).cloned().unwrap_or(Value::Null),
                asciidoctor: ba.get(i).cloned().unwrap_or(Value::Null),
            })
        }
        _ if a == b => None,
        _ => differ(),
    }
}

// ── One document ─────────────────────────────────────────────────────────────

enum Verdict {
    Agree,
    /// Asciidoctor warned or failed: it gives no reading to compare with.
    NoOracle,
    Differ(Difference),
    AcdcFailed(String),
    AcdcUnexported(String),
}

fn judge(asciidoctor: &mut Asciidoctor, src: &str) -> Verdict {
    let oracle = match asciidoctor.read(src) {
        Oracle::Read(asg) => asg,
        Oracle::NoReading => return Verdict::NoOracle,
    };
    let ours = match acdc(src) {
        AcdcRead::Read(asg) => asg,
        AcdcRead::ParseError(e) => return Verdict::AcdcFailed(e),
        AcdcRead::Unexported(e) => return Verdict::AcdcUnexported(e),
    };
    match first_difference("", &canonical(&ours), &canonical(&oracle)) {
        None => Verdict::Agree,
        Some(d) => Verdict::Differ(d),
    }
}

fn verdict_signature(v: &Verdict) -> Option<String> {
    match v {
        Verdict::Agree | Verdict::NoOracle => None,
        Verdict::Differ(d) => Some(d.signature()),
        Verdict::AcdcFailed(e) => Some(format!("acdc failed: {}", e.chars().take(60).collect::<String>())),
        Verdict::AcdcUnexported(e) => Some(format!("acdc ASG export refused: {}", e.chars().take(60).collect::<String>())),
    }
}

// ── The survey ───────────────────────────────────────────────────────────────

struct Finding {
    seed: u64,
    count: usize,
    minimal: String,
    verdict_text: String,
}

fn ledger() -> BTreeMap<String, String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/asciidoctor_diff_divergences.json");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn describe(v: &Verdict) -> String {
    match v {
        Verdict::Differ(d) => format!(
            "at `{}`\n\nacdc:\n```json\n{}\n```\nasciidoctor:\n```json\n{}\n```",
            d.path,
            serde_json::to_string_pretty(&d.acdc).unwrap_or_default(),
            serde_json::to_string_pretty(&d.asciidoctor).unwrap_or_default()
        ),
        Verdict::AcdcFailed(e) | Verdict::AcdcUnexported(e) => e.clone(),
        Verdict::Agree | Verdict::NoOracle => String::new(),
    }
}

/// Shrink the seed's document while it keeps the same signature.
fn shrink(asciidoctor: &mut Asciidoctor, tree: &mut impl ValueTree<Value = String>, signature: &str) -> (String, String) {
    let mut best = (tree.current(), String::new());
    let mut steps = 0;
    let accept = |doc: &str, asciidoctor: &mut Asciidoctor| {
        let v = judge(asciidoctor, doc);
        (verdict_signature(&v).as_deref() == Some(signature)).then(|| describe(&v))
    };
    if let Some(text) = accept(&best.0, asciidoctor) {
        best.1 = text;
    }
    while steps < 2000 && tree.simplify() {
        steps += 1;
        loop {
            let doc = tree.current();
            if let Some(text) = accept(&doc, asciidoctor) {
                best = (doc, text);
                break;
            }
            steps += 1;
            if steps >= 2000 || !tree.complicate() {
                return best;
            }
        }
    }
    best
}

/// Write the seeded documents as `[{"seed": n, "src": "..."}]` for checks
/// outside this crate (Glyph's ASG → mdast preservation test).
#[test]
#[ignore = "writes a corpus file; run with --ignored"]
fn asciidoctor_diff_corpus() {
    let path = std::env::var("ADOC_DIFF_CORPUS").expect("ADOC_DIFF_CORPUS names the output file");
    let seed_from = env_u64("ADOC_DIFF_SEED_FROM", 1);
    let seeds = env_u64("ADOC_DIFF_SEEDS", 200);
    let strategy = diff_document();
    let corpus: Vec<Value> = (seed_from..seed_from + seeds)
        .map(|seed| serde_json::json!({ "seed": seed, "src": seeded_tree(&strategy, seed).current() }))
        .collect();
    let text = serde_json::to_string_pretty(&corpus).expect("serialize corpus");
    std::fs::write(&path, text + "\n").unwrap_or_else(|e| panic!("{path}: {e}"));
}

fn survey(seed_from: u64, seeds: u64) -> (BTreeMap<String, Finding>, usize, usize, usize) {
    let mut asciidoctor = Asciidoctor::spawn();
    let mut findings: BTreeMap<String, Finding> = BTreeMap::new();
    let (mut agreed, mut no_oracle, mut unexported) = (0, 0, 0);
    let strategy = diff_document();
    for seed in seed_from..seed_from + seeds {
        let mut tree = seeded_tree(&strategy, seed);
        let verdict = judge(&mut asciidoctor, &tree.current());
        unexported += usize::from(matches!(verdict, Verdict::AcdcUnexported(_)));
        let Some(signature) = verdict_signature(&verdict) else {
            if matches!(verdict, Verdict::NoOracle) { no_oracle += 1 } else { agreed += 1 }
            continue;
        };
        if let Some(f) = findings.get_mut(&signature) {
            f.count += 1;
            continue;
        }
        let (minimal, verdict_text) = shrink(&mut asciidoctor, &mut tree, &signature);
        findings.insert(signature, Finding { seed, count: 1, minimal, verdict_text });
    }
    (findings, agreed, no_oracle, unexported)
}

fn report(seed_from: u64, seeds: u64, findings: &BTreeMap<String, Finding>, counts: (usize, usize, usize), ledger: &BTreeMap<String, String>) -> String {
    let (agreed, no_oracle, unexported) = counts;
    let differ = usize::try_from(seeds).unwrap_or(usize::MAX) - agreed - no_oracle - unexported;
    let mut md = format!(
        "# acdc / asciidoctor AST differential\n\nseeds {seed_from}..{} · agree {agreed} · asciidoctor warned {no_oracle} · acdc export refused {unexported} · differ {differ} · differing signatures {}\n\n",
        seed_from + seeds,
        findings.len()
    );
    for (signature, f) in findings {
        let status = ledger.get(signature).map_or("**NEW**".to_string(), |why| format!("ledgered: {why}"));
        let _ = write!(
            md,
            "## {signature}\n\n{status} · {} seeds · first seed {}\n\n```adoc\n{}```\n\n{}\n\n",
            f.count, f.seed, f.minimal, f.verdict_text
        );
    }
    md
}

#[test]
#[ignore = "needs asciidoctor; run with --ignored"]
fn asciidoctor_diff_generated() {
    let seed_from = env_u64("ADOC_DIFF_SEED_FROM", 1);
    let seeds = env_u64("ADOC_DIFF_SEEDS", 200);
    let ledger = ledger();
    let (findings, agreed, no_oracle, unexported) = survey(seed_from, seeds);
    let md = report(seed_from, seeds, &findings, (agreed, no_oracle, unexported), &ledger);
    if let Ok(path) = std::env::var("ADOC_DIFF_REPORT") {
        std::fs::write(&path, &md).unwrap_or_else(|e| panic!("{path}: {e}"));
    }
    println!("{md}");
    let new: Vec<&String> = findings.keys().filter(|s| !ledger.contains_key(*s)).collect();
    assert!(new.is_empty(), "{} differences no ledger entry names:\n{}", new.len(), new.iter().map(|s| format!("  {s}")).collect::<Vec<_>>().join("\n"));
}

#[test]
#[ignore = "needs asciidoctor; run with --ignored"]
fn asciidoctor_diff_fixtures() {
    let mut asciidoctor = Asciidoctor::spawn();
    let ledger = ledger();
    let files = fixture_sources();
    let mut new = Vec::new();
    let mut md = String::from("# acdc / asciidoctor AST differential — fixtures\n\n");
    let (mut agreed, mut no_oracle, mut unexported) = (0, 0, 0);
    for (name, src) in &files {
        let verdict = judge(&mut asciidoctor, src);
        unexported += usize::from(matches!(verdict, Verdict::AcdcUnexported(_)));
        match verdict_signature(&verdict) {
            None if matches!(verdict, Verdict::NoOracle) => no_oracle += 1,
            None => agreed += 1,
            Some(signature) => {
                let status = if ledger.contains_key(&signature) { "ledgered" } else { "**NEW**" };
                let _ = write!(md, "## {name}\n\n{status} · {signature}\n\n{}\n\n", describe(&verdict));
                if !ledger.contains_key(&signature) {
                    new.push(format!("{name}: {signature}"));
                }
            }
        }
    }
    let _ = write!(
        md,
        "\nagree {agreed} · asciidoctor warned {no_oracle} · acdc export refused {unexported} · differ {}\n",
        files.len() - agreed - no_oracle - unexported
    );
    if let Ok(path) = std::env::var("ADOC_DIFF_REPORT") {
        std::fs::write(&path, &md).unwrap_or_else(|e| panic!("{path}: {e}"));
    }
    println!("{md}");
    assert!(new.is_empty(), "{} fixtures differ with no ledger entry:\n{}", new.len(), new.join("\n"));
}
