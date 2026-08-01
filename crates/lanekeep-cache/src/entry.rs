//! What a cache entry holds, and how it is encoded.
//!
//! One entry per `(file, key)`: everything the per-file pass produced for that file, plus
//! the dependencies whose contents the result relied on.
//!
//! # Why a hand-written encoding
//!
//! The warm-run budget is milliseconds for the whole corpus, and a text format would spend
//! most of it parsing. A binary serialization crate would do the job, but the value type
//! here is a handful of strings, integers and one enum — small enough that the format is
//! less code than the dependency review, and it leaves nothing to a crate's version
//! compatibility rules.
//!
//! # Decoding is total
//!
//! Every decode path returns `None` rather than failing or panicking. The cache is
//! disposable: a truncated file, a torn write, a byte flipped on disk, a file written by a
//! different build — all of them mean "recompute", never an error the user has to act on. A
//! cache that can break a run is worse than no cache.

use lanekeep_core::fact::Fact;
use lanekeep_core::suppression::{Date, Scope, Suppression};
use lanekeep_core::tracked::{ContentHash, TrackedRead};
use lanekeep_core::{FilePath, Location, Position, RuleId, Severity, Violation};

/// Everything one file's pass produced.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Entry {
    /// Violations found in this file.
    pub violations: Vec<Violation>,
    /// Facts this file's rules emitted.
    pub facts: Vec<Fact>,
    /// Files this file's rules read, and what they hashed to.
    pub dependencies: Vec<TrackedRead>,

    /// The file's suppression directives.
    ///
    /// Stored because a reduce-phase violation can be reported at a site in a file that was
    /// not reprocessed this run. Without them, the warm path would drop the directive and
    /// report a violation the author had already accepted.
    pub suppressions: Vec<Suppression>,

    /// Indices into `suppressions` of the directives that silenced something.
    ///
    /// Recorded because a warm run never sees what a directive suppressed — the entry holds
    /// the violations that survived, and the ones it hid are gone. Without this, every
    /// suppression in a cached file would look unused.
    pub used_suppressions: Vec<u32>,
}

impl Entry {
    /// Append the encoded form to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) {
        write_len(out, self.violations.len());
        for violation in &self.violations {
            write_str(out, &violation.rule_id.to_string());
            write_str(out, violation.location.file.as_str());
            out.extend_from_slice(&violation.location.position.line.to_le_bytes());
            out.extend_from_slice(&violation.location.position.column.to_le_bytes());
            write_str(out, &violation.message);
            write_str(out, &violation.remediation);
            out.push(severity_code(violation.severity));
        }

        write_len(out, self.facts.len());
        for fact in &self.facts {
            write_str(out, &fact.rule_id.to_string());
            write_str(out, fact.file.as_str());
            write_str(out, &fact.kind);
            write_str(out, &fact.data);
            out.extend_from_slice(&fact.sequence.to_le_bytes());
        }

        write_len(out, self.suppressions.len());
        for suppression in &self.suppressions {
            out.push(match suppression.scope {
                Scope::NextLine => 0,
                Scope::File => 1,
            });
            out.extend_from_slice(&suppression.line.to_le_bytes());
            out.extend_from_slice(&suppression.column.to_le_bytes());
            write_str(out, &suppression.reason);

            write_len(out, suppression.rules.len());
            for rule in &suppression.rules {
                write_str(out, &rule.to_string());
            }

            match suppression.expires {
                Some(date) => {
                    out.push(1);
                    out.extend_from_slice(&date.year.to_le_bytes());
                    out.push(date.month);
                    out.push(date.day);
                }
                None => out.push(0),
            }
        }

        write_len(out, self.used_suppressions.len());
        for index in &self.used_suppressions {
            out.extend_from_slice(&index.to_le_bytes());
        }

        write_len(out, self.dependencies.len());
        for read in &self.dependencies {
            write_str(out, read.path.as_str());
            match read.hash {
                // A present/absent flag rather than a sentinel hash: "this file was not
                // there" is a distinct answer from any possible digest, and encoding it as
                // one would make an unlucky file collide with absence.
                Some(hash) => {
                    out.push(1);
                    out.extend_from_slice(hash.as_bytes());
                }
                None => out.push(0),
            }
        }
    }

    /// Decode an entry, or `None` if the bytes are not one.
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let mut cursor = Cursor::new(bytes);

        let mut violations = Vec::with_capacity(cursor.peek_len()?);
        for _ in 0..cursor.read_len()? {
            let rule_id = cursor.read_str()?.parse::<RuleId>().ok()?;
            let file = FilePath::new(cursor.read_str()?);
            let line = cursor.read_u32()?;
            let column = cursor.read_u32()?;
            violations.push(Violation {
                rule_id,
                location: Location::new(file, Position::new(line, column)),
                message: cursor.read_str()?.to_owned(),
                remediation: cursor.read_str()?.to_owned(),
                severity: severity_from(cursor.read_u8()?)?,
            });
        }

        let mut facts = Vec::with_capacity(cursor.peek_len()?);
        for _ in 0..cursor.read_len()? {
            facts.push(Fact {
                rule_id: cursor.read_str()?.parse::<RuleId>().ok()?,
                file: FilePath::new(cursor.read_str()?),
                kind: cursor.read_str()?.to_owned(),
                data: cursor.read_str()?.to_owned(),
                sequence: cursor.read_u32()?,
            });
        }

        let mut suppressions = Vec::with_capacity(cursor.peek_len()?);
        for _ in 0..cursor.read_len()? {
            let scope = match cursor.read_u8()? {
                0 => Scope::NextLine,
                1 => Scope::File,
                _ => return None,
            };
            let line = cursor.read_u32()?;
            let column = cursor.read_u32()?;
            let reason = cursor.read_str()?.to_owned();

            let mut rules = Vec::with_capacity(cursor.peek_len()?);
            for _ in 0..cursor.read_len()? {
                rules.push(cursor.read_str()?.parse().ok()?);
            }

            let expires = match cursor.read_u8()? {
                0 => None,
                1 => Some(Date {
                    year: u16::from_le_bytes(cursor.take(2)?.try_into().ok()?),
                    month: cursor.read_u8()?,
                    day: cursor.read_u8()?,
                }),
                _ => return None,
            };

            suppressions.push(Suppression {
                scope,
                rules,
                reason,
                expires,
                line,
                column,
            });
        }

        let mut used_suppressions = Vec::with_capacity(cursor.peek_len()?);
        for _ in 0..cursor.read_len()? {
            let index = cursor.read_u32()?;
            // An index past the end would be an entry claiming a directive that is not
            // there. Refusing is one more way this stays total.
            if index as usize >= suppressions.len() {
                return None;
            }
            used_suppressions.push(index);
        }

        let mut dependencies = Vec::with_capacity(cursor.peek_len()?);
        for _ in 0..cursor.read_len()? {
            let path = FilePath::new(cursor.read_str()?);
            let hash = match cursor.read_u8()? {
                0 => None,
                1 => Some(ContentHash::new(cursor.read_hash()?)),
                _ => return None,
            };
            dependencies.push(TrackedRead { path, hash });
        }

        // Trailing bytes mean this is not the entry it claims to be. Accepting them would
        // let a stale suffix ride along into whatever the format grows next.
        cursor.finished().then_some(Self {
            violations,
            facts,
            dependencies,
            suppressions,
            used_suppressions,
        })
    }
}

/// Reading a byte slice without trusting any of it.
struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(count)?;
        let slice = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(slice)
    }

    fn read_u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }

    fn read_u32(&mut self) -> Option<u32> {
        let bytes: [u8; 4] = self.take(4)?.try_into().ok()?;
        Some(u32::from_le_bytes(bytes))
    }

    fn read_hash(&mut self) -> Option<[u8; 32]> {
        self.take(32)?.try_into().ok()
    }

    fn read_len(&mut self) -> Option<usize> {
        self.read_u32().map(|n| n as usize)
    }

    /// The next length without consuming it, for sizing a `Vec` before the loop.
    ///
    /// Deliberately clamped: a corrupt count of four billion would otherwise reserve
    /// gigabytes before the first read failed. The loop still reads the real count, so a
    /// clamped hint costs at most a reallocation on a legitimate entry.
    fn peek_len(&self) -> Option<usize> {
        let bytes: [u8; 4] = self.bytes.get(self.at..self.at + 4)?.try_into().ok()?;
        Some((u32::from_le_bytes(bytes) as usize).min(1024))
    }

    fn read_str(&mut self) -> Option<&'a str> {
        let len = self.read_len()?;
        std::str::from_utf8(self.take(len)?).ok()
    }

    const fn finished(&self) -> bool {
        self.at == self.bytes.len()
    }
}

fn write_len(out: &mut Vec<u8>, len: usize) {
    // Saturating rather than truncating: a count that did not fit would otherwise encode as
    // a small number and silently drop the tail on the next read.
    out.extend_from_slice(&u32::try_from(len).unwrap_or(u32::MAX).to_le_bytes());
}

fn write_str(out: &mut Vec<u8>, text: &str) {
    write_len(out, text.len());
    out.extend_from_slice(text.as_bytes());
}

/// Severity as one byte.
///
/// An explicit mapping rather than a cast, so reordering the enum cannot silently
/// reinterpret every cached violation in the world.
const fn severity_code(severity: Severity) -> u8 {
    match severity {
        Severity::Off => 0,
        Severity::Warn => 1,
        Severity::Error => 2,
    }
}

const fn severity_from(code: u8) -> Option<Severity> {
    match code {
        0 => Some(Severity::Off),
        1 => Some(Severity::Warn),
        2 => Some(Severity::Error),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn violation(rule: &str, file: &str, line: u32) -> Violation {
        Violation {
            rule_id: rule.parse().expect("valid id"),
            location: Location::new(FilePath::new(file), Position::new(line, 1)),
            message: "a message".to_owned(),
            remediation: "a remediation".to_owned(),
            severity: Severity::Error,
        }
    }

    fn fact(rule: &str, file: &str, sequence: u32) -> Fact {
        Fact {
            rule_id: rule.parse().expect("valid id"),
            file: FilePath::new(file),
            kind: "export".to_owned(),
            data: r#"{"kind":"export","symbol":"x"}"#.to_owned(),
            sequence,
        }
    }

    fn populated() -> Entry {
        Entry {
            violations: vec![
                violation("local/a", "src/a.ts", 1),
                violation("lanekeep/no-default-export", "src/a.ts", 9),
            ],
            facts: vec![
                fact("local/a", "src/a.ts", 0),
                fact("local/a", "src/a.ts", 1),
            ],
            dependencies: vec![
                TrackedRead::found(FilePath::new("package.json"), ContentHash::new([7; 32])),
                TrackedRead::absent(FilePath::new("tsconfig.json")),
            ],
            suppressions: vec![
                Suppression {
                    scope: Scope::NextLine,
                    rules: vec!["local/a".parse().expect("valid id")],
                    reason: "legacy".to_owned(),
                    expires: None,
                    line: 4,
                    column: 3,
                },
                Suppression {
                    scope: Scope::File,
                    rules: vec![
                        "local/a".parse().expect("valid id"),
                        "lanekeep/no-default-export".parse().expect("valid id"),
                    ],
                    reason: "generated".to_owned(),
                    expires: Some(Date {
                        year: 2026,
                        month: 12,
                        day: 31,
                    }),
                    line: 1,
                    column: 1,
                },
            ],
            used_suppressions: vec![1],
        }
    }

    #[test]
    fn which_suppressions_were_used_survives_a_round_trip() {
        // A warm run never sees what a directive suppressed, so without this every
        // suppression in a cached file would look unused.
        let entry = populated();
        assert_eq!(
            round_trip(&entry).expect("decodes").used_suppressions,
            entry.used_suppressions
        );
    }

    #[test]
    fn a_used_index_past_the_end_is_refused() {
        let mut entry = populated();
        entry.used_suppressions = vec![99];
        let mut bytes = Vec::new();
        entry.encode(&mut bytes);
        assert_eq!(Entry::decode(&bytes), None);
    }

    #[test]
    fn suppressions_survive_a_round_trip() {
        // A reduce-phase violation can land on a file that was a cache hit. Without these,
        // the warm path would drop the directive and report a violation the author had
        // already accepted.
        let entry = populated();
        let decoded = round_trip(&entry).expect("decodes");
        assert_eq!(decoded.suppressions, entry.suppressions);
    }

    #[test]
    fn an_expiry_survives_as_an_expiry() {
        let entry = populated();
        let decoded = round_trip(&entry).expect("decodes");
        assert_eq!(decoded.suppressions[0].expires, None);
        assert_eq!(
            decoded.suppressions[1].expires,
            Some(Date {
                year: 2026,
                month: 12,
                day: 31
            })
        );
    }

    fn round_trip(entry: &Entry) -> Option<Entry> {
        let mut bytes = Vec::new();
        entry.encode(&mut bytes);
        Entry::decode(&bytes)
    }

    #[test]
    fn a_populated_entry_survives_a_round_trip() {
        let entry = populated();
        assert_eq!(round_trip(&entry).as_ref(), Some(&entry));
    }

    #[test]
    fn an_empty_entry_survives_a_round_trip() {
        let entry = Entry::default();
        assert_eq!(round_trip(&entry).as_ref(), Some(&entry));
    }

    #[test]
    fn absence_survives_as_absence() {
        // The distinction a cache is wrong without: "this file was not there" must not come
        // back as "this file hashed to something".
        let entry = Entry {
            dependencies: vec![TrackedRead::absent(FilePath::new("tsconfig.json"))],
            ..Entry::default()
        };
        let decoded = round_trip(&entry).expect("decodes");
        assert_eq!(decoded.dependencies[0].hash, None);
    }

    #[test]
    fn every_severity_survives() {
        for severity in [Severity::Off, Severity::Warn, Severity::Error] {
            let mut entry = Entry::default();
            let mut v = violation("local/a", "src/a.ts", 1);
            v.severity = severity;
            entry.violations.push(v);
            assert_eq!(
                round_trip(&entry).expect("decodes").violations[0].severity,
                severity
            );
        }
    }

    #[test]
    fn non_ascii_text_survives() {
        // Messages carry rule-authored text, which is not ASCII in general — a length in
        // characters rather than bytes would truncate here.
        let mut entry = Entry::default();
        let mut v = violation("local/a", "src/a.ts", 1);
        v.message = "circular import: a → b → a".to_owned();
        v.remediation = "casse le cycle — extrais le partagé".to_owned();
        entry.violations.push(v);
        assert_eq!(round_trip(&entry).as_ref(), Some(&entry));
    }

    #[test]
    fn a_truncated_entry_decodes_to_nothing() {
        // A torn write means recompute, not a panic and not a partial entry.
        let entry = populated();
        let mut bytes = Vec::new();
        entry.encode(&mut bytes);

        for cut in 0..bytes.len() {
            assert_eq!(
                Entry::decode(&bytes[..cut]),
                None,
                "a {cut}-byte prefix decoded as an entry"
            );
        }
    }

    #[test]
    fn trailing_bytes_are_refused() {
        // Otherwise a stale suffix rides along into whatever the format grows next.
        let mut bytes = Vec::new();
        populated().encode(&mut bytes);
        bytes.push(0);
        assert_eq!(Entry::decode(&bytes), None);
    }

    #[test]
    fn a_corrupt_entry_never_panics() {
        // Every byte flipped, one at a time. The cache is disposable: garbage means
        // recompute, and a panic would make a corrupt file break every future run.
        let mut bytes = Vec::new();
        populated().encode(&mut bytes);

        for index in 0..bytes.len() {
            for bit in 0..8u32 {
                let mut corrupt = bytes.clone();
                corrupt[index] ^= 1 << bit;
                // The result may legitimately decode — flipping a byte inside a message
                // yields a different but valid entry. What must not happen is a panic.
                let _ = Entry::decode(&corrupt);
            }
        }
    }

    #[test]
    fn an_absurd_count_does_not_allocate_absurdly() {
        // A corrupt length must not reserve gigabytes before the read fails.
        let mut bytes = u32::MAX.to_le_bytes().to_vec();
        bytes.extend_from_slice(&[0; 8]);
        assert_eq!(Entry::decode(&bytes), None);
    }

    #[test]
    fn a_bad_severity_code_is_refused() {
        let mut bytes = Vec::new();
        Entry {
            violations: vec![violation("local/a", "src/a.ts", 1)],
            ..Entry::default()
        }
        .encode(&mut bytes);

        let last = bytes.len() - 1;
        bytes[last] = 9;
        assert_eq!(Entry::decode(&bytes), None);
    }

    #[test]
    fn a_bad_rule_id_is_refused() {
        // Rule ids are namespaced. A bare one in a cache file was written by something
        // else, and decoding it would smuggle an invalid id into the run's output.
        let mut bytes = Vec::new();
        write_len(&mut bytes, 1);
        write_str(&mut bytes, "bare-id");
        write_str(&mut bytes, "src/a.ts");
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        write_str(&mut bytes, "m");
        write_str(&mut bytes, "r");
        bytes.push(2);
        write_len(&mut bytes, 0);
        write_len(&mut bytes, 0);

        assert_eq!(Entry::decode(&bytes), None);
    }

    #[test]
    fn invalid_utf8_is_refused() {
        let mut bytes = Vec::new();
        write_len(&mut bytes, 1);
        write_len(&mut bytes, 2);
        bytes.extend_from_slice(&[0xff, 0xfe]);
        assert_eq!(Entry::decode(&bytes), None);
    }
}
