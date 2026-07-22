//! Exact, root-confined source reads used by projection and hydration.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use super::model::SourceSpan;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SourceReadLimits {
    pub(crate) max_lines: u32,
    pub(crate) max_span_bytes: usize,
    pub(crate) max_scanned_bytes: usize,
}

impl Default for SourceReadLimits {
    fn default() -> Self {
        Self {
            max_lines: 200,
            max_span_bytes: 64 * 1024,
            max_scanned_bytes: 16 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceSlice {
    pub(crate) span: SourceSpan,
    /// Exact UTF-8 bytes from the current filesystem, including original line endings.
    pub(crate) text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SourceError {
    InvalidRoot(String),
    UnknownRoot(String),
    InvalidPath(String),
    InvalidRange,
    TooManyLines {
        requested: u64,
        limit: u32,
    },
    EscapesRoot(String),
    NotFile(String),
    Io {
        operation: &'static str,
        kind: std::io::ErrorKind,
    },
    ScanLimit(usize),
    SpanLimit(usize),
    Binary,
    InvalidUtf8,
    OutOfRange {
        requested: u32,
        available: u32,
    },
}

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRoot(root) => write!(f, "invalid source root `{root}`"),
            Self::UnknownRoot(root) => write!(f, "unknown source root `{root}`"),
            Self::InvalidPath(path) => write!(f, "source path must be root-relative: `{path}`"),
            Self::InvalidRange => f.write_str("source lines must be a non-empty 1-based range"),
            Self::TooManyLines { requested, limit } => {
                write!(f, "source range has {requested} lines; limit is {limit}")
            }
            Self::EscapesRoot(path) => write!(f, "source path escapes its root: `{path}`"),
            Self::NotFile(path) => write!(f, "source path is not a regular file: `{path}`"),
            Self::Io { operation, kind } => write!(f, "source {operation} failed ({kind:?})"),
            Self::ScanLimit(limit) => write!(f, "source scan exceeded {limit} bytes"),
            Self::SpanLimit(limit) => write!(f, "source span exceeded {limit} bytes"),
            Self::Binary => f.write_str("source contains NUL bytes"),
            Self::InvalidUtf8 => f.write_str("source is not valid UTF-8"),
            Self::OutOfRange {
                requested,
                available,
            } => write!(
                f,
                "source line {requested} is out of range (file has {available} lines)"
            ),
        }
    }
}

impl std::error::Error for SourceError {}

#[derive(Debug, Clone)]
pub(crate) struct SourceReader {
    roots: BTreeMap<String, PathBuf>,
    limits: SourceReadLimits,
}

impl SourceReader {
    #[cfg(test)]
    pub(crate) fn for_root(
        slug: impl Into<String>,
        path: impl AsRef<Path>,
    ) -> Result<Self, SourceError> {
        Self::new(
            [(slug.into(), path.as_ref().to_path_buf())],
            SourceReadLimits::default(),
        )
    }

    pub(crate) fn new(
        roots: impl IntoIterator<Item = (String, PathBuf)>,
        limits: SourceReadLimits,
    ) -> Result<Self, SourceError> {
        let mut resolved = BTreeMap::new();
        for (slug, path) in roots {
            if slug.trim().is_empty() {
                return Err(SourceError::InvalidRoot(slug));
            }
            let canonical = fs::canonicalize(&path).map_err(|error| SourceError::Io {
                operation: "root resolution",
                kind: error.kind(),
            })?;
            if !canonical.is_dir() {
                return Err(SourceError::InvalidRoot(slug));
            }
            resolved.insert(slug, canonical);
        }
        Ok(Self {
            roots: resolved,
            limits,
        })
    }

    pub(crate) fn limits(&self) -> SourceReadLimits {
        self.limits
    }

    pub(crate) fn read(&self, span: &SourceSpan) -> Result<SourceSlice, SourceError> {
        if !span.is_valid() {
            return Err(SourceError::InvalidRange);
        }
        let line_count = u64::from(span.end_line) - u64::from(span.start_line) + 1;
        if line_count > u64::from(self.limits.max_lines) {
            return Err(SourceError::TooManyLines {
                requested: line_count,
                limit: self.limits.max_lines,
            });
        }
        let relative = Path::new(&span.path);
        if relative.is_absolute()
            || relative.components().any(|part| {
                matches!(
                    part,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(SourceError::InvalidPath(span.path.clone()));
        }
        let root = self
            .roots
            .get(&span.root)
            .ok_or_else(|| SourceError::UnknownRoot(span.root.clone()))?;
        let joined = root.join(relative);
        let canonical = fs::canonicalize(&joined).map_err(|error| SourceError::Io {
            operation: "path resolution",
            kind: error.kind(),
        })?;
        if !canonical.starts_with(root) {
            return Err(SourceError::EscapesRoot(span.path.clone()));
        }
        if !fs::metadata(&canonical)
            .map_err(|error| SourceError::Io {
                operation: "metadata",
                kind: error.kind(),
            })?
            .is_file()
        {
            return Err(SourceError::NotFile(span.path.clone()));
        }

        let mut file = fs::File::open(&canonical).map_err(|error| SourceError::Io {
            operation: "open",
            kind: error.kind(),
        })?;
        let mut buffer = [0_u8; 8192];
        let mut selected = Vec::new();
        let mut scanned = 0_usize;
        let mut line = 1_u32;
        let mut saw_byte = false;
        let mut last_newline = false;
        let mut complete = false;
        'read: loop {
            let count = file.read(&mut buffer).map_err(|error| SourceError::Io {
                operation: "read",
                kind: error.kind(),
            })?;
            if count == 0 {
                break;
            }
            scanned = scanned.saturating_add(count);
            if scanned > self.limits.max_scanned_bytes {
                return Err(SourceError::ScanLimit(self.limits.max_scanned_bytes));
            }
            for &byte in &buffer[..count] {
                saw_byte = true;
                last_newline = byte == b'\n';
                if byte == 0 {
                    return Err(SourceError::Binary);
                }
                if (span.start_line..=span.end_line).contains(&line) {
                    if selected.len() == self.limits.max_span_bytes {
                        return Err(SourceError::SpanLimit(self.limits.max_span_bytes));
                    }
                    selected.push(byte);
                }
                if byte == b'\n' {
                    if line == span.end_line {
                        complete = true;
                        break 'read;
                    }
                    line = line.saturating_add(1);
                }
            }
        }
        let available = if complete {
            span.end_line
        } else if !saw_byte {
            0
        } else if last_newline {
            line.saturating_sub(1)
        } else {
            line
        };
        if span.start_line > available {
            return Err(SourceError::OutOfRange {
                requested: span.start_line,
                available,
            });
        }
        if span.end_line > available {
            return Err(SourceError::OutOfRange {
                requested: span.end_line,
                available,
            });
        }
        let text = String::from_utf8(selected).map_err(|_| SourceError::InvalidUtf8)?;
        Ok(SourceSlice {
            span: span.clone(),
            text,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_unicode_and_original_line_endings() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("sample.rs"), "one\r\nβeta\nthree").unwrap();
        let reader = SourceReader::for_root("repo", dir.path()).unwrap();
        let span = SourceSpan {
            root: "repo".into(),
            path: "sample.rs".into(),
            start_line: 1,
            end_line: 2,
        };
        assert_eq!(reader.read(&span).unwrap().text, "one\r\nβeta\n");
    }

    #[test]
    fn rejects_parent_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let reader = SourceReader::for_root("repo", dir.path()).unwrap();
        let span = SourceSpan {
            root: "repo".into(),
            path: "../secret".into(),
            start_line: 1,
            end_line: 1,
        };
        assert!(matches!(
            reader.read(&span),
            Err(SourceError::InvalidPath(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret"), "nope").unwrap();
        symlink(outside.path().join("secret"), root.path().join("link")).unwrap();
        let reader = SourceReader::for_root("repo", root.path()).unwrap();
        let span = SourceSpan {
            root: "repo".into(),
            path: "link".into(),
            start_line: 1,
            end_line: 1,
        };
        assert!(matches!(
            reader.read(&span),
            Err(SourceError::EscapesRoot(_))
        ));
    }
}
