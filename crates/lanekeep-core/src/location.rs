//! Where a violation is.

use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// A path as it appears in lanekeep's output.
///
/// Two normalizations happen here, both for determinism rather than tidiness.
///
/// **Relative to the project root.** An absolute path embeds the checkout directory, so
/// the same corpus checked in `/home/ana/app` and `/ci/build/app` would produce different
/// output and different cache entries for identical content.
///
/// **Forward slashes always.** `std::path` uses `\` on Windows, so an unnormalized path
/// would make output — and every committed snapshot — disagree between a developer's
/// machine and CI. Architecture §11 requires that two runs over identical input produce
/// identical output; "identical input" has to include the platform.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FilePath(String);

impl FilePath {
    /// Normalize a path that is already relative to the project root.
    ///
    /// Separators are converted to `/`, and a leading `./` is dropped.
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        let raw = path.as_ref().to_string_lossy().replace('\\', "/");
        let trimmed = raw.strip_prefix("./").unwrap_or(&raw);
        Self(trimmed.to_owned())
    }

    /// The normalized path.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for FilePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A one-based position within a file.
///
/// One-based because every consumer of this — editors, terminals, humans, and the agents
/// reading the output — counts from one. Storing zero-based and converting at the edge
/// means every reporter has the chance to forget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Position {
    /// One-based line.
    pub line: u32,
    /// One-based column, counted in characters rather than bytes.
    pub column: u32,
}

impl Position {
    /// The first position in a file.
    pub const START: Self = Self { line: 1, column: 1 };

    /// Build a position from one-based coordinates.
    #[must_use]
    pub const fn new(line: u32, column: u32) -> Self {
        Self { line, column }
    }
}

impl fmt::Display for Position {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.column)
    }
}

/// A file and a position within it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Location {
    /// Path relative to the project root, with forward slashes.
    pub file: FilePath,
    /// One-based position.
    pub position: Position,
}

impl Location {
    /// Build a location.
    #[must_use]
    pub fn new(file: FilePath, position: Position) -> Self {
        Self { file, position }
    }
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.file, self.position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_a_posix_path_unchanged() {
        assert_eq!(
            FilePath::new("src/components/Button.tsx").as_str(),
            "src/components/Button.tsx"
        );
    }

    #[test]
    fn converts_windows_separators() {
        // Without this, a snapshot committed from macOS fails on the Windows CI runner
        // even though nothing about the analysis differs.
        assert_eq!(
            FilePath::new(r"src\components\Button.tsx").as_str(),
            "src/components/Button.tsx"
        );
        assert_eq!(FilePath::new(r"src\a/b\c.ts").as_str(), "src/a/b/c.ts");
    }

    #[test]
    fn drops_a_leading_dot_slash() {
        // Glob expansion produces these inconsistently, and `./src/a.ts` and `src/a.ts`
        // must not be two different cache keys for one file.
        assert_eq!(FilePath::new("./src/a.ts").as_str(), "src/a.ts");
        assert_eq!(FilePath::new(r".\src\a.ts").as_str(), "src/a.ts");
    }

    #[test]
    fn does_not_mangle_a_dotfile() {
        assert_eq!(FilePath::new(".eslintrc.ts").as_str(), ".eslintrc.ts");
        assert_eq!(
            FilePath::new("src/.hidden/a.ts").as_str(),
            "src/.hidden/a.ts"
        );
    }

    #[test]
    fn paths_sort_deterministically() {
        let mut paths: Vec<FilePath> = ["src/b.ts", "src/a.ts", "lib/z.ts", "src/a/b.ts"]
            .iter()
            .map(FilePath::new)
            .collect();
        paths.sort();

        let rendered: Vec<&str> = paths.iter().map(FilePath::as_str).collect();
        assert_eq!(rendered, ["lib/z.ts", "src/a.ts", "src/a/b.ts", "src/b.ts"]);
    }

    #[test]
    fn positions_order_by_line_then_column() {
        let mut positions = vec![
            Position::new(2, 1),
            Position::new(1, 10),
            Position::new(1, 2),
            Position::new(10, 1),
        ];
        positions.sort();

        assert_eq!(
            positions,
            [
                Position::new(1, 2),
                Position::new(1, 10),
                Position::new(2, 1),
                Position::new(10, 1)
            ]
        );
    }

    #[test]
    fn renders_the_way_editors_expect() {
        let location = Location::new(FilePath::new("src/a.ts"), Position::new(12, 5));
        assert_eq!(location.to_string(), "src/a.ts:12:5");
    }

    #[test]
    fn start_is_one_based() {
        assert_eq!(Position::START, Position::new(1, 1));
        assert_eq!(Position::START.to_string(), "1:1");
    }

    #[test]
    fn file_path_serializes_as_a_bare_string() {
        // The JSON schema is a public contract; a path must not appear as `{"0": "..."}`.
        let path = FilePath::new("src/a.ts");
        assert_eq!(serde_json::to_string(&path).expect("ok"), "\"src/a.ts\"");
    }
}
