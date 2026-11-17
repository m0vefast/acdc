//! Generate one selected acdc ASG expectation without using converter defaults.

use std::{error::Error, fs, path::Path};

use acdc_parser::{CompatibilityMode, Options, asg, parse_file};

const USAGE: &str = "supply one .adoc filename from fixtures/asg, then optionally `draft`";

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let name = arguments.next().ok_or(USAGE)?;
    let name = Path::new(&name);
    // `draft` writes the Draft expectation `name.draft.json` beside the default one.
    let draft = match arguments.next() {
        None => false,
        Some(mode) if mode == "draft" => true,
        Some(_) => return Err(USAGE.into()),
    };
    if arguments.next().is_some()
        || name.components().count() != 1
        || name.extension().is_none_or(|ext| ext != "adoc")
    {
        return Err(USAGE.into());
    }
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/asg")
        .join(name);
    let options = if draft {
        Options::builder()
            .with_compatibility_mode(CompatibilityMode::Draft)
            .build()?
    } else {
        Options::default()
    };
    let parsed = parse_file(&source, &options)?;
    let value = asg::export_document_with_profile(parsed.document(), asg::Profile::Acdc)?;
    let output = format!("{}\n", serde_json::to_string_pretty(&value)?);
    fs::write(
        source.with_extension(if draft { "draft.json" } else { "json" }),
        output,
    )?;
    Ok(())
}
