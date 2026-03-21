//! Scala tree-sitter extractor.
//!
//! Generic path: classes, objects, traits, case classes, methods (defs).
//! No special cases needed — the generic extractor handles the core patterns.

use std::path::Path;

use anyhow::Result;

use super::configs::SCALA_CONFIG;
use super::generic::GenericExtractor;
use super::{ExtractionResult, Extractor};

pub struct ScalaExtractor;

impl ScalaExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for ScalaExtractor {
    fn extensions(&self) -> &[&str] {
        &["scala", "sc"]
    }

    fn name(&self) -> &str {
        "scala-tree-sitter"
    }

    fn extract(&self, path: &Path, content: &str) -> Result<ExtractionResult> {
        GenericExtractor::new(&SCALA_CONFIG).run(path, content)
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
    fn test_scala_extract_class_and_object() {
        let extractor = ScalaExtractor::new();
        let code = r#"
package com.example

class UserRepository(db: Database) {
  def find(id: Int): Option[User] = db.find(id)
  def save(user: User): Unit = db.save(user)
}

object UserRepository {
  def apply(db: Database): UserRepository = new UserRepository(db)
}
"#;
        let result = extractor.extract(Path::new("src/UserRepository.scala"), code).unwrap();
        let structs: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Struct)
            .collect();
        assert!(!structs.is_empty(), "Should extract Scala classes/objects");
    }

    #[test]
    fn test_scala_extract_trait() {
        let extractor = ScalaExtractor::new();
        let code = r#"
trait Repository[T] {
  def find(id: Int): Option[T]
  def save(entity: T): Unit
}
"#;
        let result = extractor.extract(Path::new("src/Repository.scala"), code).unwrap();
        let traits: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Trait)
            .collect();
        assert!(!traits.is_empty(), "Should extract Scala traits");
    }

    #[test]
    fn test_scala_extract_def() {
        let extractor = ScalaExtractor::new();
        let code = r#"
object MathUtils {
  def add(a: Int, b: Int): Int = a + b
  def multiply(a: Int, b: Int): Int = a * b
}
"#;
        let result = extractor.extract(Path::new("src/MathUtils.scala"), code).unwrap();
        let funcs: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Function)
            .collect();
        assert!(!funcs.is_empty(), "Should extract Scala defs");
    }

    #[test]
    fn test_scala_extractor_extensions() {
        let extractor = ScalaExtractor::new();
        assert!(extractor.extensions().contains(&"scala"));
        assert!(extractor.extensions().contains(&"sc"));
        assert_eq!(extractor.name(), "scala-tree-sitter");
    }
}
