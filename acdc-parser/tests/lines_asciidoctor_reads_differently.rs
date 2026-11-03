//! Lines asciidoctor reads one way and acdc read another, found by Glyph's AsciiDoc edit
//! simulator: a `////` block comment opening the body (its delimiter was taken for a
//! line comment, so the comment closed on the next `////` and swallowed what followed),
//! a `<<<` page break not followed by a blank line, and a section marker with no title
//! text (the whole document failed to parse). Every expectation is asciidoctor 2.0.26's.

use acdc_parser::{Options, SafeMode, parse};
use serde_json::Value;

/// The header's title and authors, then the document's top-level blocks: each block's
/// name (and variant), a paragraph's text, a list's item count.
fn shape(src: &str) -> Result<Vec<String>, String> {
    let options = Options::builder().with_safe_mode(SafeMode::Unsafe).build();
    let parsed = parse(src, &options).map_err(|e| e.to_string())?;
    let json = serde_json::to_value(parsed.document()).map_err(|e| e.to_string())?;
    let text = |inlines: &Value| -> String {
        inlines.as_array().map_or(String::new(), |inlines| {
            inlines.iter().filter_map(|i| i["value"].as_str()).collect()
        })
    };
    let title = json["header"]["title"]
        .as_array()
        .map(|_| format!("title:{}", text(&json["header"]["title"])));
    let authors = json["header"]["authors"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|a| {
            let name = |part: &str| a[part].as_str().unwrap_or("?").to_string();
            format!("author:{} {}", name("firstname"), name("lastname"))
        });
    let blocks = json["blocks"].as_array().into_iter().flatten().map(|b| {
        let name = b["name"].as_str().unwrap_or("?");
        let items = b["items"].as_array().map_or(0, Vec::len);
        match (name, b["variant"].as_str()) {
            ("paragraph", _) => format!("paragraph:{}", text(&b["inlines"])),
            ("list", Some(variant)) => format!("list/{variant}:{items}"),
            (_, Some(variant)) => format!("{name}/{variant}"),
            _ => name.to_string(),
        }
    });
    Ok(title.into_iter().chain(authors).chain(blocks).collect())
}

/// Every block, nested ones indented under the block holding them: `name/variant`.
fn tree(src: &str) -> Result<Vec<String>, String> {
    fn walk(blocks: &Value, depth: usize, out: &mut Vec<String>) {
        for b in blocks.as_array().into_iter().flatten() {
            let name = b["name"].as_str().unwrap_or("?");
            let variant = b["variant"].as_str().map_or(String::new(), |v| format!("/{v}"));
            out.push(format!("{}{name}{variant}", "  ".repeat(depth)));
            walk(&b["blocks"], depth + 1, out);
        }
    }
    let options = Options::builder().with_safe_mode(SafeMode::Unsafe).build();
    let parsed = parse(src, &options).map_err(|e| e.to_string())?;
    let json = serde_json::to_value(parsed.document()).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    walk(&json["blocks"], 0, &mut out);
    Ok(out)
}

/// Every block, nested ones indented under what holds them: a paragraph with its text,
/// a list item with its principal text, a table cell.
fn outline(src: &str) -> Result<Vec<String>, String> {
    fn text(inlines: &Value) -> String {
        inlines.as_array().map_or(String::new(), |inlines| {
            inlines.iter().filter_map(|i| i["value"].as_str()).collect()
        })
    }
    fn walk(blocks: &Value, depth: usize, out: &mut Vec<String>) {
        for b in blocks.as_array().into_iter().flatten() {
            let pad = "  ".repeat(depth);
            let name = b["name"].as_str().unwrap_or("?");
            match name {
                "paragraph" => out.push(format!("{pad}paragraph:{}", text(&b["inlines"]))),
                _ => out.push(format!("{pad}{name}")),
            }
            for item in b["items"].as_array().into_iter().flatten() {
                let principal = if item["principal"].is_null() { &item["principal_text"] } else { &item["principal"] };
                out.push(format!("{pad}  item:{}", text(principal)));
                walk(&item["blocks"], depth + 2, out);
                walk(&item["description"], depth + 2, out);
            }
            for row in b["content"]["rows"].as_array().into_iter().flatten() {
                for cell in row["columns"].as_array().into_iter().flatten() {
                    out.push(format!("{pad}  cell"));
                    walk(&cell["content"], depth + 2, out);
                }
            }
            walk(&b["blocks"], depth + 1, out);
        }
    }
    let options = Options::builder().with_safe_mode(SafeMode::Unsafe).build();
    let parsed = parse(src, &options).map_err(|e| e.to_string())?;
    let json = serde_json::to_value(parsed.document()).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    walk(&json["blocks"], 0, &mut out);
    Ok(out)
}

/// A paragraph runs to a blank line, a delimiter, a block attribute line or an anchor
/// line; nothing else ends it (asciidoctor `Parser.read_paragraph_lines`, no list break
/// outside a list item's own text). A block attribute line is one line: a `[` that does
/// not close on its line is text. In an item's continuation the paragraph ends where the
/// item does: at the next item of that list or of a list holding it.
#[test]
fn a_paragraph_ends_where_asciidoctor_ends_it() {
    let rows: &[(&str, &[&str])] = &[
        ("text\n[[id]]\nmore", &["paragraph:text", "paragraph:more"]),
        ("text\n[#x]\nmore", &["paragraph:text", "paragraph:more"]),
        ("text\n. item", &["paragraph:text\n. item"]),
        ("text\nterm:: def", &["paragraph:text\nterm:: def"]),
        ("== A\n\npara\n== B\nmore", &["section", "  paragraph:para\n== B\nmore"]),
        ("t:: a\n== T", &["dlist", "  item:a\n== T"]),
        ("t:: a\n[[id]]\nmore", &["dlist", "  item:a", "paragraph:more"]),
        ("t:: a\n[[id]]\n== T", &["dlist", "  item:a", "section"]),
        ("text\n[[id,a\nb]]\nmore", &["paragraph:text\n[[id,a\nb]]\nmore"]),
        ("[.x\ny]\npara", &["paragraph:[.x\ny]\npara"]),
        ("[a=b\nc]\npara", &["paragraph:[a=b\nc]\npara"]),
        ("[%opt\nx]\npara", &["paragraph:[%opt\nx]\npara"]),
        ("[\"a\nb\"]\npara", &["paragraph:[\"a\nb\"]\npara"]),
        ("[x,'a\nb']\npara", &["paragraph:[x,'a\nb']\npara"]),
        ("image::a.png[alt\ntext]", &["paragraph:image::a.png[alt\ntext]"]),
        ("toc::[a\nb]", &["paragraph:toc::[a\nb]"]),
        ("|===\n|text\n* item\n|===", &["table", "  cell", "    paragraph:text\n* item"]),
        // A cell without the `a` style holds text paragraphs only: no block metadata,
        // no block macro, no break (asciidoctor `Table::Cell#content`).
        ("|===\n| .^ Middle\n|===", &["table", "  cell", "    paragraph:.^ Middle"]),
        ("|===\n|.T\ntext\n|===", &["table", "  cell", "    paragraph:.T\ntext"]),
        ("|===\n|[.r]\ntext\n|===", &["table", "  cell", "    paragraph:[.r]\ntext"]),
        ("|===\n|text\n\n.T\n|===", &["table", "  cell", "    paragraph:text", "    paragraph:.T"]),
        ("|===\n|image::a.png[]\n|===", &["table", "  cell", "    paragraph:image::a.png[]"]),
        ("|===\n|video::a.mp4[]\n|===", &["table", "  cell", "    paragraph:video::a.mp4[]"]),
        ("|===\n|'''\n|===", &["table", "  cell", "    paragraph:'''"]),
        ("* a\n+\npara\n== T", &["list", "  item:a", "    paragraph:para\n== T"]),
        ("* a\n+\n== T", &["list", "  item:a", "    paragraph:== T"]),
        ("* a\n+\n[discrete]\n== T", &["list", "  item:a", "    heading"]),
        // A first positional attribute holding a space is the block style as written
        // (asciidoctor `Parser.parse_style_attribute`), not an id or a role.
        ("text\n[#a b]\nmore", &["paragraph:text", "paragraph:more"]),
        ("[#a b]\n== S", &["section"]),
        ("[x#a b]\npara", &["paragraph:para"]),
        ("[.a b,x=y]\npara", &["paragraph:para"]),
        ("* a\n+\n--\npara\n* b\n--", &["list", "  item:a", "    open", "      paragraph:para\n* b"]),
    ];
    let wrong: Vec<String> = rows
        .iter()
        .filter_map(|(src, want)| {
            let got = outline(src);
            let same = got
                .as_ref()
                .is_ok_and(|g| g.iter().map(String::as_str).eq(want.iter().copied()));
            (!same).then(|| format!("{src:?}: got {got:?}, asciidoctor reads {want:?}"))
        })
        .collect();
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// A list item holds the lines asciidoctor's `Parser.read_lines_for_list_item` gives it:
/// its text runs on past a section title, a block title, a block macro or an attribute
/// entry; a block attached with `+` runs to the next item of that list or of a list holding it
/// (another list's marker is the block's text), and a block attribute line after it
/// starts another block in the same item.
#[test]
fn a_continuation_ends_where_the_item_ends() {
    let rows: &[(&str, &[&str])] = &[
        ("* a\n+\npara\n* b", &["list", "  item:a", "    paragraph:para", "  item:b"]),
        ("* a\n+\npara\n** b", &["list", "  item:a", "    paragraph:para\n** b"]),
        ("* a\n+\npara\n- b", &["list", "  item:a", "    paragraph:para\n- b"]),
        (". a\n+\npara\n. b", &["list", "  item:a", "    paragraph:para", "  item:b"]),
        (". a\n+\npara\n* b", &["list", "  item:a", "    paragraph:para\n* b"]),
        ("t:: a\n+\npara\nu:: b", &["dlist", "  item:a", "    paragraph:para", "  item:b"]),
        ("* a\n+\npara\n[#x]\nmore", &["list", "  item:a", "    paragraph:para", "    paragraph:more"]),
        ("* a\n+\n[discrete]\n== T\nmore", &["list", "  item:a", "    heading", "    paragraph:more"]),
        ("* a\n== T", &["list", "  item:a\n== T"]),
        ("t:: a\n+\npara\n* b", &["dlist", "  item:a", "    paragraph:para\n* b"]),
        (". a\n+\npara\n.. b", &["list", "  item:a", "    paragraph:para\n.. b"]),
        ("* a\n+\npara\n----\ncode\n----", &["list", "  item:a", "    paragraph:para", "listing"]),
        ("* a\n+\n----\nx\n----\nmore", &["list", "  item:a", "    listing", "    paragraph:more"]),
        ("* a\n+\n----\nx\n----\n** b", &["list", "  item:a", "    listing", "    list", "      item:b"]),
        ("* a\n+\n[#x]\n----\nx\n----", &["list", "  item:a", "    listing"]),
        (
            ". a\n.. b\n+\npara\n... c",
            &["list", "  item:a", "    list", "      item:b", "        paragraph:para\n... c"],
        ),
        (". a\n.. b\n+\npara\n. d", &["list", "  item:a", "    list", "      item:b", "        paragraph:para", "  item:d"]),
        (
            "* a\n . n\n+\npara\n* b",
            &["list", "  item:a", "    list", "      item:n", "        paragraph:para", "  item:b"],
        ),
        (
            "* a\n . n\n+\npara\n . m",
            &["list", "  item:a", "    list", "      item:n", "        paragraph:para", "      item:m"],
        ),
        ("* a\n.T\nb", &["list", "  item:a\n.T\nb"]),
        ("* a\ntoc::[]", &["list", "  item:a\ntoc::[]"]),
        ("* a\n:x: y\nb", &["list", "  item:a\n:x: y\nb"]),
        (
            "* a\n** n\n+\npara\n* b",
            &["list", "  item:a", "    list", "      item:n", "        paragraph:para", "  item:b"],
        ),
        (
            "* a\n** n\n+\npara\n** m",
            &["list", "  item:a", "    list", "      item:n", "        paragraph:para", "      item:m"],
        ),
    ];
    let wrong: Vec<String> = rows
        .iter()
        .filter_map(|(src, want)| {
            let got = outline(src);
            let same = got
                .as_ref()
                .is_ok_and(|g| g.iter().map(String::as_str).eq(want.iter().copied()));
            (!same).then(|| format!("{src:?}: got {got:?}, asciidoctor reads {want:?}"))
        })
        .collect();
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// A delimited block left open holds what follows it to the end of its parent
/// (asciidoctor: "unterminated example block"); acdc kept it empty once a table was
/// among what followed (Glyph AsciiDoc simulator seed 84: a table inserted after the
/// block vanished from the page).
#[test]
fn an_unterminated_block_holds_what_follows() {
    let rows: &[(&str, &[&str])] = &[
        ("====\n'''\n", &["example", "  break/thematic"]),
        ("====\n\n|===\n| a\n|===\n", &["example", "  table"]),
        ("para\n\n====\n\n'''\n\n|===\n| a | b\n|===\n", &["paragraph", "example", "  break/thematic", "  table"]),
    ];
    let wrong: Vec<String> = rows
        .iter()
        .filter_map(|(src, want)| {
            let got = tree(src);
            let same = got
                .as_ref()
                .is_ok_and(|g| g.iter().map(String::as_str).eq(want.iter().copied()));
            (!same).then(|| format!("{src:?}: got {got:?}, asciidoctor reads {want:?}"))
        })
        .collect();
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// A delimited block's content ends with blank lines when a blank line comes before its
/// closing delimiter, or when it runs unclosed to a blank line at the end of the input:
/// the blocks before them are its content (asciidoctor drops trailing blank lines).
#[test]
fn a_blank_line_ending_a_delimited_block_keeps_its_content() {
    let rows: &[(&str, &[&str])] = &[
        ("====\nx\n\n====\n", &["example", "  paragraph:x"]),
        ("====\nx\n\n", &["example", "  paragraph:x"]),
        ("====\nx\n\n\n====\n", &["example", "  paragraph:x"]),
        ("****\nx\n\n****\n", &["sidebar", "  paragraph:x"]),
        ("____\nx\n\n____\n", &["quote", "  paragraph:x"]),
        ("--\nx\n\n--\n", &["open", "  paragraph:x"]),
        ("* a\n+\n--\nx\n\n--\n", &["list", "  item:a", "    open", "      paragraph:x"]),
        ("====\nx\n\n|===\n| a\n|===\n\n", &["example", "  paragraph:x", "  table", "    cell", "      paragraph:a"]),
        ("====\nx\n  \n====\n", &["example", "  paragraph:x"]),
        ("|===\na|\nx\n\n|===\n", &["table", "  cell", "    paragraph:x"]),
    ];
    let wrong: Vec<String> = rows
        .iter()
        .filter_map(|(src, want)| {
            let got = outline(src);
            let same = got
                .as_ref()
                .is_ok_and(|g| g.iter().map(String::as_str).eq(want.iter().copied()));
            (!same).then(|| format!("{src:?}: got {got:?}, asciidoctor reads {want:?}"))
        })
        .collect();
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// Each top-level block as `name#id.role(title):text`.
fn blocks_with_metadata(src: &str) -> Result<Vec<String>, String> {
    let options = Options::builder().with_safe_mode(SafeMode::Unsafe).build();
    let parsed = parse(src, &options).map_err(|e| e.to_string())?;
    let json = serde_json::to_value(parsed.document()).map_err(|e| e.to_string())?;
    let text = |inlines: &Value| -> String {
        inlines.as_array().map_or(String::new(), |inlines| {
            inlines.iter().filter_map(|i| i["value"].as_str()).collect()
        })
    };
    let blocks = json["blocks"].as_array().into_iter().flatten().map(|b| {
        let meta = &b["metadata"];
        let id = meta["anchors"].as_array().and_then(|a| a.last()).and_then(|a| a["id"].as_str());
        let role = meta["roles"].as_array().and_then(|r| r.first()).and_then(Value::as_str);
        let title = text(&b["title"]);
        format!(
            "{}{}{}{}:{}",
            b["name"].as_str().unwrap_or("?"),
            id.map_or(String::new(), |id| format!("#{id}")),
            role.map_or(String::new(), |r| format!(".{r}")),
            if title.is_empty() { String::new() } else { format!("({title})") },
            text(&b["inlines"]),
        )
    });
    Ok(blocks.collect())
}

/// Block metadata (a title, an anchor, a role) goes to the next block over the comment
/// lines, attribute entries and blank lines between them; acdc failed the whole
/// document instead.
#[test]
fn metadata_reaches_the_next_block() {
    let rows: &[(&str, &[&str])] = &[
        ("[.lead]\n// plan\n\npara", &["paragraph.lead:para"]),
        ("[#a2]\n// alpha\n\npara", &["paragraph#a2:para"]),
        (".Title\n[#a3]\n// Docs\n\npara", &["paragraph#a3(Title):para"]),
        (".[source,js]\n\n\nA title", &["paragraph([source,js]):A title"]),
        ("[#a2]\n:shown: yes\n\npara", &["paragraph#a2:para"]),
        (".Title\n\npara", &["paragraph(Title):para"]),
        ("[.lead]\n\npara", &["paragraph.lead:para"]),
        ("[#a2]\n\npara", &["paragraph#a2:para"]),
        ("[#a2]\n// alpha\npara", &["paragraph#a2:para"]),
        ("[.lead]\n// plan\npara", &["paragraph.lead:para"]),
        ("[.lead]\n\n[#x]\npara", &["paragraph#x.lead:para"]),
        ("[.lead]\n\n\npara", &["paragraph.lead:para"]),
        (".T\n\n\npara", &["paragraph(T):para"]),
        ("[#a2]\n// alpha\n\n[.lead]\n--\nx\n--", &["open#a2.lead:"]),
        ("[.lead]\n// plan\n\n[quote,Ada]\n____\nq\n____", &["quote.lead:"]),
        (".Title\n[#a3]\n// Docs\n\n[#a2]\n.. xt", &["list#a2(Title):"]),
        ("[#a b]\npara", &["paragraph:para"]),
        ("[#a b.r]\npara", &["paragraph:para"]),
        // Metadata with no block after it is dropped (Glyph AsciiDoc simulator seed 5:
        // the whole document failed to parse).
        ("[#a2]\n////\nDocs\n////", &[]),
        ("p\n\n[#a2]\n////\nDocs\n////\n\nq", &["paragraph:p", "paragraph#a2:q"]),
        (".T\n////\nx\n////", &[]),
        ("para\n\n[#a2]", &["paragraph:para"]),
        ("[#a2]\n// c", &[]),
        ("[#a2]\n// c\n", &[]),
        ("[.r]\n// c", &[]),
        ("[.r]", &[]),
        (".T", &[]),
        ("p\n\n.T", &["paragraph:p"]),
        ("p\n\n[[id]]", &["paragraph:p"]),
        ("[#a2]\n:x: y", &[]),
        ("[#a2]\n\n", &[]),
    ];
    let wrong: Vec<String> = rows
        .iter()
        .filter_map(|(src, want)| {
            let got = blocks_with_metadata(src);
            let same = got
                .as_ref()
                .is_ok_and(|g| g.iter().map(String::as_str).eq(want.iter().copied()));
            (!same).then(|| format!("{src:?}: got {got:?}, asciidoctor reads {want:?}"))
        })
        .collect();
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn lines_read_as_asciidoctor_reads_them() {
    const HEADER_BLOCK: &str = "= T\n\n////\nhidden\n////\n\nshown text";
    const HEADER_ATTR_BLOCK: &str = "= T\n:a: b\n\n////\nhidden\n////\n\nshown text";
    const AUTHOR_AFTER_BLOCK: &str = "= T\n////\nx\n////\nJane Doe\n\nbody";
    const ATTR_AFTER_BLOCK: &str = "= T\n:a: b\n////\nx\n////\n:c: d\n\nbody {a} {c}";
    let rows: &[(&str, &[&str])] = &[
        // A `////` block before the title is skipped with the header's other comments.
        ("////\nhidden\n////\n\nshown text", &["paragraph:shown text"]),
        ("////\nhidden\n////\n= T\n\nbody", &["title:T", "paragraph:body"]),
        (HEADER_BLOCK, &["title:T", "comment", "paragraph:shown text"]),
        (HEADER_ATTR_BLOCK, &["title:T", "comment", "paragraph:shown text"]),
        // Comments inside the header: skipped, a `////` block whole (asciidoctor
        // `Reader#skip_comment_lines`).
        ("= T\n//\nJane Doe\n\nbody", &["title:T", "author:Jane Doe", "paragraph:body"]),
        (AUTHOR_AFTER_BLOCK, &["title:T", "author:Jane Doe", "paragraph:body"]),
        ("= T\n////\nx\n////\n:a: b\n\nbody {a}", &["title:T", "paragraph:body b"]),
        (ATTR_AFTER_BLOCK, &["title:T", "paragraph:body b d"]),
        ("= T\n//\n:a: b\n\nbody {a}", &["title:T", "paragraph:body b"]),
        // Between list items a comment line is dropped; a `////` block ends the list.
        ("* a\n//\n* b", &["list/unordered:2"]),
        ("* a\n///x\n* b", &["list/unordered:2"]),
        ("* a\n// c\n* b", &["list/unordered:2"]),
        ("* a\n////\nx\n////\n* b", &["list/unordered:1", "comment", "list/unordered:1"]),
        ("<<<", &["break/page"]),
        ("<<<\nnext para", &["break/page", "paragraph:next para"]),
        ("a\n\n<<<", &["paragraph:a", "break/page"]),
        ("== \nfiller", &["paragraph:==\nfiller"]),
        ("== \n\nfiller", &["paragraph:==", "paragraph:filler"]),
        ("x\n\n== \nfiller", &["paragraph:x", "paragraph:==\nfiller"]),
        ("long line\n== \ny", &["paragraph:long line\n==\ny"]),
        ("== A\n\npara\n== \nmore", &["section"]),
        ("== A\n\n== \nmore", &["section"]),
        ("[discrete]\n== \nx", &["paragraph:==\nx"]),
        ("= \nx", &["paragraph:=\nx"]),
        ("///\ntext", &["paragraph:///\ntext"]),
        ("//\ntext", &["comment", "paragraph:text"]),
        // A paragraph runs to a blank line: a section title, a list item, a block macro
        // or a block title on the next line is its text; a block attribute line ends it
        // (Glyph AsciiDoc simulator seed 84). A `[` line that does not close is text.
        ("text\n== Title\nmore", &["paragraph:text\n== Title\nmore"]),
        ("text\n* item", &["paragraph:text\n* item"]),
        ("text\ntoc::[]", &["paragraph:text\ntoc::[]"]),
        ("text\n.Title\nmore", &["paragraph:text\n.Title\nmore"]),
        ("text\n[NOTE]\n====\nx\n====", &["paragraph:text", "admonition/note"]),
        ("[.x'\n=== a]\ntoc::[]\n\npara", &["paragraph:[.x'\n=== a]\ntoc::[]", "paragraph:para"]),
        // An unclosed `**` in a section title is text (Glyph AsciiDoc simulator seed 143:
        // the whole document failed to parse).
        ("== ** a", &["section"]),
        ("== ** [ ] x", &["section"]),
        ("== ** https://a.co", &["section"]),
        ("== ** https://a.co:attr: v", &["section"]),
        ("== ** https://a.co:a", &["section"]),
        ("== ** a: v", &["section"]),
        ("x ** https://a.co:a", &["paragraph:x ** https://a.co:a"]),
        ("== https://a.co:a", &["section"]),
        ("x\n\n== ** [ ] https://a.co:attr: v", &["paragraph:x", "section"]),
        ("x\n\n===== ** [ ] https://a.co:attr: v\nm[beta] t+", &["paragraph:x", "section"]),
    ];
    let wrong: Vec<String> = rows
        .iter()
        .filter_map(|(src, want)| {
            let got = shape(src);
            let same = got
                .as_ref()
                .is_ok_and(|g| g.iter().map(String::as_str).eq(want.iter().copied()));
            (!same).then(|| format!("{src:?}: got {got:?}, asciidoctor reads {want:?}"))
        })
        .collect();
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}
