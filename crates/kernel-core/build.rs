use std::collections::BTreeSet;
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path};

fn main() {
    let manifest_dir = env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let content_dir = Path::new(&manifest_dir).join("../../spaces/content");
    let catalog_path = content_dir.join("catalog.json");
    let output_path =
        Path::new(&env::var_os("OUT_DIR").expect("OUT_DIR")).join("bundled_spaces.rs");

    println!("cargo:rerun-if-changed=../../spaces/content");

    let generated = match fs::metadata(&catalog_path) {
        Ok(metadata) if metadata.is_file() => generate_bundle(&content_dir, &catalog_path),
        Ok(_) => panic!("bundled Space catalog is not a file: {}", catalog_path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            "static BUNDLED_CATALOG: Option<&str> = None;\nstatic BUNDLED_PACKAGES: &[(&str, &str)] = &[];\n".to_owned()
        }
        Err(error) => panic!("read bundled Space catalog metadata: {error}"),
    };
    fs::write(output_path, generated).expect("write bundled Space data");
}

fn generate_bundle(content_dir: &Path, catalog_path: &Path) -> String {
    let catalog_bytes = fs::read(catalog_path).expect("read bundled Space catalog");
    let catalog: serde_json::Value =
        serde_json::from_slice(&catalog_bytes).expect("parse bundled Space catalog JSON");
    let content_root = content_dir
        .canonicalize()
        .expect("resolve bundled Space content directory");
    let spaces = catalog
        .get("spaces")
        .and_then(serde_json::Value::as_array)
        .expect("bundled Space catalog has a spaces array");
    let mut package_paths = BTreeSet::new();

    for space in spaces {
        let versions = space
            .get("versions")
            .and_then(serde_json::Value::as_array)
            .expect("bundled Space entry has a versions array");
        for version in versions {
            let package_path = version
                .get("package_path")
                .and_then(serde_json::Value::as_str)
                .expect("bundled Space version has a package_path string");
            let relative_path = Path::new(package_path);
            assert!(
                !relative_path.is_absolute()
                    && !relative_path
                        .components()
                        .any(|part| part == Component::ParentDir),
                "bundled Space package path must be relative and cannot contain '..': {package_path}"
            );
            let absolute_path = content_dir
                .join(relative_path)
                .canonicalize()
                .unwrap_or_else(|error| {
                    panic!("resolve bundled Space package {package_path}: {error}")
                });
            assert!(
                absolute_path.starts_with(&content_root),
                "bundled Space package path escapes spaces/content: {package_path}"
            );
            println!("cargo:rerun-if-changed={}", absolute_path.display());
            package_paths.insert((package_path.to_owned(), absolute_path));
        }
    }

    let catalog_path = catalog_path
        .canonicalize()
        .expect("resolve bundled Space catalog");
    let catalog_path_literal = rust_string_literal(&catalog_path.to_string_lossy());
    let mut generated = format!(
        "static BUNDLED_CATALOG: Option<&str> = Some(include_str!({catalog_path_literal}));\n"
    );
    generated.push_str("static BUNDLED_PACKAGES: &[(&str, &str)] = &[\n");
    for (package_path, absolute_path) in package_paths {
        let package_path_literal = rust_string_literal(&package_path);
        let absolute_path_literal = rust_string_literal(&absolute_path.to_string_lossy());
        writeln!(
            generated,
            "    ({package_path_literal}, include_str!({absolute_path_literal})),"
        )
        .expect("write package include");
    }
    generated.push_str("];\n");
    generated
}

#[allow(clippy::unnecessary_debug_formatting)]
fn rust_string_literal(value: &str) -> String {
    format!("{value:?}")
}
