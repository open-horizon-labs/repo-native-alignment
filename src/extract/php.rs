//! PHP tree-sitter extractor.
//!
//! Generic path: functions, classes, methods, interfaces, traits.
//! No special cases — PHP follows the standard pattern cleanly.

use std::path::Path;

use anyhow::Result;

use super::configs::PHP_CONFIG;
use super::generic::GenericExtractor;
use super::{ExtractionResult, Extractor};

pub struct PhpExtractor;

impl Default for PhpExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl PhpExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for PhpExtractor {
    fn extensions(&self) -> &[&str] {
        &["php"]
    }

    fn name(&self) -> &str {
        "php-tree-sitter"
    }

    fn extract(&self, path: &Path, content: &str) -> Result<ExtractionResult> {
        GenericExtractor::new(&PHP_CONFIG).run(path, content)
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
    fn test_php_extract_functions() {
        let extractor = PhpExtractor::new();
        let code = r#"<?php
function greet(string $name): string {
    return "Hello, $name!";
}

function add(int $a, int $b): int {
    return $a + $b;
}
"#;
        let result = extractor.extract(Path::new("src/helpers.php"), code).unwrap();
        let funcs: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Function)
            .collect();
        assert!(!funcs.is_empty(), "Should extract PHP functions");
        let names: Vec<&str> = funcs.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names.contains(&"greet"), "Should find 'greet'");
        assert!(names.contains(&"add"), "Should find 'add'");
    }

    #[test]
    fn test_php_extract_classes() {
        let extractor = PhpExtractor::new();
        let code = r#"<?php
class UserRepository {
    private $db;

    public function __construct($db) {
        $this->db = $db;
    }

    public function find(int $id): ?User {
        return $this->db->find('users', $id);
    }
}

interface UserService {
    public function create(string $name): User;
}

trait Loggable {
    public function log(string $message): void {
        error_log($message);
    }
}
"#;
        let result = extractor.extract(Path::new("src/User.php"), code).unwrap();
        let classes: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Struct)
            .collect();
        assert!(!classes.is_empty(), "Should extract PHP classes/traits");

        let traits: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Trait)
            .collect();
        assert!(!traits.is_empty(), "Should extract PHP interfaces or traits as Trait");
    }

    #[test]
    fn test_php_extractor_extensions() {
        let extractor = PhpExtractor::new();
        assert_eq!(extractor.extensions(), &["php"]);
        assert_eq!(extractor.name(), "php-tree-sitter");
    }

    #[test]
    fn test_php_extract_methods() {
        let extractor = PhpExtractor::new();
        let code = r#"<?php
class Calculator {
    public function add(int $a, int $b): int {
        return $a + $b;
    }

    private function validate(int $n): bool {
        return $n >= 0;
    }
}
"#;
        let result = extractor.extract(Path::new("src/Calculator.php"), code).unwrap();
        let funcs: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Function)
            .collect();
        assert!(!funcs.is_empty(), "Should extract PHP methods");
    }
}
