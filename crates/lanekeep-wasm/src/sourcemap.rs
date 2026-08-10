//! Turning a position in a compiled bundle back into the position an author wrote.
//!
//! A rule authored in TypeScript and compiled ahead of time does not run as the file its author
//! edited. Several files are flattened into one entry module, that module is compiled into
//! StarlingMonkey, and a thrown error's stack therefore carries positions in the flattened
//! module: `matches@entry.js:957:16`. `entry.js` is not a file anybody can open.
//!
//! **This is the only thing a source map affects here, and the scope is worth stating.** A
//! violation's position never comes from JavaScript — `report` takes a node handle and the host
//! reads the position out of the Rust arena, and the reduce phase's `reduce-location` carries a
//! position a rule captured from a node earlier. So nothing in this module can move a violation,
//! and nothing that reads a violation needs to know it exists. What it moves is where a *thrown*
//! error is reported, which is [`crate::WasmError::RuleFailed`] and nothing else.
//!
//! # Why the decoding is here rather than taken from a crate
//!
//! `sourcemap` 9.3.2 was vetted and passes `deny.toml` — advisories, licenses, bans and sources
//! all clean. It was not taken because of what it costs to have: 46 transitive crates, among them
//! `url` and the whole ICU normalization stack behind `idna`, `wasm-bindgen` and `js-sys` behind
//! `uuid`, `bitvec`, and a SIMD base64 decoder. That is a source-map *toolkit* — indexed maps,
//! RAM bundles, writing, debug ids — and what is wanted here is one lookup against one flat map.
//! `docs/architecture.md` §13 makes a small dependency surface a design property rather than a
//! preference, and 46 crates for a base64-VLQ decoder is not proportionate to a diagnostic.
//!
//! What that trade costs is that the decoding is ours to get right, which is why the tests below
//! start from hand-encoded mappings whose expected answers were worked out by hand.
//!
//! # What is deliberately not supported
//!
//! Index maps — the `sections` form — and `sourceRoot`. Neither is produced by the build this
//! reads (`crates/lanekeep-rules/typescript/rolldown.config.mjs`), and a map carrying either is
//! refused rather than half-read: a `sourceRoot` silently ignored would prefix every path with
//! nothing and name files that do not exist.

use std::fmt;

use crate::bindings::types::StackFrame;
use crate::error::WasmError;

/// One mapped position in the generated file.
///
/// Flat and `Copy`, because a map holds one per mapped token — 13 KB of mappings for the shipped
/// component is a few thousand of these — and they are only ever read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Segment {
    /// Zero-based column in the generated line this segment belongs to.
    generated_column: u32,
    /// Index into [`SourceMap::sources`].
    source: u32,
    /// Zero-based line in that source.
    line: u32,
    /// Zero-based column in that source.
    column: u32,
}

/// Where a generated position came from, ready to be shown to a reader.
///
/// One-based, unlike the map's own encoding, because that is what a stack frame is and what an
/// editor jumping to a position expects. The conversion happens once, here, rather than at each
/// of the places a position is rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Position<'a> {
    /// The source path, exactly as the map spells it.
    pub(crate) file: &'a str,
    /// One-based line.
    pub(crate) line: u32,
    /// One-based column.
    pub(crate) column: u32,
}

/// A source map, decoded once, ready to answer lookups.
///
/// Built by [`SourceMap::parse`] and never mutated afterwards, so it is shared behind an
/// [`std::sync::Arc`] by every worker that runs the component it belongs to.
pub(crate) struct SourceMap {
    /// The generated file this map describes — `entry.js`.
    ///
    /// **Required, though the specification makes it optional.** It is what says which frames
    /// this map explains, and a map that cannot answer that can only be applied by guessing:
    /// applying it to a frame from some other file would remap a real line number into a real
    /// position in a real source, which is the one outcome worse than leaving the frame alone.
    file: String,
    sources: Vec<String>,
    /// Segments grouped by zero-based generated line, each group ascending by column.
    ///
    /// A `Vec` of `Vec`s rather than one sorted list, because the query is always "this
    /// generated line, this column": the outer index makes the line lookup an array index and
    /// leaves a binary search over the handful of segments on it.
    lines: Vec<Vec<Segment>>,
}

impl fmt::Debug for SourceMap {
    /// Hand-written: the segment table is thousands of entries and printing it helps nobody.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SourceMap")
            .field("file", &self.file)
            .field("sources", &self.sources)
            .field("segments", &self.lines.iter().map(Vec::len).sum::<usize>())
            .finish()
    }
}

impl SourceMap {
    /// Decode a source map from the JSON a bundler wrote.
    ///
    /// # Errors
    ///
    /// Returns [`WasmError::Engine`] for anything malformed: a version other than 3, a missing
    /// `file` or `sources`, a `mappings` string that does not decode, or a segment naming a
    /// source the map does not have. A bad map means a broken build rather than anything about a
    /// rule, which is the variant's own definition — and it is an error rather than a shrug
    /// because a map that silently explains nothing leaves every failure reporting a position in
    /// a file that does not exist, with nothing red anywhere.
    pub(crate) fn parse(name: &str, json: &[u8]) -> Result<Self, WasmError> {
        let refuse = |detail: String| WasmError::Engine(format!("`{name}`'s source map: {detail}"));

        let value: serde_json::Value =
            serde_json::from_slice(json).map_err(|e| refuse(e.to_string()))?;
        let object = value
            .as_object()
            .ok_or_else(|| refuse("is not a JSON object".to_owned()))?;

        match object.get("version").and_then(serde_json::Value::as_u64) {
            Some(3) => {}
            other => {
                return Err(refuse(format!(
                    "declares version {other:?}, and only version 3 is understood"
                )));
            }
        }
        if object.contains_key("sections") {
            return Err(refuse(
                "is an index map, which this decoder does not read".to_owned(),
            ));
        }
        if object.contains_key("sourceRoot") {
            return Err(refuse(
                "sets `sourceRoot`, which this decoder does not apply — every source has to be \
                 spelled in full, or the paths it reports name files that are not there"
                    .to_owned(),
            ));
        }

        let file = object
            .get("file")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                refuse("names no `file`, so nothing says which frames it explains".to_owned())
            })?
            .to_owned();

        let sources = object
            .get("sources")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| refuse("has no `sources` array".to_owned()))?
            .iter()
            .map(|source| {
                source
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| refuse("has a source that is not a string".to_owned()))
            })
            .collect::<Result<Vec<_>, _>>()?;

        let mappings = object
            .get("mappings")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| refuse("has no `mappings` string".to_owned()))?;

        let lines = decode(mappings, sources.len()).map_err(refuse)?;
        Ok(Self {
            file,
            sources,
            lines,
        })
    }

    /// Where a one-based generated position came from, or `None` if the map does not say.
    ///
    /// # The column is advisory and the line is not
    ///
    /// A generated line with mappings on it always answers, even when the queried column
    /// precedes every one of them: the first segment on the line is taken rather than nothing.
    /// That is not the usual convention and it is deliberate. The line is what a reader acts on
    /// — it is what an editor jumps to and what a `git blame` is about — and a bundler emits one
    /// statement per generated line, so every segment on a line comes from a small neighborhood
    /// of one source line. Refusing the whole frame over a column that lands a few characters
    /// early would trade the answer for the footnote.
    ///
    /// A generated line with no mappings at all answers `None`, because there is nothing there to
    /// be approximately right about.
    pub(crate) fn lookup(&self, line: u32, column: u32) -> Option<Position<'_>> {
        let segments = self
            .lines
            .get(usize::try_from(line.checked_sub(1)?).ok()?)?;
        let zero_based = column.saturating_sub(1);

        // How many segments start at or before the column. `partition_point` is the binary
        // search; the segment wanted is the one before that boundary.
        let at = segments.partition_point(|segment| segment.generated_column <= zero_based);
        let segment = match at.checked_sub(1) {
            Some(index) => segments.get(index)?,
            // The documented departure, spelled as its own arm rather than folded into a
            // `saturating_sub` — which is what it was, and which made the policy indistinguishable
            // from the ordinary path and therefore untestable. Nothing starts at or before the
            // column, so the line's first segment is the answer.
            None => segments.first()?,
        };

        Some(Position {
            file: self.sources.get(usize::try_from(segment.source).ok()?)?,
            line: segment.line.saturating_add(1),
            column: segment.column.saturating_add(1),
        })
    }

    /// A guest's stack, in the space its author wrote rather than the one it was compiled to.
    ///
    /// # Three cases, and the second and third are not one case
    ///
    /// **A frame in a file this map does not describe is dropped.** For the shipped component
    /// those are the outermost three, `componentize-js`'s own generated glue, which name
    /// `/var/folders/…/T/…/sources/initializer.js` — a path in a temporary directory on whichever
    /// machine built the artifact, which exists nowhere else and means nothing to a reader.
    ///
    /// **A frame in the mapped file that the map can place is rewritten**, which is the point.
    ///
    /// **A frame in the mapped file that the map cannot place is kept exactly as it arrived.**
    /// 531 of the shipped map's 1,138 generated lines carry no segments, spread evenly, so this
    /// is an ordinary case rather than a corruption — and dropping such a frame would take a real
    /// call out of the middle of a stack with nothing to say it had been there. Bundle
    /// coordinates are worse than source coordinates and far better than a hole.
    ///
    /// These were one case until a reviewer separated them, and the difference was load-bearing
    /// in a way that is not obvious: with unplaceable frames dropped, a stack whose every frame
    /// was on an unmapped line remapped to nothing, and the "hand back what arrived" fallback
    /// then returned the `/var/folders` frames after all. Keeping them makes the fallback
    /// unreachable whenever the guest's own file is on the stack at all.
    ///
    /// # The fallback that is left, and what it now keys on
    ///
    /// A stack with *no* frame in the mapped file is handed back untouched. That is a map which
    /// does not describe this stack — a mispaired map, or a throw entirely inside the engine —
    /// and emptying the stack on the strength of a map that recognized none of it would be acting
    /// on an authority it has not demonstrated.
    pub(crate) fn remap(&self, frames: Vec<StackFrame>) -> Vec<StackFrame> {
        let mut remapped = Vec::with_capacity(frames.len());

        for frame in &frames {
            if frame.file != self.file {
                continue;
            }
            match self.lookup(frame.line, frame.column) {
                Some(position) => remapped.push(StackFrame {
                    function: frame.function.clone(),
                    file: position.file.to_owned(),
                    line: position.line,
                    column: position.column,
                }),
                None => remapped.push(frame.clone()),
            }
        }

        if remapped.is_empty() {
            frames
        } else {
            remapped
        }
    }
}

/// Decode a `mappings` string into per-generated-line segment lists.
///
/// The encoding, restated because getting one of these backwards is silent: groups are separated
/// by `;` and are one generated line each, segments within a group by `,`. Every field is a
/// *delta*. The generated column resets at each line; the source index, the source line and the
/// source column accumulate across the whole string, lines included. A segment of one field is a
/// generated position with no source, which is dropped — there is nothing to map it to.
fn decode(mappings: &str, sources: usize) -> Result<Vec<Vec<Segment>>, String> {
    let mut lines: Vec<Vec<Segment>> = Vec::new();
    let mut source = 0_i64;
    let mut source_line = 0_i64;
    let mut source_column = 0_i64;

    for group in mappings.split(';') {
        let mut segments: Vec<Segment> = Vec::new();
        let mut generated_column = 0_i64;

        for field in group.split(',') {
            if field.is_empty() {
                continue;
            }
            let bytes = field.as_bytes();
            let mut at = 0_usize;

            generated_column += vlq(bytes, &mut at)?;
            if at == bytes.len() {
                continue;
            }
            source += vlq(bytes, &mut at)?;
            source_line += vlq(bytes, &mut at)?;
            source_column += vlq(bytes, &mut at)?;
            // A fifth field is an index into `names`, which nothing here reads. It is left
            // undecoded rather than consumed: the segment ends either way.

            let segment = Segment {
                generated_column: nonnegative(generated_column, "a generated column")?,
                source: nonnegative(source, "a source index")?,
                line: nonnegative(source_line, "a source line")?,
                column: nonnegative(source_column, "a source column")?,
            };
            if usize::try_from(segment.source).unwrap_or(usize::MAX) >= sources {
                return Err(format!("names source {} and has {sources}", segment.source));
            }
            segments.push(segment);
        }

        // Ascending by generated column, which the encoding is supposed to guarantee and which
        // the binary search in `lookup` depends on. Sorted rather than asserted: a map that
        // merely violates a should-be is not worth refusing a component over, and an unsorted
        // list would make the search silently wrong.
        segments.sort_unstable_by_key(|segment| segment.generated_column);
        lines.push(segments);
    }

    Ok(lines)
}

/// One base64-VLQ value, advancing `at` past it.
///
/// Six bits a character: the top bit continues, the low bit of the assembled value is the sign.
fn vlq(bytes: &[u8], at: &mut usize) -> Result<i64, String> {
    let mut assembled = 0_u64;
    let mut shift = 0_u32;

    loop {
        let byte = *bytes
            .get(*at)
            .ok_or_else(|| "a segment ends inside a value".to_owned())?;
        *at += 1;

        let digit =
            base64(byte).ok_or_else(|| format!("`{}` is not a base64 digit", char::from(byte)))?;
        let value = u64::from(digit & 0b0001_1111);
        assembled |= value
            .checked_shl(shift)
            .ok_or_else(|| "a value is too long to be a position".to_owned())?;

        if digit & 0b0010_0000 == 0 {
            break;
        }
        shift += 5;
        if shift >= 64 {
            return Err("a value is too long to be a position".to_owned());
        }
    }

    // The sign is the low bit, so the magnitude is what is left after shifting it off. `i64::MIN`
    // has no positive counterpart, which `checked_neg` is what catches.
    let magnitude = i64::try_from(assembled >> 1)
        .map_err(|_| "a value is too large to be a position".to_owned())?;
    if assembled & 1 == 1 {
        magnitude
            .checked_neg()
            .ok_or_else(|| "a value is too large to be a position".to_owned())
    } else {
        Ok(magnitude)
    }
}

/// A base64 digit's value, or `None` for anything that is not one.
const fn base64(byte: u8) -> Option<u32> {
    match byte {
        b'A'..=b'Z' => Some((byte - b'A') as u32),
        b'a'..=b'z' => Some((byte - b'a') as u32 + 26),
        b'0'..=b'9' => Some((byte - b'0') as u32 + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// A running total that has to be a position, or a message naming which one went negative.
fn nonnegative(value: i64, what: &str) -> Result<u32, String> {
    u32::try_from(value).map_err(|_| format!("{what} decodes to {value}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The map the tests below decode: two sources, a handful of segments, encoded by hand.
    ///
    /// `A` is 0, `C` is 1, `E` is 2, `I` is 4, `D` is -1 — the low bit is the sign, so an odd
    /// base64 digit is a negative delta. Each segment is four deltas: generated column, source,
    /// source line, source column.
    ///
    /// **The first segment starts at generated column 1, not 0, and that is the whole reason this
    /// constant is written the way it is.** It was `AAAA,IACA` — a first segment at column 0 —
    /// and under that fixture no lookup could ever ask for a column *before* every segment on a
    /// line: column 0 was an exact hit on segment 0. So
    /// [`a_column_before_every_segment_still_answers_with_the_line`] was named for a policy it
    /// never reached, and inverting that policy to return nothing left all eleven tests green. A
    /// reviewer's mutation found it. One base64 digit is the difference.
    ///
    /// So: line 1 holds (col 1 → a.ts 0:0) and (col 5 → a.ts 1:0); line 2 holds (col 0 → a.ts
    /// 2:0), which is where the source line goes on accumulating across a line boundary while the
    /// generated column resets.
    const MAPPINGS: &str = "CAAA,IACA;AACA";

    fn map() -> SourceMap {
        let json = br#"{"version":3,"file":"out.js","sources":["a.ts","b.ts"],"mappings":"CAAA,IACA;AACA"}"#;
        SourceMap::parse("fixture", json).expect("the fixture decodes")
    }

    /// A frame in the fixture's generated file, at a position the caller chooses.
    fn frame(function: &str, file: &str, line: u32, column: u32) -> StackFrame {
        StackFrame {
            function: function.to_owned(),
            file: file.to_owned(),
            line,
            column,
        }
    }

    #[test]
    fn the_deltas_accumulate_the_way_the_encoding_says() {
        let decoded = decode(MAPPINGS, 2).expect("it decodes");
        assert_eq!(
            decoded,
            vec![
                vec![
                    Segment {
                        generated_column: 1,
                        source: 0,
                        line: 0,
                        column: 0
                    },
                    Segment {
                        generated_column: 5,
                        source: 0,
                        line: 1,
                        column: 0
                    },
                ],
                // The generated column resets on the new line and the source line does not,
                // which is the half of the encoding that is easy to get backwards.
                vec![Segment {
                    generated_column: 0,
                    source: 0,
                    line: 2,
                    column: 0
                }],
            ]
        );
    }

    #[test]
    fn a_lookup_answers_in_one_based_coordinates() {
        // Generated line 1, one-based column 6 is zero-based 5, which is exactly where the second
        // segment starts. Its source position is zero-based line 1, column 0 — one-based 2:1.
        let map = map();
        assert_eq!(
            map.lookup(1, 6),
            Some(Position {
                file: "a.ts",
                line: 2,
                column: 1
            })
        );
        // And a column inside that segment rather than at its start answers the same, which is
        // what "the last segment at or before the column" means.
        assert_eq!(map.lookup(1, 9), map.lookup(1, 6));
    }

    #[test]
    fn a_column_before_every_segment_still_answers_with_the_line() {
        // The documented departure from the usual convention, and now actually reached: the
        // fixture's first segment is at zero-based column 1, so a one-based column of 1 — zero
        // based 0 — precedes every segment on the line. Returning nothing here is the mutation
        // this test exists to kill.
        let map = map();
        assert_eq!(
            map.lookup(1, 1),
            Some(Position {
                file: "a.ts",
                line: 1,
                column: 1
            })
        );
    }

    #[test]
    fn a_line_with_no_mappings_answers_nothing() {
        // Distinct from the case above and the distinction is the point: there is no segment to
        // be approximately right about, so the frame is left as it is rather than sent somewhere.
        let json = br#"{"version":3,"file":"out.js","sources":["a.ts"],"mappings":"AAAA;;AACA"}"#;
        let map = SourceMap::parse("fixture", json).expect("it decodes");
        assert_eq!(map.lookup(2, 1), None);
        assert!(map.lookup(3, 1).is_some(), "the third line does map");
        assert_eq!(map.lookup(0, 1), None, "there is no generated line zero");
        assert_eq!(map.lookup(9, 1), None, "past the end of the file");
    }

    #[test]
    fn a_segment_naming_a_source_the_map_does_not_have_is_refused() {
        // Silently clamping would report a position in whichever source happened to be last,
        // which is a real file and a real line and entirely unrelated to the failure.
        let json = br#"{"version":3,"file":"out.js","sources":["a.ts"],"mappings":"AAAA,ICAA"}"#;
        let error = SourceMap::parse("fixture", json).expect_err("it is refused");
        assert!(
            error.to_string().contains("names source 1 and has 1"),
            "{error}"
        );
    }

    #[test]
    fn a_map_that_names_no_generated_file_is_refused() {
        // Without it nothing says which frames the map explains, and the only way to apply it
        // would be to assume every frame is in it.
        let json = br#"{"version":3,"sources":["a.ts"],"mappings":"AAAA"}"#;
        let error = SourceMap::parse("fixture", json).expect_err("it is refused");
        assert!(error.to_string().contains("names no `file`"), "{error}");
    }

    #[test]
    fn the_forms_this_decoder_does_not_read_are_refused_by_name() {
        for (json, expected) in [
            (
                &br#"{"version":3,"file":"o.js","sections":[]}"#[..],
                "index map",
            ),
            (
                &br#"{"version":3,"file":"o.js","sourceRoot":"/x","sources":["a.ts"],"mappings":"AAAA"}"#[..],
                "sourceRoot",
            ),
            (
                &br#"{"version":2,"file":"o.js","sources":["a.ts"],"mappings":"AAAA"}"#[..],
                "only version 3",
            ),
        ] {
            let error = SourceMap::parse("fixture", json).expect_err("it is refused");
            assert!(error.to_string().contains(expected), "{error}");
        }
    }

    #[test]
    fn a_malformed_mapping_is_an_error_rather_than_a_wrong_answer() {
        for mappings in ["A!AA", "AAA"] {
            assert!(
                decode(mappings, 1).is_err(),
                "`{mappings}` decoded to something"
            );
        }
    }

    #[test]
    fn negative_values_decode_and_a_position_that_goes_negative_does_not() {
        // `D` is -1: the low bit is the sign, so 3 encodes as `D` reading back as -1. A source
        // line that walks below zero is a corrupt map rather than a position.
        assert_eq!(vlq(b"D", &mut 0), Ok(-1));
        assert_eq!(vlq(b"C", &mut 0), Ok(1));
        assert_eq!(vlq(b"A", &mut 0), Ok(0));
        // A multi-character value, derived rather than copied: 10,000 shifts left past the sign
        // bit to 20,000, whose five-bit groups from the least significant are 0, 17 and 19 — the
        // first two carrying the continuation bit, giving `g`, `x` and `T`.
        assert_eq!(vlq(b"gxT", &mut 0), Ok(10_000));
        assert!(decode("AADA", 1).is_err(), "a line delta below zero");
    }

    #[test]
    fn a_frame_in_a_file_the_map_does_not_describe_is_dropped() {
        let map = map();
        let remapped = map.remap(vec![
            frame("inner", "out.js", 1, 6),
            frame(
                "glue",
                "/var/folders/x/T/y/sources/initializer.js",
                5588,
                22,
            ),
        ]);

        assert_eq!(remapped.len(), 1, "{remapped:?}");
        assert_eq!(remapped[0].function, "inner", "the name is the guest's");
        assert_eq!(remapped[0].file, "a.ts");
        assert_eq!((remapped[0].line, remapped[0].column), (2, 1));
    }

    #[test]
    fn a_frame_on_a_line_the_map_cannot_place_is_kept_as_it_arrived() {
        // The case that used to be folded in with the one above. Generated line 3 has no
        // segments — the fixture has two lines — so the frame cannot be placed, and dropping it
        // would take a real call out of the middle of a stack with nothing to say it was there.
        let map = map();
        let unplaceable = frame("middle", "out.js", 3, 1);
        let remapped = map.remap(vec![
            frame("inner", "out.js", 1, 6),
            unplaceable.clone(),
            frame(
                "glue",
                "/var/folders/x/T/y/sources/initializer.js",
                5588,
                22,
            ),
        ]);

        assert_eq!(remapped.len(), 2, "{remapped:?}");
        assert_eq!(remapped[0].file, "a.ts", "the placeable one is remapped");
        assert_eq!(
            remapped[1], unplaceable,
            "the unplaceable one survives in the coordinates it arrived in"
        );
    }

    #[test]
    fn an_unplaceable_stack_in_the_mapped_file_does_not_let_a_foreign_frame_back_in() {
        // The consequence of the two rules together, and the reason they have to be two. With
        // unplaceable frames dropped, this stack remapped to nothing, the "hand back what
        // arrived" fallback fired, and the build machine's temporary directory reappeared in a
        // diagnostic — after `remap`'s own documentation had promised it would not.
        let map = map();
        let remapped = map.remap(vec![
            frame("middle", "out.js", 3, 1),
            frame(
                "glue",
                "/var/folders/x/T/y/sources/initializer.js",
                5588,
                22,
            ),
        ]);

        assert_eq!(remapped, vec![frame("middle", "out.js", 3, 1)]);
    }

    #[test]
    fn a_stack_with_no_frame_in_the_mapped_file_is_handed_back_whole() {
        // The fallback that is left. This map describes `out.js` and this stack mentions no such
        // file, so it recognized nothing here — and emptying a stack on the strength of a map
        // that recognized none of it would be acting on authority it has not shown. A failure
        // that arrived with a stack must not leave with none.
        let map = map();
        let frames = vec![frame("glue", "elsewhere.js", 3, 1)];

        assert_eq!(map.remap(frames.clone()), frames);
    }

    /// Every segment of the shipped map lands inside the file it names.
    ///
    /// **The per-rule evidence, and the only form of it available.** The migrated rules are four
    /// programs in one bundle behind one map, and a map that is right about one rule and forty
    /// lines out about another satisfies every assertion made anywhere else: the CLI test drives
    /// one rule, and `the_map_covers_every_rule_the_component_hosts` in
    /// `crates/lanekeep-rules/tests/source_maps.rs` only checks that the four paths appear in
    /// `sources`. This decodes the committed map in full and holds all 2,500-odd segments to the
    /// files they name.
    ///
    /// A unit test rather than one under `tests/`, because the decoder is private to this crate
    /// and the whole point is to run the real one rather than a second copy of it. Reaching
    /// across the repository at run time is the pattern `tests/fixture_currency.rs` already uses
    /// for the same artifact.
    #[test]
    fn every_segment_of_the_shipped_map_lands_inside_the_source_it_names() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let json = std::fs::read(
            root.join("crates/lanekeep-rules/components/typescript-builtins.wasm.map"),
        )
        .expect("the sidecar map ships beside the component");
        let map = SourceMap::parse("typescript-builtins", &json).expect("the shipped map decodes");

        // Read once per source rather than once per segment: there are nine sources and a few
        // thousand segments.
        let lengths: Vec<usize> = map
            .sources
            .iter()
            .map(|source| {
                std::fs::read_to_string(root.join(source))
                    .unwrap_or_else(|e| panic!("`{source}` is not readable: {e}"))
                    .lines()
                    .count()
            })
            .collect();

        let mut referenced = vec![0_usize; map.sources.len()];
        let mut segments = 0_usize;
        for line in &map.lines {
            for segment in line {
                segments += 1;
                let at = usize::try_from(segment.source).expect("a source index is small");
                referenced[at] += 1;
                assert!(
                    (segment.line as usize) < lengths[at],
                    "a segment names line {} of `{}`, which has {} lines",
                    segment.line + 1,
                    map.sources[at],
                    lengths[at]
                );
            }
        }

        // A map that decoded to nothing would satisfy the loop above without asserting anything,
        // which is the one way this could be green while covering no rule at all.
        assert!(segments > 1000, "the map decoded to {segments} segments");
        for (at, count) in referenced.iter().enumerate() {
            assert!(
                *count > 0,
                "`{}` is in `sources` and no segment points at it",
                map.sources[at]
            );
        }

        // And the four rules by name, so that a bundle which quietly stopped including one is a
        // failure here rather than a rule reporting in bundle coordinates in the field.
        for rule in [
            "no-circular-imports",
            "no-default-export",
            "no-restricted-imports",
            "no-unused-exports",
        ] {
            let expected = format!("crates/lanekeep-rules/rules/{rule}.ts");
            assert!(
                map.sources.contains(&expected),
                "the map does not cover `{rule}`: {:?}",
                map.sources
            );
        }
    }
}
