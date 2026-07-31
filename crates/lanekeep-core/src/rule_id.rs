//! Rule identity.
//!
//! A rule ID appears in four places that are expensive to change once users exist: config
//! files, suppression comments in source, JSON output consumed by other tools, and CI
//! configuration filtering on specific rules. Architecture §14 lists namespacing as the
//! most expensive of the one-way doors for exactly that reason.
//!
//! Parsing is therefore strict. Every rejection here is a case that would otherwise become
//! two IDs users believe are one, or one ID they believe is two.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

/// Where a rule came from.
///
/// Closed rather than open: an unknown namespace is a typo, not an extension point. Adding
/// a variant later is additive and cannot invalidate an existing ID, whereas accepting
/// arbitrary namespaces now would make `lanekep/foo` a valid ID that silently never matches
/// anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Namespace {
    /// A rule shipped with lanekeep and reviewed by a maintainer.
    Lanekeep,
    /// A rule authored in the project being checked.
    Local,
}

impl Namespace {
    /// The namespace as it appears in a rule ID.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lanekeep => "lanekeep",
            Self::Local => "local",
        }
    }

    /// Every namespace, for diagnostics that need to list the valid options.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[Self::Lanekeep, Self::Local]
    }
}

impl fmt::Display for Namespace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a string is not a valid rule ID.
///
/// Each variant carries what was actually seen. A diagnostic that says only "invalid rule
/// ID" makes the reader diff two strings by eye.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ParseRuleIdError {
    /// No `/` separating namespace from name.
    #[error(
        "rule ID `{0}` has no namespace: write `lanekeep/{0}` for a built-in rule or \
         `local/{0}` for one defined in this project"
    )]
    MissingNamespace(String),

    /// More than one `/`.
    #[error("rule ID `{0}` contains more than one `/`")]
    TooManySeparators(String),

    /// The namespace is not one lanekeep knows.
    #[error("unknown rule namespace `{namespace}` in `{id}`: expected one of {expected}")]
    UnknownNamespace {
        /// The namespace as written.
        namespace: String,
        /// The whole ID as written.
        id: String,
        /// Comma-separated list of valid namespaces.
        expected: String,
    },

    /// Nothing after the separator.
    #[error("rule ID `{0}` has an empty name")]
    EmptyName(String),

    /// The name violates the naming rules.
    #[error("invalid rule name `{name}` in `{id}`: {reason}")]
    InvalidName {
        /// The name portion as written.
        name: String,
        /// The whole ID as written.
        id: String,
        /// What specifically is wrong.
        reason: &'static str,
    },
}

/// A namespaced rule identifier, such as `lanekeep/no-default-export`.
///
/// Construct one by parsing: `"local/no-numeric-sizes".parse::<RuleId>()`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RuleId {
    namespace: Namespace,
    name: String,
}

impl RuleId {
    /// Which namespace this rule belongs to.
    #[must_use]
    pub const fn namespace(&self) -> Namespace {
        self.namespace
    }

    /// The name portion, without the namespace or separator.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether this is a rule shipped with lanekeep.
    #[must_use]
    pub const fn is_built_in(&self) -> bool {
        matches!(self.namespace, Namespace::Lanekeep)
    }

    /// Build an ID from parts, validating the name.
    ///
    /// # Errors
    ///
    /// Returns [`ParseRuleIdError`] when the name is empty or not in the required form.
    pub fn new(namespace: Namespace, name: &str) -> Result<Self, ParseRuleIdError> {
        let id = format!("{}/{name}", namespace.as_str());
        validate_name(name, &id)?;
        Ok(Self {
            namespace,
            name: name.to_owned(),
        })
    }
}

/// Rule names are lowercase kebab-case: `no-default-export`.
///
/// Strictness buys one specific thing. These strings are typed by hand into suppression
/// comments, and a suppression that silently fails to match is worse than no suppression —
/// the violation reappears and the author believes they already handled it. Permitting
/// `No_Default_Export` alongside `no-default-export` would make near-miss IDs
/// indistinguishable from correct ones at a glance.
fn validate_name(name: &str, id: &str) -> Result<(), ParseRuleIdError> {
    if name.is_empty() {
        return Err(ParseRuleIdError::EmptyName(id.to_owned()));
    }

    let invalid = |reason: &'static str| {
        Err(ParseRuleIdError::InvalidName {
            name: name.to_owned(),
            id: id.to_owned(),
            reason,
        })
    };

    if !name.is_ascii() {
        return invalid("only ASCII letters, digits and hyphens are allowed");
    }
    if name.chars().any(|c| c.is_ascii_uppercase()) {
        return invalid("must be lowercase");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return invalid("only lowercase letters, digits and hyphens are allowed");
    }
    if name.starts_with('-') || name.ends_with('-') {
        return invalid("must not start or end with a hyphen");
    }
    if name.contains("--") {
        return invalid("must not contain consecutive hyphens");
    }

    Ok(())
}

impl FromStr for RuleId {
    type Err = ParseRuleIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut parts = s.split('/');
        let (Some(namespace), Some(name)) = (parts.next(), parts.next()) else {
            return Err(ParseRuleIdError::MissingNamespace(s.to_owned()));
        };
        if parts.next().is_some() {
            return Err(ParseRuleIdError::TooManySeparators(s.to_owned()));
        }

        let namespace = match namespace {
            "lanekeep" => Namespace::Lanekeep,
            "local" => Namespace::Local,
            other => {
                let expected = Namespace::all()
                    .iter()
                    .map(|n| format!("`{}`", n.as_str()))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(ParseRuleIdError::UnknownNamespace {
                    namespace: other.to_owned(),
                    id: s.to_owned(),
                    expected,
                });
            }
        };

        validate_name(name, s)?;
        Ok(Self {
            namespace,
            name: name.to_owned(),
        })
    }
}

impl fmt::Display for RuleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.namespace.as_str(), self.name)
    }
}

/// Ordering is over the rendered string, not over `(Namespace, name)`.
///
/// The distinction matters because violations are sorted by `(ruleId, file, line, column)`
/// and that order is part of lanekeep's output contract. Deriving `Ord` would order by the
/// `Namespace` enum's declaration order, so adding a variant — say, one sorting between the
/// existing two — would silently reorder every report without any output-producing code
/// changing. Comparing rendered strings is stable against that, and is also the order a
/// reader expects from looking at the output.
impl Ord for RuleId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.namespace
            .as_str()
            .cmp(other.namespace.as_str())
            .then_with(|| self.name.cmp(&other.name))
    }
}

impl PartialOrd for RuleId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Serialize for RuleId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for RuleId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<RuleId, ParseRuleIdError> {
        s.parse()
    }

    #[test]
    fn parses_a_built_in_id() {
        let id = parse("lanekeep/no-default-export").expect("valid");
        assert_eq!(id.namespace(), Namespace::Lanekeep);
        assert_eq!(id.name(), "no-default-export");
        assert!(id.is_built_in());
    }

    #[test]
    fn parses_a_project_id() {
        let id = parse("local/no-numeric-sizes").expect("valid");
        assert_eq!(id.namespace(), Namespace::Local);
        assert_eq!(id.name(), "no-numeric-sizes");
        assert!(!id.is_built_in());
    }

    #[test]
    fn accepts_digits_in_names() {
        assert!(parse("local/no-utf8-bom").is_ok());
        assert!(parse("local/rule2").is_ok());
    }

    #[test]
    fn round_trips_through_display() {
        for raw in ["lanekeep/no-default-export", "local/a", "local/x-1-y"] {
            let id = parse(raw).expect("valid");
            assert_eq!(id.to_string(), raw);
            assert_eq!(parse(&id.to_string()).expect("valid"), id);
        }
    }

    #[test]
    fn rejects_a_bare_name() {
        let err = parse("no-default-export").expect_err("must be namespaced");
        assert!(matches!(err, ParseRuleIdError::MissingNamespace(_)));
        // The message has to teach the fix, since this is the mistake everyone
        // migrating from another linter makes first.
        let msg = err.to_string();
        assert!(msg.contains("lanekeep/no-default-export"), "{msg}");
        assert!(msg.contains("local/no-default-export"), "{msg}");
    }

    #[test]
    fn rejects_an_unknown_namespace() {
        let err = parse("lanekep/no-default-export").expect_err("typo in namespace");
        match err {
            ParseRuleIdError::UnknownNamespace {
                namespace,
                expected,
                ..
            } => {
                assert_eq!(namespace, "lanekep");
                assert!(expected.contains("lanekeep"), "{expected}");
                assert!(expected.contains("local"), "{expected}");
            }
            other => panic!("wrong error: {other:?}"),
        }
    }

    #[test]
    fn rejects_extra_separators() {
        let err = parse("local/nested/rule").expect_err("one separator only");
        assert!(matches!(err, ParseRuleIdError::TooManySeparators(_)));
    }

    #[test]
    fn rejects_empty_parts() {
        assert!(matches!(
            parse("local/"),
            Err(ParseRuleIdError::EmptyName(_))
        ));
        assert!(matches!(
            parse("/rule"),
            Err(ParseRuleIdError::UnknownNamespace { .. })
        ));
        assert!(matches!(
            parse(""),
            Err(ParseRuleIdError::MissingNamespace(_))
        ));
        assert!(matches!(
            parse("/"),
            Err(ParseRuleIdError::UnknownNamespace { .. })
        ));
    }

    #[test]
    fn rejects_names_that_are_not_kebab_case() {
        // Each of these would otherwise be a second spelling of an existing rule, and a
        // suppression comment using the wrong spelling fails silently.
        for bad in [
            "No-Default-Export",
            "no_default_export",
            "no default export",
            "-leading",
            "trailing-",
            "double--hyphen",
            "no.default.export",
            "café",
            "rule!",
        ] {
            let raw = format!("local/{bad}");
            assert!(parse(&raw).is_err(), "should have rejected {raw}");
        }
    }

    #[test]
    fn constructor_validates_the_same_way_as_parsing() {
        assert!(RuleId::new(Namespace::Local, "ok-name").is_ok());
        assert!(RuleId::new(Namespace::Local, "Bad_Name").is_err());
        assert!(RuleId::new(Namespace::Local, "").is_err());

        let built = RuleId::new(Namespace::Lanekeep, "no-default-export").expect("valid");
        let parsed = parse("lanekeep/no-default-export").expect("valid");
        assert_eq!(built, parsed);
    }

    #[test]
    fn orders_by_rendered_string() {
        let mut ids: Vec<RuleId> = ["local/b", "lanekeep/z", "local/a", "lanekeep/a"]
            .iter()
            .map(|s| parse(s).expect("valid"))
            .collect();
        ids.sort();

        let rendered: Vec<String> = ids.iter().map(ToString::to_string).collect();
        assert_eq!(rendered, ["lanekeep/a", "lanekeep/z", "local/a", "local/b"]);
    }

    #[test]
    fn ordering_matches_string_ordering_exactly() {
        // The property that protects the output contract: however `Namespace` is
        // declared or extended, sorting rule IDs must agree with sorting their rendered
        // forms. If this ever fails, reports have silently reordered.
        let ids: Vec<RuleId> = [
            "lanekeep/a",
            "lanekeep/no-default-export",
            "local/a",
            "local/zzz",
            "lanekeep/zzz",
        ]
        .iter()
        .map(|s| parse(s).expect("valid"))
        .collect();

        for a in &ids {
            for b in &ids {
                assert_eq!(
                    a.cmp(b),
                    a.to_string().cmp(&b.to_string()),
                    "ordering disagreed for {a} vs {b}"
                );
            }
        }
    }

    #[test]
    fn serializes_as_a_plain_string() {
        let id = parse("lanekeep/no-default-export").expect("valid");
        let json = serde_json::to_string(&id).expect("serializes");
        assert_eq!(json, "\"lanekeep/no-default-export\"");

        let back: RuleId = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(back, id);
    }

    #[test]
    fn deserializing_rejects_an_invalid_id() {
        // Config and cache entries both arrive through serde, so validation cannot live
        // only in `FromStr` or a malformed ID enters through the side door.
        let err = serde_json::from_str::<RuleId>("\"nonsense\"").expect_err("invalid");
        assert!(err.to_string().contains("nonsense"), "{err}");
    }
}
