//! Optional craft-fonts build input (https://github.com/storytold/craft-fonts, recipe from its
//! `docs/integration.md`). With `CRAFT_FONTS_DIR=<checkout>` the fonts in its
//! `fonts/latin-manifest.txt` (presentation fonts) and `fonts/manifest.txt` are embedded as
//! `CRAFT_FONTS`, Latin first; unset, `CRAFT_FONTS` is empty. Web (wasm32) builds embed only Inter
//! Regular/Bold and BIZ UDPGothic Regular, to stay within the web size budget. It only reads the
//! local checkout: no network. With `CRAFT_FONTS_REQUIRED` set (release builds), a missing or
//! unreadable checkout is an error, and so is one lacking the Arabic fallback face or the real
//! italic faces (WHIQ, see `REQUIRED_SCRIPTS` and `REQUIRED_ITALIC`).
use std::fmt::Write as _;
use std::path::PathBuf;

/// WHIQ: with `CRAFT_FONTS_REQUIRED` set (release builds), native builds fail unless these scripts
/// are covered by an embedded face: Arabic (the fallback for Arabic text, so every machine draws
/// it the same), Japanese and Latin. Simplified Chinese (`Hans`) is not required: craft-fonts'
/// only Hans face is Noto Sans CJK SC, which DeckCraft's asset policy excludes.
const REQUIRED_SCRIPTS: &[&str] = &["Arab", "Jpan", "Latn"];
/// WHIQ: families that must ship a real italic face (no slanted Regular) in release builds: the
/// default face and the metric-compatible stand-ins for the common Office fonts.
const REQUIRED_ITALIC: &[&str] = &["Inter", "Carlito", "Caladea", "Liberation Sans", "Liberation Serif", "Liberation Mono"];

/// Families DeckCraft never embeds (AGENTS.md §1: no Adobe-authored visual design).
const EXCLUDED_FAMILIES: &[&str] = &["Source Sans 3", "Source Serif 4", "Source Han Sans", "Source Han Serif", "Noto Sans CJK SC"];

fn main() {
    println!("cargo::rerun-if-env-changed=CRAFT_FONTS_DIR");
    println!("cargo::rerun-if-env-changed=CRAFT_FONTS_REQUIRED");
    let mut src = String::from("pub static CRAFT_FONTS: &[CraftFont] = &[\n");
    let required = std::env::var_os("CRAFT_FONTS_REQUIRED").is_some();
    let wasm = std::env::var("CARGO_CFG_TARGET_ARCH").is_ok_and(|a| a == "wasm32");
    match std::env::var_os("CRAFT_FONTS_DIR").map(PathBuf::from) {
        None if required => println!("cargo::error=CRAFT_FONTS_REQUIRED is set but CRAFT_FONTS_DIR is not"),
        None => {}
        Some(dir) => match craft_fonts(&dir) {
            Ok((entries, faces)) => {
                src.push_str(&entries);
                if required && !wasm {
                    for e in coverage_errors(&faces) {
                        println!("cargo::error=CRAFT_FONTS_DIR={}: {e}", dir.display());
                    }
                }
            }
            Err(e) if required => {
                println!("cargo::error=CRAFT_FONTS_DIR={}: {e}", dir.display());
            }
            Err(e) => println!("cargo::warning=building without craft-fonts: CRAFT_FONTS_DIR={}: {e}", dir.display()),
        },
    }
    src.push_str("];\n");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap_or_default()).join("craft_fonts.rs");
    if let Err(e) = std::fs::write(&out, src) {
        println!("cargo::error=writing {}: {e}", out.display());
    }
}

/// An embedded face: family, style, scripts.
type Face = (String, String, Vec<String>);

/// What a release build lacks: required scripts no embedded face covers, and required families
/// without a real italic face.
fn coverage_errors(faces: &[Face]) -> Vec<String> {
    let mut errors = Vec::new();
    for script in REQUIRED_SCRIPTS {
        if !faces.iter().any(|(_, _, s)| s.iter().any(|s| s == script)) {
            errors.push(format!("no embedded face covers script {script}"));
        }
    }
    for family in REQUIRED_ITALIC {
        let italic = |s: &str| s.to_ascii_lowercase().contains("italic");
        if !faces.iter().any(|(f, s, _)| f == family && italic(s)) {
            errors.push(format!("no real italic face for {family} (is fonts/latin-manifest.txt at the release pin?)"));
        }
    }
    errors
}

/// One `CraftFont { .. }` initialiser per manifest line, and the faces embedded.
fn craft_fonts(dir: &std::path::Path) -> Result<(String, Vec<Face>), String> {
    let mut out = String::new();
    let mut faces = Vec::new();
    // The Latin presentation fonts are optional within craft-fonts (older checkouts lack them).
    let latin = dir.join("fonts/latin-manifest.txt");
    println!("cargo::rerun-if-changed={}", latin.display());
    if latin.exists() {
        out.push_str(&manifest(dir, &latin, &mut faces)?);
    }
    out.push_str(&manifest(dir, &dir.join("fonts/manifest.txt"), &mut faces)?);
    Ok((out, faces))
}

fn manifest(dir: &std::path::Path, manifest: &std::path::Path, faces: &mut Vec<Face>) -> Result<String, String> {
    println!("cargo::rerun-if-changed={}", manifest.display());
    let text = std::fs::read_to_string(manifest).map_err(|e| format!("{}: {e}", manifest.display()))?;
    let wasm = std::env::var("CARGO_CFG_TARGET_ARCH").is_ok_and(|a| a == "wasm32");
    let mut out = String::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let f: Vec<&str> = line.split(" | ").map(str::trim).collect();
        let [family, style, file, scripts, ..] = f.as_slice() else {
            return Err(format!("malformed manifest line: {line}"));
        };
        // DeckCraft's asset policy: no Adobe-authored typefaces, even openly licensed ones.
        if EXCLUDED_FAMILIES.contains(family) {
            continue;
        }
        if wasm && !matches!((*family, *style), ("BIZ UDPGothic", "Regular") | ("Inter", "Regular" | "Bold" | "Variable")) {
            continue;
        }
        let path = dir.join(file).canonicalize().map_err(|e| format!("{file}: {e}"))?;
        println!("cargo::rerun-if-changed={}", path.display());
        faces.push((family.to_string(), style.to_string(), scripts.split(',').map(|s| s.trim().to_string()).collect()));
        let scripts: Vec<String> = scripts.split(',').map(|s| format!("{:?}", s.trim())).collect();
        let _ = writeln!(
            out,
            "    CraftFont {{ family: {family:?}, style: {style:?}, scripts: &[{}], bytes: include_bytes!({:?}) }},",
            scripts.join(", "),
            path.display().to_string(),
        );
    }
    Ok(out)
}
