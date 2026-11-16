//! Round-trip real `.iop` files through the same import/export path the editor
//! uses, to catch codec and validation regressions on pools you already have.
//!
//! All tests are no-ops unless their environment variable is set:
//!
//! ```text
//! IOP_ROUNDTRIP="a.iop;b.iop" [IOP_ROUNDTRIP_OUT=dir] \
//!     cargo test --release --test iop_roundtrip -- --nocapture
//! IOP_DUMP=pool.iop          cargo test --release --test iop_roundtrip iop_dump -- --nocapture
//! IOP_TO_AITP="in.iop;out.aitp" cargo test --release --test iop_roundtrip iop_to_aitp -- --nocapture
//! ```
//!
//! For each pool `IOP_ROUNDTRIP` reports: the library alone (parse + write),
//! the editor import/export as VT3 and as VT6, and a VT3 -> VT6 upgrade whose
//! output must itself round-trip unchanged. `.iop` files carry no VT version,
//! so the version used for import matters.
use ag_iso_stack::object_pool::{vt_version::VtVersion, ObjectPool};
use ag_iso_terminal_designer::EditorProject;

fn paths(var: &str) -> Option<Vec<String>> {
    let list = std::env::var(var).ok()?;
    Some(
        list.split(';')
            .filter(|p| !p.is_empty())
            .map(String::from)
            .collect(),
    )
}

fn first_diff(a: &[u8], b: &[u8]) -> String {
    match a.iter().zip(b).position(|(x, y)| x != y) {
        Some(i) => format!("first difference at byte {i}"),
        None if a.len() == b.len() => "identical".into(),
        None => format!("common prefix, lengths {} vs {}", a.len(), b.len()),
    }
}

fn editor_roundtrip(name: &str, bytes: &[u8], version: VtVersion) -> Option<Vec<u8>> {
    let tag = format!("{name} as VT{}", u8::from(version));
    let project = match EditorProject::import_iop(bytes.to_vec(), version) {
        Ok(project) => project,
        Err(error) => {
            println!("  {tag}: IMPORT FAILED: {error}");
            return None;
        }
    };
    let objects = project.get_pool().size();
    match project.export_iop() {
        Ok(out) => {
            if let Ok(dir) = std::env::var("IOP_ROUNDTRIP_OUT") {
                let file = format!("{dir}/{name}.vt{}.iop", u8::from(version));
                std::fs::write(file, &out).unwrap();
            }
            let verdict = if out == bytes {
                "IDENTICAL".to_string()
            } else {
                format!("DIFFERS ({})", first_diff(bytes, &out))
            };
            println!(
                "  {tag}: {objects} objects, export {} bytes -> {verdict}",
                out.len()
            );
            Some(out)
        }
        Err(error) => {
            let message: Vec<_> = error
                .to_string()
                .lines()
                .take(8)
                .map(String::from)
                .collect();
            println!(
                "  {tag}: {objects} objects, EXPORT REFUSED: {}",
                message.join(" | ")
            );
            None
        }
    }
}

#[test]
fn iop_roundtrip() {
    let Some(pools) = paths("IOP_ROUNDTRIP") else {
        return;
    };
    for path in pools {
        let bytes = std::fs::read(&path).expect(&path);
        let name = std::path::Path::new(&path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        println!("=== {name} ({} bytes)", bytes.len());

        let library = ObjectPool::try_from_iop_for_version(bytes.clone(), VtVersion::Version3)
            .map_err(|e| e.to_string())
            .and_then(|pool| {
                let objects = pool.size();
                pool.try_as_iop_for_version(VtVersion::Version3)
                    .map(|out| (objects, out))
                    .map_err(|e| e.to_string())
            });
        match library {
            Ok((objects, out)) => println!(
                "  library only VT3: {objects} objects, {} bytes -> {}",
                out.len(),
                first_diff(&bytes, &out)
            ),
            Err(error) => println!("  library only VT3: FAILED {error}"),
        }

        let vt3 = editor_roundtrip(&name, &bytes, VtVersion::Version3);
        editor_roundtrip(&name, &bytes, VtVersion::Version6);

        if vt3.is_none() {
            continue;
        }
        let mut project = EditorProject::import_iop(bytes.clone(), VtVersion::Version3).unwrap();
        let upgraded = project
            .set_vt_version(VtVersion::Version6)
            .map_err(|e| e.to_string())
            .and_then(|()| project.export_iop().map_err(|e| e.to_string()));
        match upgraded {
            Ok(up) => {
                let again = EditorProject::import_iop(up.clone(), VtVersion::Version6)
                    .ok()
                    .and_then(|p| p.export_iop().ok());
                let verdict = match again {
                    Some(a) if a == up => "IDENTICAL".to_string(),
                    Some(a) => format!("DIFFERS ({})", first_diff(&up, &a)),
                    None => "FAILED".to_string(),
                };
                println!(
                    "  upgrade VT3->VT6: {} bytes; re-roundtrip {verdict}",
                    up.len()
                );
            }
            Err(error) => println!("  upgrade VT3->VT6: {error}"),
        }
    }
}

/// Print every object of a pool as JSON, with long arrays (picture data) elided.
#[test]
fn iop_dump() {
    let Ok(path) = std::env::var("IOP_DUMP") else {
        return;
    };
    let project =
        EditorProject::import_iop(std::fs::read(&path).unwrap(), VtVersion::Version6).unwrap();
    fn elide(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Array(items) if items.len() > 64 => {
                *value = format!("<{} items>", items.len()).into()
            }
            serde_json::Value::Array(items) => items.iter_mut().for_each(elide),
            serde_json::Value::Object(map) => map.values_mut().for_each(elide),
            _ => {}
        }
    }
    for object in project.get_pool().objects() {
        let mut value = serde_json::to_value(object).unwrap();
        elide(&mut value);
        println!("{value}");
    }
}

/// Import a VT3 `.iop` and save it as an `.aitp` project, e.g. to edit it with `aitd`.
#[test]
fn iop_to_aitp() {
    let Ok(spec) = std::env::var("IOP_TO_AITP") else {
        return;
    };
    let (input, output) = spec
        .split_once(';')
        .expect("IOP_TO_AITP=\"in.iop;out.aitp\"");
    let project =
        EditorProject::import_iop(std::fs::read(input).unwrap(), VtVersion::Version3).unwrap();
    std::fs::write(output, project.save_project().unwrap()).unwrap();
    println!("saved {output}");
}
