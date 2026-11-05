//! No argparse abbreviations, implicit values or flag-shaped paths cross this boundary.
use std::io;

#[derive(Debug)]
pub(super) struct Arguments {
    pub input: String,
    pub output: Option<String>,
    pub format: Option<String>,
    pub conversion: bool,
}

pub(super) fn parse(skill: &str, stem: &str, args: &[String]) -> io::Result<Arguments> {
    super::manifest::embedded_script(skill, stem)?;
    let formats: &[&str] = match (skill, stem) {
        (_, "extract") => &["json", "md"],
        ("docx" | "pptx", "create") => &["md", "json"],
        ("xlsx", "create") => &["json", "csv"],
        ("pdf", "create") => &["md", "text"],
        (_, "to_pdf") => &[],
        _ => return Err(io::Error::other("Unknown office argument schema")),
    };
    let mut input = None;
    let mut output = None;
    let mut format = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg.contains('=') || arg.contains('\0') {
            return Err(io::Error::other("Inline --key=value and NUL arguments are refused"));
        }
        match arg.as_str() {
            "--out" if stem == "create" && output.is_none() => {
                output = Some(value(&mut iter)?);
            }
            "--outdir" if stem == "to_pdf" && output.is_none() => {
                output = Some(value(&mut iter)?);
            }
            "--format" if !formats.is_empty() && format.is_none() => {
                let v = value(&mut iter)?;
                if !formats.contains(&v.as_str()) {
                    return Err(io::Error::other("Invalid --format for this script"));
                }
                format = Some(v);
            }
            v if !v.starts_with('-') && input.is_none() && !v.is_empty() => {
                input = Some(v.to_owned());
            }
            _ => return Err(io::Error::other("Unknown, duplicate or extra office argument")),
        }
    }
    if stem == "create" && output.is_none() {
        return Err(io::Error::other("Missing required --out"));
    }
    Ok(Arguments {
        input: input.ok_or_else(|| io::Error::other("Missing required input document"))?,
        output,
        format,
        conversion: stem == "to_pdf",
    })
}

fn value<'a>(iter: &mut impl Iterator<Item = &'a String>) -> io::Result<String> {
    match iter.next() {
        Some(value) if !value.is_empty() && !value.starts_with('-')
            && !value.contains(['=', '\0']) => Ok(value.clone()),
        _ => Err(io::Error::other("Missing or invalid office flag value")),
    }
}
