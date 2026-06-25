use std::path::Path;

use crate::{SchemaError, SchemaModel};

const SCHEMA_FILE: &str = "schema.json";
const FINGERPRINT_FILE: &str = "fingerprint";

/// Write `schema.json` and a `fingerprint` file into `cache_dir` (created if needed).
pub fn save_schema(cache_dir: &Path, model: &SchemaModel) -> Result<(), SchemaError> {
    std::fs::create_dir_all(cache_dir)?;
    let json = serde_json::to_string_pretty(model)?;
    std::fs::write(cache_dir.join(SCHEMA_FILE), &json)?;
    std::fs::write(cache_dir.join(FINGERPRINT_FILE), model.fingerprint())?;
    Ok(())
}

/// Read `schema.json` from `cache_dir`; Ok(None) if it doesn't exist.
pub fn load_schema(cache_dir: &Path) -> Result<Option<SchemaModel>, SchemaError> {
    let path = cache_dir.join(SCHEMA_FILE);
    match std::fs::read_to_string(&path) {
        Ok(s) => {
            let model: SchemaModel = serde_json::from_str(&s)?;
            Ok(Some(model))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(SchemaError::Io(e)),
    }
}

/// Read the stored `fingerprint` file; None if absent.
pub fn cached_fingerprint(cache_dir: &Path) -> Option<String> {
    std::fs::read_to_string(cache_dir.join(FINGERPRINT_FILE)).ok()
}
