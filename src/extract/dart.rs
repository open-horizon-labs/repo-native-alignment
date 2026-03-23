//! Dart tree-sitter extractor.
//!
//! Generic path: functions, classes, mixins, methods.
//! No special cases — the generic extractor handles the core patterns.

use std::path::Path;

use anyhow::Result;

use super::configs::DART_CONFIG;
use super::generic::GenericExtractor;
use super::{ExtractionResult, Extractor};

pub struct DartExtractor;

impl Default for DartExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl DartExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for DartExtractor {
    fn extensions(&self) -> &[&str] {
        &["dart"]
    }

    fn name(&self) -> &str {
        "dart-tree-sitter"
    }

    fn extract(&self, path: &Path, content: &str) -> Result<ExtractionResult> {
        GenericExtractor::new(&DART_CONFIG).run(path, content)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::NodeKind;

    #[test]
    fn test_dart_extract_class() {
        let extractor = DartExtractor::new();
        let code = r#"
class UserRepository {
  final Database _db;

  UserRepository(this._db);

  Future<User?> find(int id) async {
    return await _db.find(id);
  }

  Future<void> save(User user) async {
    await _db.save(user);
  }
}
"#;
        let result = extractor.extract(Path::new("lib/user_repository.dart"), code).unwrap();
        let classes: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Struct)
            .collect();
        assert!(!classes.is_empty(), "Should extract Dart classes");
    }

    #[test]
    fn test_dart_extract_function() {
        let extractor = DartExtractor::new();
        let code = r#"
void main() {
  runApp(MyApp());
}

String greet(String name) {
  return 'Hello, $name!';
}
"#;
        let result = extractor.extract(Path::new("lib/main.dart"), code).unwrap();
        let funcs: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Function)
            .collect();
        assert!(!funcs.is_empty(), "Should extract Dart top-level functions");
    }

    #[test]
    fn test_dart_extractor_extensions() {
        let extractor = DartExtractor::new();
        assert_eq!(extractor.extensions(), &["dart"]);
        assert_eq!(extractor.name(), "dart-tree-sitter");
    }
}
