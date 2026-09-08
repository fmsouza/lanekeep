//! The `tsc` provider: the project's own TypeScript compiler, behind [`TypeProvider`].
//!
//! # What this widens, stated where it happens
//!
//! Every other answer in this crate comes from bytes lanekeep read through a `FileAccess`,
//! confined to the project root. This one comes from a process lanekeep started, running the
//! project's own toolchain, reading whatever that toolchain reads — its `node_modules`
//! included. That is a real widening of `docs/architecture.md` §13's confinement and it is
//! opt-in and off by default.
//!
//! It is not new ambient authority in the binary: `crates/lanekeep-core/src/changed.rs`
//! already spawns `git` for `--since` and `--staged`, with the same `env_remove` hygiene and
//! the same two-shape error. That spawn is the model this one copies.
//!
//! # Why one process with serialized requests
//!
//! A `Program` is expensive to build and cheap to query, so the state has to outlive a single
//! question. A process per query would rebuild it every time; a thread pool over one process
//! would need the driver to interleave answers, which buys nothing because the checker is
//! single-threaded anyway. One process, one mutex, one request in flight.
//!
//! # How a dependency of an answer reaches the cache key
//!
//! Not through [`Query::files`]. The compiler reads what a `tsconfig.json` tells it to read,
//! which is a *program* rather than a per-question set of files, so recording those reads one
//! question at a time would record them after the answers that depended on them. Instead
//! [`TscProvider::programs`] asks the driver for its whole read set and
//! [`TypeProvider::begin_run`] folds that listing into the run key before any answer exists
//! (spec §5.6).
//!
//! **What the listing is: every path a compiler host was asked to read, with its content
//! hash.** The driver wraps `readFile` on the host it hands to config parsing and to
//! `createProgram`, so a `tsconfig.json`, every file in its `extends` chain, every
//! `package.json` module resolution consulted and every source file are all in it — none of
//! which except the last appears in a program's `getSourceFiles()`. The driver's own
//! `exportedType` and `complete` resolve specifiers outside any program, and they read through
//! the same recording host, so what they consult is recorded too.
//!
//! **What "no checkout location" means, exactly.** Every path is spelled relative to the
//! project root, `..` segments included, and the root and every path crossing the driver's
//! boundary are `realpath`ed first. That is what the guarantee rests on: TypeScript resolves a
//! `node_modules` specifier through `realpath`, so a root reached through a symlink — which on
//! macOS `$TMPDIR` always is — would otherwise put every resolved dependency *outside* the root
//! and list it as `../../../private/var/…/<the project's own directory name>/…`, which is the
//! checkout's location and, under pnpm, most of a listing. What the guarantee does **not**
//! cover is a file that genuinely lives outside the root: a monorepo sibling is listed as
//! `../shared/b.ts`, which encodes the root's depth relative to that file and nothing else
//! about where either sits.
//!
//! Two things are deliberately outside the listing. The `typescript` package's own directory is
//! excluded whole, because its bytes are a function of the compiler version, which
//! [`TscProvider::identity`] already folds — the package rather than only its `lib/`, since
//! `types.typescript` may point outside the root and the manifest resolution reads on the way
//! there would be `../`-prefixed. And absence probes — `fileExists`, `directoryExists` — are not
//! recorded: the key is recomputed from the current run's read set, so a file that appears and
//! changes what resolution finds changes what is *read*, and so changes the key.
//!
//! Any file the compiler read whose bytes moved therefore changes the key for the whole run,
//! which is stricter than per-file tracked reads rather than weaker than them.
//!
//! **A read first made by a *query* reaches the key on the following run, not on this one.**
//! `programs` is asked once, in `begin_run`, before any answer exists, so a file the compiler
//! reaches for the first time while answering `isAssignableTo` or `complete` joins the read set
//! after the key that run committed under. It is in the answer `programs` gives next, which for
//! a provider a session holds across runs (plan 6) is the following run's key — and a run that
//! spawns its own sidecar starts from an empty read set, so for that shape the following run
//! carries it only if the same read happens again before `begin_run` returns.
//!
//! That is nearly always so, and the residue is worth naming rather than rounding off.
//! `createProgram` resolves every import of every root file, so every `package.json`
//! `complete` consults has already been read while the programs were being built — `complete`
//! resolves exactly that file's own specifiers. The one shape not covered is
//! `isAssignableTo`'s `module` argument, which is written by a *rule* rather than by the file
//! and may name a module the file does not import: the `package.json`s on the way to it are
//! read only by the query. A change to one of those, with nothing else moving, does not move
//! the key. The alternative — re-asking `programs` after every file and rekeying — would make
//! the key depend on which files a run happened to check and in what order, which is a worse
//! bargain than a narrow, stated gap.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Mutex, PoisonError};

use lanekeep_core::{AnalysisBudget, FileAccess, FilePath, TypesConfig, analysis_overrun_fallback};

use crate::provider::{BeginRunError, Query, TypeProvider};
use crate::types::{Primitive, Symbol, Type};

/// The driver, embedded so no installation step can leave it stale or absent.
///
/// **Not covered by `oracle_identity()`**: `build.rs` folds only `*.rs`. Its bytes reach the
/// cache key through [`TscProvider::identity`] and nowhere else, which is why that fold is
/// load-bearing rather than defensive.
const DRIVER: &str = include_str!("driver.mjs");

/// How much of the sidecar's stderr is kept for a diagnostic.
///
/// Bounded because the buffer is memory the run pays for and a driver in a loop could fill it,
/// and because what a reader needs is the first failure rather than the thousandth.
const STDERR_KEPT: usize = 8 * 1024;

/// Why a provider could not answer.
///
/// Two shapes and a timeout, matching `lanekeep_core::changed::ChangeError` plus the one thing
/// a long-lived process has that a `git` invocation does not.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderError {
    /// The command could not be run at all — no Node, no such binary, no permission.
    ///
    /// Split from [`Self::Unloadable`] because the remedies are different and only one of them
    /// can work. This one used to carry both cases, so a project whose Node ran perfectly well
    /// and whose `typescript` package could not be found was told to put `types.command` on
    /// PATH — advice about the one part of the configuration that was already right.
    #[error(
        "cannot start the type provider: {0}\n  \
         `types.provider` is `tsc`, which runs the project's own toolchain, so it needs \
         `types.command` on PATH"
    )]
    Unavailable(String),

    /// It ran, and could not load a `typescript` this driver can use.
    ///
    /// The command is fine; the package is what has to move. See [`Self::Unavailable`].
    #[error(
        "the type provider started but could not use the project's `typescript`: {0}\n  \
         point `types.typescript` at the package to load"
    )]
    Unloadable(String),

    /// The driver could not be written into the project's `.lanekeep/`.
    ///
    /// Its own variant because it is the one failure here that is about neither the command nor
    /// the package: nothing has been run yet, and both other remedies — put `types.command` on
    /// PATH, point `types.typescript` somewhere else — are advice about a configuration that
    /// may be perfectly correct. What has to move is the directory's permissions.
    #[error(
        "cannot write the type provider's driver to {0}\n  \
         `types.provider` is `tsc`, which writes its sidecar into the project's `.lanekeep/`, \
         so that directory has to be writable"
    )]
    Unwritable(String),

    /// It ran and refused: a request failed, or the sidecar died holding one.
    #[error("the type provider refused: {0}")]
    Refused(String),

    /// A request outlived what was left of `timeouts.analysis`.
    #[error(
        "the type provider did not answer within the remaining `timeouts.analysis` budget\n  \
         the sidecar has been killed; raise `timeouts.analysis` or set `types.provider` to \
         `builtin`"
    )]
    Timeout,
}

/// Whether the sidecar is still running when a refusal is built.
///
/// It decides one thing: whether [`TscProvider::refused`] may join the stderr drain thread
/// before reading its buffer. Joining is what makes the child's dying words *there* rather
/// than racing the read — and it terminates only because the child is gone and its stderr pipe
/// with it, so asking for it on a live sidecar would hang the run instead.
#[derive(Clone, Copy)]
enum Sidecar {
    /// Killed and reaped by the caller, so the drain thread is about to end.
    Gone,
    /// Still serving. Whatever it has written so far is what the diagnostic gets.
    Live,
}

/// A failed exchange, before it becomes a [`ProviderError`].
///
/// The conversion needs the session lock released — `refused` joins a thread and reads a
/// second mutex — so the locked half of a request names the failure and the caller builds it.
enum Failure {
    /// The request outlived the remaining budget.
    Timeout,
    /// The sidecar said no, or stopped being able to say anything.
    Refused(String, Sidecar),
}

/// The live sidecar, and everything a request needs.
struct Session {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<std::io::Result<String>>,
    next_id: u64,
}

/// The project's own TypeScript compiler, driven through a sidecar process.
pub struct TscProvider {
    session: Mutex<Session>,
    /// The project root every relative path in a request is resolved against. Held rather
    /// than left to the child's working directory, so a question names an absolute file and
    /// no answer depends on where the driver happens to have been started.
    root: PathBuf,
    /// The handshake's budget; `hello` runs under it, and so does anything asked before a run
    /// begins.
    budget: AnalysisBudget,
    /// The current run's budget, set by `begin_run`. A provider a session holds across runs
    /// (plan 6) gets a fresh clock per run rather than one that ran out during the first.
    run_budget: Mutex<Option<AnalysisBudget>>,
    /// Whatever the sidecar wrote to stderr, kept for a diagnostic rather than inherited.
    ///
    /// Bytes, not text: the buffer is truncated at a byte bound, and decoding each 4 KiB read
    /// separately would mangle a character that straddles two of them. It is decoded once,
    /// where it is read.
    stderr: std::sync::Arc<Mutex<Vec<u8>>>,
    /// The drain thread, kept so a refusal can join it before reading the buffer above.
    ///
    /// Without the join, a child that died mid-sentence is read before its last write lands
    /// and the diagnostic is the empty string — which is exactly the case the buffer exists
    /// for. Taken out of the slot when joined, so a second refusal does not try again.
    stderr_drain: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// The first error this provider produced, kept forever.
    ///
    /// **The contract Task 9 wires the engine to.** Every [`TypeProvider`] method answers "I
    /// don't know" on a failure, because the trait has nowhere to put one — so without this a
    /// timed-out or refused sidecar would let the file mid-check finish *degraded* and be
    /// committed under a valid cache key, which is a limit quietly degrading a run instead of
    /// cancelling it. The engine asks [`TscProvider::failure`] after every file; a `Some`
    /// means discard that file's result and cancel the run with the error. First rather than
    /// last, because the first is the one that explains the rest.
    ///
    /// A `Mutex` rather than a `Cell`: [`TypeProvider`] is `Send + Sync` and a provider is
    /// shared across the engine's workers, which a `Cell` is not.
    failure: Mutex<Option<ProviderError>>,
    typescript_version: String,
    identity: Vec<u8>,
    programs_hash: Mutex<[u8; 32]>,
    /// How many of the run's files were typed by the ad-hoc program, for [`Self::notices`].
    ///
    /// Set by `programs`, per run, so a provider a session holds across runs reports what the
    /// current run found rather than what the first one did.
    adhoc: Mutex<usize>,
}

impl std::fmt::Debug for TscProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TscProvider")
            .field("typescript", &self.typescript_version)
            .finish_non_exhaustive()
    }
}

impl Drop for TscProvider {
    fn drop(&mut self) {
        // Killed rather than left to notice a closed stdin, and waited for rather than left as
        // a zombie. A driver mid-`createProgram` does not poll its input, so a run that ended
        // would otherwise leave a process holding a gigabyte of checker state for as long as
        // that build takes.
        if let Ok(mut session) = self.session.lock() {
            let _ = session.child.kill();
            let _ = session.child.wait();
        }
    }
}

impl TscProvider {
    /// Write the driver, start the sidecar, and complete the handshake.
    ///
    /// # Errors
    ///
    /// [`ProviderError::Unavailable`] when the command cannot be run,
    /// [`ProviderError::Unloadable`] when it runs and the `typescript` package cannot be used,
    /// [`ProviderError::Unwritable`] when the driver cannot be written into the project's
    /// `.lanekeep/`, [`ProviderError::Refused`] when the sidecar answers something this cannot
    /// read, and [`ProviderError::Timeout`] when `hello` outlives the budget. The first two are
    /// split because only one of their remedies can work; see the variants.
    pub fn spawn(
        root: &Path,
        config: &TypesConfig,
        budget: AnalysisBudget,
    ) -> Result<Self, ProviderError> {
        Self::spawn_with_env(root, config, budget, &[])
    }

    /// [`TscProvider::spawn`] with extra environment for the child.
    ///
    /// The only caller that passes anything is the budget test, which needs a real process to
    /// spend real time. Kept as a separate entry point rather than as a parameter on `spawn`
    /// so that no production call site can pass one by accident.
    ///
    /// # Errors
    ///
    /// As [`TscProvider::spawn`].
    pub fn spawn_with_env(
        root: &Path,
        config: &TypesConfig,
        budget: AnalysisBudget,
        env: &[(&str, &str)],
    ) -> Result<Self, ProviderError> {
        // Before `write_driver`, so a configuration that cannot start anything leaves no
        // `.lanekeep/` behind in a project that may never have had one.
        let (program, arguments) = config
            .command
            .split_first()
            .ok_or_else(|| ProviderError::Unavailable("`types.command` is empty".to_owned()))?;

        // Absolute, once, before anything is built from it — because the child's working
        // directory is this same root, and two of its arguments are paths joined onto it. A
        // relative root was therefore applied twice: node resolved
        // `<root>/.lanekeep/types-driver-….mjs` against a cwd that was already `<root>` and
        // reported a missing module at `<root>/<root>/…`, a path nobody wrote. `lanekeep check
        // .` and `lanekeep check src` are ordinary invocations, so this was every relative one.
        //
        // `absolute` rather than `canonicalize`: the root need only stop depending on a working
        // directory, and the driver realpaths it and every path crossing its boundary anyway —
        // which is what keeps the listing free of the checkout's location. Resolving links here
        // as well would put a second spelling of the root into `.lanekeep/`'s location for no
        // gain.
        //
        // `types.command[0]` is deliberately **not** given this treatment: it is a program
        // name, resolved by the OS through `PATH`, and joining it onto the project root would
        // make `node` mean `<root>/node`.
        let root = std::path::absolute(root).map_err(|e| {
            ProviderError::Unavailable(format!("cannot resolve the project root: {e}"))
        })?;

        let driver = write_driver(&root)?;

        let mut command = Command::new(program);
        command
            .args(arguments)
            .arg(&driver)
            .arg(&root)
            .arg(&config.typescript)
            .current_dir(&root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Captured, not inherited. A stack trace out of the driver names a broken
            // `tsconfig.json` or a lanekeep bug and must reach the reader — but through this
            // provider's own error message, not by interleaving with lanekeep's reporters on a
            // stream whose bytes are part of the tool's output.
            .stderr(Stdio::piped())
            // The same hygiene `changed.rs` applies to `git`: a variable the parent inherited
            // must not decide what the child resolves.
            .env_remove("NODE_OPTIONS")
            .env_remove("NODE_PATH")
            .env_remove("TS_NODE_PROJECT")
            // Test-only, and removed for the same reason as the other three: a variable
            // lanekeep never sets must not be able to arrive from the parent's environment
            // and make every request spend real time. The one caller that wants it sets it
            // back below, which is why the loop runs after the removes.
            .env_remove("LANEKEEP_TSC_DRIVER_DELAY_MS");
        for (key, value) in env {
            command.env(key, value);
        }

        let mut child = command
            .spawn()
            .map_err(|e| ProviderError::Unavailable(e.to_string()))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| ProviderError::Unavailable("no stdin on the sidecar".to_owned()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ProviderError::Unavailable("no stdout on the sidecar".to_owned()))?;
        let child_stderr = child
            .stderr
            .take()
            .ok_or_else(|| ProviderError::Unavailable("no stderr on the sidecar".to_owned()))?;

        let lines = read_lines(stdout)?;
        let (stderr, stderr_drain) = drain_stderr(child_stderr)?;

        let mut provider = Self {
            session: Mutex::new(Session {
                child,
                stdin,
                lines,
                next_id: 0,
            }),
            root,
            budget,
            run_budget: Mutex::new(None),
            stderr,
            stderr_drain: Mutex::new(Some(stderr_drain)),
            failure: Mutex::new(None),
            typescript_version: String::new(),
            identity: Vec::new(),
            programs_hash: Mutex::new([0; 32]),
            adhoc: Mutex::new(0),
        };

        // The handshake, before anything else and before `run_key`. Its answer is a cache-key
        // input, so a run that has not had it cannot key anything.
        let hello = provider.request("hello", &serde_json::json!({}))?;
        if let Some(error) = hello.get("error").and_then(serde_json::Value::as_str) {
            // The package could not be loaded at all. Named with the specifier as configured,
            // because the usual cause is a layout, not a typo: a pnpm workspace has no root
            // `node_modules/typescript`, and the remedy is `types.typescript` naming a
            // workspace package's copy.
            return Err(ProviderError::Unloadable(format!(
                "{error}; a pnpm workspace has no root `node_modules/typescript` — point \
                 `types.typescript` at a workspace package's copy"
            )));
        }
        let version = hello
            .get("typescript")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| provider.refused("`hello` carried no version", Sidecar::Live))?
            .to_owned();
        if let Some(missing) = hello
            .get("unsupported")
            .and_then(serde_json::Value::as_array)
        {
            let missing: Vec<&str> = missing
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect();
            return Err(ProviderError::Unloadable(format!(
                "typescript {version} at `{}` does not provide the compiler API the driver \
                 needs ({}); the tsc provider is written against the TypeScript 5.x API and \
                 measured against 5.9.3",
                config.typescript,
                missing.join(", "),
            )));
        }

        provider.identity = fold_identity(&version, DRIVER, config);
        provider.typescript_version = version;
        Ok(provider)
    }

    /// The TypeScript version the sidecar loaded.
    #[must_use]
    pub fn typescript_version(&self) -> &str {
        &self.typescript_version
    }

    /// Build every program the run's files belong to, and record their hash.
    ///
    /// Called once at prepare, before `run_key`. Eager because §5.6's dependency mechanism for
    /// this provider is the whole program listing rather than per-query tracked reads: the
    /// listing is the key, so it has to exist before a key does.
    ///
    /// # Errors
    ///
    /// As [`TscProvider::spawn`].
    pub fn programs(&self, files: &[FilePath]) -> Result<(), ProviderError> {
        // Absolute, like every other request. A relative path is resolved by the driver
        // against its working directory, and `process.cwd()` answers the *real* path — on
        // macOS `/private/var/...` where the root was given as `/var/...` — so the file
        // stopped being under `projectRoot`, its `tsconfig.json` was never found, and the
        // listing carried the checkout's location instead of a relative path.
        //
        // Filtered to what the driver can parse. Discovery hands over the whole corpus, and a
        // repository is mostly not TypeScript: a file the compiler has no `ScriptKind` for
        // reaches no program and contributes no row, so asking about it is work with no answer.
        let listed: Vec<String> = files
            .iter()
            .filter(|file| typed_extension(file.as_str()))
            .map(|file| self.absolute(file))
            .collect();
        let answer = self.request("programs", &serde_json::json!({ "files": listed }))?;
        let hash = fold_programs(&answer).inspect_err(|e| self.remember(e))?;
        if let Ok(mut slot) = self.programs_hash.lock() {
            *slot = hash;
        }
        let adhoc = adhoc_count(&answer).inspect_err(|e| self.remember(e))?;
        if let Ok(mut slot) = self.adhoc.lock() {
            *slot = adhoc;
        }
        Ok(())
    }

    /// The first error this provider produced, if it has produced one.
    ///
    /// See the `failure` field: every [`TypeProvider`] method answers `None`/`false` on a
    /// failure because the trait has nowhere to put an error, so this is the only way a caller
    /// can tell "the compiler says there is no type here" from "the compiler is gone". The
    /// engine asks after every file and cancels the run when it is `Some`.
    #[must_use]
    pub fn failure(&self) -> Option<ProviderError> {
        // Poison-tolerant: a panic in another thread while this lock was held must not turn a
        // recorded failure into `None`, which is the answer that lets a degraded run commit.
        // The value behind it is a plain `Option<ProviderError>` with no invariant a panic
        // could have left half-applied.
        self.failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Write one raw line to the sidecar, outside the request protocol.
    ///
    /// Test-only, and the only way to reach the id-mismatch path through the real driver: the
    /// driver answers a line it cannot parse with `id: 0`, which is then sitting in the stream
    /// ahead of the next request's own answer.
    #[cfg(test)]
    fn write_raw(&self, line: &str) {
        if let Ok(mut session) = self.session.lock() {
            let _ = session.stdin.write_all(line.as_bytes());
            let _ = session.stdin.flush();
        }
    }

    /// Kill the sidecar under the provider's feet.
    ///
    /// Test-only. Forcing a refusal needs a sidecar that has stopped answering, and there is
    /// no configuration that produces one on demand.
    #[cfg(test)]
    fn kill_sidecar(&self) {
        if let Ok(mut session) = self.session.lock() {
            let _ = stop(&mut session);
        }
    }

    /// Keep an error if it is the first one. Later ones are dropped, not overwritten.
    fn remember(&self, error: &ProviderError) {
        // Poison-tolerant for the reason [`TscProvider::failure`] gives: dropping the record of
        // a failure is the one outcome that cannot be allowed, and a poisoned lock here would
        // do exactly that.
        self.failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert_with(|| error.clone());
    }

    /// The hash of every program's files, as `analysis_hash` folds it.
    #[must_use]
    pub fn programs_hash(&self) -> [u8; 32] {
        self.programs_hash.lock().map_or([0; 32], |slot| *slot)
    }

    /// The budget every request after the handshake runs under: the current run's when
    /// `begin_run` set one, else the handshake's.
    ///
    /// Cloned rather than copied, and a clone shares the accumulator every [`AnalysisBudget`]
    /// clone shares — so what this charges is what the engine's own copy reads.
    fn current_budget(&self) -> AnalysisBudget {
        self.run_budget
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .unwrap_or_else(|| self.budget.clone())
    }

    /// Whatever the sidecar has said on stderr, as a suffix for a diagnostic.
    ///
    /// Empty when it has said nothing, so an error that already reads well is not given a
    /// blank tail.
    fn stderr_tail(&self) -> String {
        let Ok(kept) = self.stderr.lock() else {
            return String::new();
        };
        // Decoded once, here, over the whole buffer: the drain thread truncates at a byte
        // bound and a character can straddle two of its reads.
        let text = String::from_utf8_lossy(&kept);
        if text.trim().is_empty() {
            return String::new();
        }
        format!("\n  the sidecar wrote on stderr:\n{}", text.trim_end())
    }

    /// Wait for the drain thread to finish, so the buffer holds everything the child said.
    ///
    /// Only ever called with the child already killed and reaped — see [`Sidecar`]. The thread
    /// ends when its read of a closed pipe returns zero, which cannot happen while the sidecar
    /// still holds the write end.
    fn join_stderr(&self) {
        let handle = self
            .stderr_drain
            .lock()
            .ok()
            .and_then(|mut slot| slot.take());
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }

    /// The absolute path of a file in this project, as the driver names files.
    ///
    /// Forward slashes throughout: the path is interpolated into JSON, where a Windows
    /// separator opens an escape (`AGENTS.md`'s "a Windows path interpolated into a JSON
    /// string is invalid JSON"), and `path.resolve` accepts either separator.
    fn absolute(&self, file: &FilePath) -> String {
        self.root
            .join(file.as_str())
            .to_string_lossy()
            .replace('\\', "/")
    }

    /// `{"file", "start", "end"}` — the three fields every position-taking op shares.
    fn locate(&self, q: &Query<'_>) -> serde_json::Value {
        serde_json::json!({
            "file": self.absolute(q.file),
            "start": q.node.start_byte(),
            "end": q.node.end_byte(),
        })
    }

    /// One position-taking request, and its answer.
    fn ask_about(&self, op: &str, q: &Query<'_>) -> Result<serde_json::Value, ProviderError> {
        self.request(op, &self.locate(q))
    }

    /// One request, under whatever is left of the analysis budget.
    ///
    /// Every failure is kept by [`TscProvider::remember`] on the way out, because the
    /// [`TypeProvider`] methods above turn one into an "I don't know" the caller cannot tell
    /// from a real answer.
    fn request(
        &self,
        op: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, ProviderError> {
        let error = match self.exchange(op, body) {
            Ok(value) => return Ok(value),
            Err(Failure::Timeout) => ProviderError::Timeout,
            Err(Failure::Refused(detail, sidecar)) => self.refused(&detail, sidecar),
        };
        self.remember(&error);
        Err(error)
    }

    /// The locked half of a request: write the line, wait for the matching answer, read it.
    ///
    /// Returns a [`Failure`] rather than a [`ProviderError`] because building one needs the
    /// session lock released — [`TscProvider::refused`] joins a thread and takes a second
    /// mutex — and because a failure that leaves the protocol out of step has to kill the
    /// child here, while the lock that owns it is still held.
    fn exchange(&self, op: &str, body: &serde_json::Value) -> Result<serde_json::Value, Failure> {
        let budget = self.current_budget();
        let mut body = body.clone();
        let Ok(mut session) = self.session.lock() else {
            return Err(Failure::Refused(
                "the sidecar's mutex is poisoned".to_owned(),
                Sidecar::Live,
            ));
        };

        // Service time only, and the budget is read here rather than above the lock. One
        // sidecar answers one request at a time, so a worker that arrives while another's
        // request is in flight waits — and charging that wait would make the accumulator grow
        // with the number of rayon workers rather than with the work: fourteen workers each
        // waiting about 200 ms charged 2.866 s against 205 ms of wall clock, so a 60 s budget
        // bounded roughly 60/P seconds of real analysis and the breach message quoted a
        // duration nobody could observe. Charged from inside the lock, the sum of what every
        // worker charges is the wall time the sidecar was busy, which is the number the
        // message names. The same reasoning fixes where `remaining` is read: a request's I/O
        // timeout is what is left of the budget when the sidecar is about to work on it, not
        // when its caller joined the queue.
        //
        // Holding the session lock across the guard cannot deadlock: `Charge` takes no lock of
        // any kind — it reads an `Instant` here and adds to an atomic on drop — so there is no
        // second lock for an ordering to exist between. It drops before `session` does,
        // because a `let` binding declared later is dropped first, so the charged window ends
        // with the exchange rather than with the lock's release.
        let Some(remaining) = budget.remaining() else {
            return Err(Failure::Timeout);
        };
        let _charge = budget.charge();

        session.next_id += 1;
        let id = session.next_id;
        if let Some(object) = body.as_object_mut() {
            object.insert("id".to_owned(), serde_json::json!(id));
            object.insert("op".to_owned(), serde_json::json!(op));
        }

        let mut line = match serde_json::to_string(&body) {
            Ok(line) => line,
            Err(e) => return Err(Failure::Refused(e.to_string(), Sidecar::Live)),
        };
        line.push('\n');
        if let Err(e) = session
            .stdin
            .write_all(line.as_bytes())
            .and_then(|()| session.stdin.flush())
        {
            return Err(Failure::Refused(e.to_string(), stop(&mut session)));
        }

        let answer = match session.lines.recv_timeout(remaining) {
            Ok(Ok(line)) => line,
            Ok(Err(e)) => return Err(Failure::Refused(e.to_string(), stop(&mut session))),
            Err(RecvTimeoutError::Timeout) => {
                // Killed rather than abandoned, and waited for rather than left as a zombie. A
                // limit cancels the run, and leaving a process that is still building a program
                // behind would spend the machine's memory on an answer nobody will read.
                let _ = stop(&mut session);
                return Err(Failure::Timeout);
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(Failure::Refused(
                    "the sidecar exited without answering".to_owned(),
                    stop(&mut session),
                ));
            }
        };

        let parsed: serde_json::Value = match serde_json::from_str(&answer) {
            Ok(parsed) => parsed,
            // A line this cannot read is a line whose place in the stream is unknown, so the
            // next answer would be read against the wrong request. The session ends here.
            Err(e) => {
                return Err(Failure::Refused(
                    format!("{e}: {answer}"),
                    stop(&mut session),
                ));
            }
        };

        // The echoed id, compared rather than assumed. One stray line on stdout — the driver's
        // own malformed-input reply carries `id: 0` — would otherwise shift every later answer
        // by one and attribute each to the wrong question, silently and for the rest of the
        // run.
        let echoed = parsed.get("id").and_then(serde_json::Value::as_u64);
        if echoed != Some(id) {
            let echoed = echoed.map_or_else(|| "none".to_owned(), |value| value.to_string());
            return Err(Failure::Refused(
                format!(
                    "the sidecar answered id {echoed} for request id {id}; the protocol is out \
                     of step and every later answer would be attributed to the wrong question"
                ),
                stop(&mut session),
            ));
        }

        if parsed.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
            // The sidecar answered this exact request and said no: it is still serving, and
            // whatever comes next is still in step.
            let detail = parsed
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("no reason given");
            return Err(Failure::Refused(detail.to_owned(), Sidecar::Live));
        }
        Ok(parsed
            .get("value")
            .cloned()
            .unwrap_or(serde_json::Value::Null))
    }

    /// A refusal carrying whatever the sidecar said on the way down.
    ///
    /// Every refusal that has a sidecar to quote is built here, so none of them can be raised
    /// without the sidecar's own words attached.
    ///
    /// Three do not, deliberately, and they are [`fold_programs`]'s: a listing that is not a
    /// list of `[path, hash]` pairs is a *shape* the answer does not have, described in full by
    /// the value itself, and the sidecar that produced it is still serving — so there is
    /// nothing on stderr that the message would be improved by, and joining the drain thread
    /// would hang on a live child. Every other [`ProviderError::Refused`] outside tests comes
    /// through here.
    fn refused(&self, detail: &str, sidecar: Sidecar) -> ProviderError {
        if matches!(sidecar, Sidecar::Gone) {
            self.join_stderr();
        }
        ProviderError::Refused(format!("{detail}{}", self.stderr_tail()))
    }
}

/// Kill the sidecar and reap it, and say that it is gone.
///
/// Reaped rather than left as a zombie, and answered with [`Sidecar::Gone`] so the caller
/// knows the stderr drain thread is now joinable.
fn stop(session: &mut Session) -> Sidecar {
    let _ = session.child.kill();
    let _ = session.child.wait();
    Sidecar::Gone
}

/// The `FileAccess` on a [`Query`] is deliberately unused here — see this module's
/// documentation. This provider's dependencies are the program listing, folded into the run
/// key by [`TypeProvider::begin_run`] before any answer exists, not per-question tracked
/// reads.
impl TypeProvider for TscProvider {
    fn notices(&self) -> Vec<String> {
        let count = self.adhoc.lock().map_or(0, |slot| *slot);
        if count == 0 {
            return Vec::new();
        }
        vec![format!(
            "{count} file(s) typed without a `tsconfig.json` under the project root — there is \
             none, or the nearest one is above it, so the project's own compiler options \
             (`strict` among them) did not apply to them"
        )]
    }

    /// Yes. This provider's whole cost is a process building programs and answering requests,
    /// which is exactly what `timeouts.analysis` bounds.
    fn spends_analysis_budget(&self) -> bool {
        true
    }

    fn type_of(&self, q: Query<'_>) -> Option<Type> {
        decode_type(&self.ask_about("typeOf", &q).ok()?)
    }

    fn symbol_of(&self, q: Query<'_>) -> Option<Symbol> {
        decode_symbol(&self.ask_about("symbolOf", &q).ok()?)
    }

    fn return_type_of(&self, q: Query<'_>) -> Option<Type> {
        decode_type(&self.ask_about("returnTypeOf", &q).ok()?)
    }

    fn is_assignable_to(&self, q: Query<'_>, module: &str, name: &str) -> Option<bool> {
        let mut body = self.locate(&q);
        let object = body.as_object_mut()?;
        object.insert("module".to_owned(), serde_json::json!(module));
        object.insert("name".to_owned(), serde_json::json!(name));
        self.request("isAssignableTo", &body).ok()?.as_bool()
    }

    fn complete(&self, q: Query<'_>) -> bool {
        // `false` on a failure, which is the honest answer: a provider that could not say
        // whether every import resolved has not established that they did, and a rule reading
        // `complete` is deciding whether to stay silent.
        self.request(
            "complete",
            &serde_json::json!({ "file": self.absolute(q.file) }),
        )
        .ok()
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    }

    fn identity(&self) -> Vec<u8> {
        self.identity.clone()
    }

    /// True once the sidecar is gone, which for this provider is unrecoverable in place.
    ///
    /// Every failure path that leaves the protocol out of step kills the child (see `stop`),
    /// and nothing here starts another: the `Session` holds one `Child`, and a provider is
    /// shared behind an `Arc` across the run's workers, so respawning under them would be a
    /// second sidecar answering questions the first was asked. A session builds a new provider
    /// instead — see [`TypeProvider::needs_rebuild`].
    ///
    /// `try_wait` rather than a flag set where the child is killed: the child may also have
    /// died on its own — out of memory building a program is the realistic one — and a flag
    /// would only ever know about the deaths lanekeep caused.
    fn needs_rebuild(&self) -> bool {
        let mut session = self.session.lock().unwrap_or_else(PoisonError::into_inner);
        // An error from `try_wait` is a child whose state cannot be established, which is not a
        // child this provider can go on using either.
        !matches!(session.child.try_wait(), Ok(None))
    }

    fn revalidate(&self, _files: &FileAccess) {
        // Nothing to do: the sidecar's state is the programs it built, and `begin_run`
        // re-answers `programs` — re-reading every config's file set and rebuilding with
        // `oldProgram` — on every prepare, held provider or not. The program hash the run key
        // folds is therefore already current per request without this method's help.
    }

    fn begin_run(
        &self,
        files: &dyn Fn() -> Vec<FilePath>,
        budget: AnalysisBudget,
    ) -> Result<Vec<u8>, BeginRunError> {
        // The run's budget replaces the handshake's: a provider a session holds (plan 6) gets
        // a fresh accumulator per run rather than one that was already spent on the first.
        // Poison-tolerant, like the failure slot below: a fallback to the spawn-time budget would
        // charge the run against a different accumulator with a different duration.
        *self
            .run_budget
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(budget.clone());
        // And the sticky failure is cleared beside it, for the same reason and in the same
        // breath. The record exists so one broken answer cancels the run it broke; a session
        // that holds this provider across runs would otherwise cancel every later run for a
        // fault the previous one already reported, with no way to make progress. A sidecar
        // that is genuinely gone fails the very first request of the new run and records
        // itself again.
        //
        // Poison-tolerant, as `remember` and `failure` are and for the mirror of their reason:
        // a poisoned lock skipped here leaves the previous run's cancellation in place for
        // every run the session serves afterwards, which is exactly the condition this block
        // exists to remove.
        *self.failure.lock().unwrap_or_else(PoisonError::into_inner) = None;
        // Every program the run's files belong to, built now so their listing can go into the
        // key before a key exists (§5.6). Re-answered on every prepare, so a held provider
        // keys each run on the programs as they are then.
        self.programs(&files()).map_err(|e| match e {
            ProviderError::Timeout => BeginRunError::Timeout(
                budget
                    .overrun()
                    .unwrap_or_else(|| analysis_overrun_fallback(budget.budget())),
            ),
            other => BeginRunError::Failed(other.to_string()),
        })?;
        Ok(self.programs_hash().to_vec())
    }

    /// The sticky first error, in the shape the engine takes its exit from.
    ///
    /// Read after every file. The mapping is `begin_run`'s, deliberately: one failure must
    /// take the same exit whether it happened while the programs were being built or while a
    /// rule was asking a question, or the same broken sidecar would name `timeouts.analysis`
    /// in one phase and the toolchain in the other.
    ///
    /// The budget the overrun is measured against is the *run's* when `begin_run` set one, so
    /// a provider a session holds across runs reports against the run that broke rather than
    /// the one that spawned it.
    fn failure(&self) -> Option<BeginRunError> {
        let budget = self.current_budget();
        Some(match TscProvider::failure(self)? {
            ProviderError::Timeout => BeginRunError::Timeout(
                budget
                    .overrun()
                    .unwrap_or_else(|| analysis_overrun_fallback(budget.budget())),
            ),
            other => BeginRunError::Failed(other.to_string()),
        })
    }
}

/// The sidecar's stdout, one line per channel message.
///
/// A reader thread and a channel, because `BufReader::read_line` cannot be given a deadline.
/// This is what makes a hung sidecar killable rather than something the run waits on forever —
/// §5.5's "a request carries the remaining budget as its I/O timeout".
fn read_lines(
    stdout: std::process::ChildStdout,
) -> Result<Receiver<std::io::Result<String>>, ProviderError> {
    let (sender, lines): (SyncSender<std::io::Result<String>>, _) = sync_channel(16);
    std::thread::Builder::new()
        .name("lanekeep-tsc-reader".to_owned())
        .spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        if sender.send(Ok(line)).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = sender.send(Err(e));
                        break;
                    }
                }
            }
        })
        .map_err(|e| ProviderError::Unavailable(e.to_string()))?;
    Ok(lines)
}

/// The sidecar's stderr, drained into a bounded buffer.
///
/// A thread of its own, because a child that fills its stderr pipe while nobody drains it
/// blocks — which would present as exactly the timeout this provider exists to distinguish
/// from a real one.
/// The stderr buffer and the thread filling it.
///
/// A named pair rather than a tuple in the signature, because the two are only ever handed out
/// together and clippy is right that the spelled-out type is unreadable.
type Drain = (std::sync::Arc<Mutex<Vec<u8>>>, std::thread::JoinHandle<()>);

/// Bytes rather than text, and the bound applied after the push rather than before it. Checking
/// before would keep up to `STDERR_KEPT` plus one whole read, and decoding each read on its own
/// would replace any character that straddles two of them with a replacement character — so the
/// truncation is a byte truncation and the decode happens once, in [`TscProvider::stderr_tail`].
/// Reading continues past the bound with the bytes discarded, because a child whose stderr pipe
/// fills with nobody draining it blocks.
fn drain_stderr(stderr: std::process::ChildStderr) -> Result<Drain, ProviderError> {
    let kept = std::sync::Arc::new(Mutex::new(Vec::new()));
    let sink = std::sync::Arc::clone(&kept);
    let handle = std::thread::Builder::new()
        .name("lanekeep-tsc-stderr".to_owned())
        .spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut chunk = [0_u8; 4096];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        let Ok(mut buffer) = sink.lock() else { break };
                        if buffer.len() < STDERR_KEPT {
                            buffer.extend_from_slice(&chunk[..read]);
                            buffer.truncate(STDERR_KEPT);
                        }
                    }
                }
            }
        })
        .map_err(|e| ProviderError::Unavailable(e.to_string()))?;
    Ok((kept, handle))
}

/// The driver on disk, under the directory the watcher already ignores.
///
/// Named by its own hash, so two lanekeep versions in one project do not fight over one path,
/// and written to a temporary first and renamed: `std::fs::write` truncates before it writes,
/// so two runs starting at once would otherwise have one of them read an empty file — the
/// same truncate-then-write race `AGENTS.md` records for a test's fixture path.
///
/// **Older drivers are never pruned**, deliberately. The name is the content hash, so an
/// obsolete one is a file another lanekeep process may have open right now — a longer run
/// started before this binary was upgraded, or a `--watch` in another terminal — and deleting
/// it would kill that run's sidecar with a message about a missing module. They are a few
/// kilobytes each under `.lanekeep/`, which the whole directory's own removal already covers.
///
/// Called only after `types.command` has been validated, so a configuration that could never
/// start a sidecar does not create a `.lanekeep/` in a project that has none — and only from a
/// command that is actually going to *run* a check. `lanekeep rules` and `lanekeep explain`
/// read a run's metadata and prepare with no provider at all
/// (`PrepareOptions::without_provider`), so neither reaches this and neither leaves a
/// `.lanekeep/` behind in a project it only listed the rules of.
fn write_driver(root: &Path) -> Result<PathBuf, ProviderError> {
    let digest = blake3::hash(DRIVER.as_bytes()).to_hex();
    let dir = root.join(".lanekeep");
    // Every failure here is [`ProviderError::Unwritable`], naming the directory. They used to
    // be `Unavailable`, whose remedy is "`types.command` on PATH" — advice about the command,
    // raised by the one step that has not run it and cannot have anything against it.
    let unwritable =
        |e: &std::io::Error| ProviderError::Unwritable(format!("{}: {e}", dir.display()));
    std::fs::create_dir_all(&dir).map_err(|e| unwritable(&e))?;
    let final_path = dir.join(format!("types-driver-{}.mjs", &digest[..16]));
    if final_path.is_file() {
        return Ok(final_path);
    }
    let temporary = dir.join(format!(
        "types-driver-{}.{}.tmp",
        &digest[..16],
        std::process::id()
    ));
    std::fs::write(&temporary, DRIVER).map_err(|e| unwritable(&e))?;
    std::fs::rename(&temporary, &final_path).map_err(|e| unwritable(&e))?;
    Ok(final_path)
}

/// The provider's identity: what it is, not what it has answered.
///
/// Three terms, length-prefixed, for the reason `TypesConfig::canonical_bytes` gives: the
/// TypeScript version, the driver's bytes, and the `types` block. A result computed by a
/// different compiler, a different driver or a different configuration is not a valid result
/// for this run.
fn fold_identity(version: &str, driver: &str, config: &TypesConfig) -> Vec<u8> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"lanekeep-tsc-provider-v1");
    for field in [
        version.as_bytes(),
        driver.as_bytes(),
        config.canonical_bytes().as_slice(),
    ] {
        hasher.update(&u64::try_from(field.len()).unwrap_or(u64::MAX).to_le_bytes());
        hasher.update(field);
    }
    hasher.finalize().as_bytes().to_vec()
}

/// The program listing, folded. Sorted by the driver; folded here in the order given rather
/// than re-sorted, so a driver that stopped sorting is a changed hash rather than a silently
/// identical one.
///
/// # Errors
///
/// [`ProviderError::Refused`] on any shape this cannot read. A listing that is not an array of
/// `[string, string]` pairs used to fold to a *constant* — the same bytes for every malformed
/// answer, and for every run that got one — which is a cache key that says two different
/// programs are the same program. Not knowing what the sidecar meant is a refusal.
fn fold_programs(answer: &serde_json::Value) -> Result<[u8; 32], ProviderError> {
    let listing = answer.get("listing").unwrap_or(answer);
    let rows = listing.as_array().ok_or_else(|| {
        ProviderError::Refused(format!(
            "`programs` answered {answer}, whose `listing` is not a list of `[path, hash]` pairs"
        ))
    })?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"lanekeep-tsc-programs-v1");
    hasher.update(&u64::try_from(rows.len()).unwrap_or(u64::MAX).to_le_bytes());
    for row in rows {
        let pair = row.as_array().filter(|pair| pair.len() == 2);
        let pair = pair.ok_or_else(|| {
            ProviderError::Refused(format!(
                "`programs` answered a row {row} that is not a `[path, hash]` pair"
            ))
        })?;
        for field in pair {
            let text = field.as_str().ok_or_else(|| {
                ProviderError::Refused(format!(
                    "`programs` answered a row {row} whose fields are not both strings"
                ))
            })?;
            hasher.update(&u64::try_from(text.len()).unwrap_or(u64::MAX).to_le_bytes());
            hasher.update(text.as_bytes());
        }
    }
    // Which files fell to the ad-hoc program is a key input in its own right, and not one the
    // rows above carry: a file typed with the project's `strict` and the same file typed
    // without it have byte-identical listings, because the listing is paths and content
    // hashes. Folded as its own length-prefixed section rather than encoded into a row — a row
    // is `[path, hash]`, and putting a third fact inside one of two strings is exactly the
    // overloading the length prefixes exist to make unnecessary.
    hasher.update(b"lanekeep-tsc-adhoc-v1");
    let adhoc = adhoc_paths(answer)?;
    hasher.update(&u64::try_from(adhoc.len()).unwrap_or(u64::MAX).to_le_bytes());
    for path in adhoc {
        hasher.update(&u64::try_from(path.len()).unwrap_or(u64::MAX).to_le_bytes());
        hasher.update(path.as_bytes());
    }
    Ok(*hasher.finalize().as_bytes())
}

/// The files `programs` said no `tsconfig.json` under the root claims, as it spelled them.
///
/// Sorted by the driver already; sorted again here rather than trusted, because this feeds a
/// cache key and a key that depends on a peer's sort order is a key two runs can disagree on.
///
/// # Errors
///
/// [`ProviderError::Refused`] on an entry that is not a string, on [`fold_programs`]'s own
/// reasoning: this list is a cache-key input, and dropping an entry nobody could read would
/// fold two different answers to the same bytes. It used to be a `filter_map`, which is
/// exactly that — a shorter list, silently, for an answer this could not understand.
fn adhoc_paths(answer: &serde_json::Value) -> Result<Vec<String>, ProviderError> {
    let Some(rows) = answer.get("adhoc") else {
        return Ok(Vec::new());
    };
    let rows = rows.as_array().ok_or_else(|| {
        ProviderError::Refused(format!(
            "`programs` answered an `adhoc` of {rows}, which is not a list of paths"
        ))
    })?;
    let mut paths: Vec<String> = Vec::with_capacity(rows.len());
    for row in rows {
        let path = row.as_str().ok_or_else(|| {
            ProviderError::Refused(format!(
                "`programs` answered an `adhoc` entry {row} that is not a path"
            ))
        })?;
        paths.push(path.to_owned());
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// How many files fell to the ad-hoc program.
///
/// # Errors
///
/// As [`adhoc_paths`].
fn adhoc_count(answer: &serde_json::Value) -> Result<usize, ProviderError> {
    adhoc_paths(answer).map(|paths| paths.len())
}

/// Whether the `tsc` driver has a `ts.ScriptKind` for this path.
///
/// The list is `scriptKindOf`'s in `crates/lanekeep-types/src/tsc/driver.mjs`, and the two are
/// kept in step by hand: an extension the driver types and this refuses is a file the run
/// silently never builds a program for, and one this admits and the driver does not is a
/// request that can only answer nothing.
fn typed_extension(path: &str) -> bool {
    const TYPED: &[&str] = &[
        ".ts", ".tsx", ".mts", ".cts", ".d.ts", ".js", ".jsx", ".mjs", ".cjs",
    ];
    let lowered = path.to_ascii_lowercase();
    TYPED.iter().any(|suffix| lowered.ends_with(suffix))
}

/// One of the seven names [`Primitive::as_str`] renders, or nothing.
///
/// Derived from the enum rather than from a second table, for the reason `TypesProvider::all`
/// gives: a name is accepted exactly when there is a variant for it.
fn decode_primitive(name: &str) -> Option<Primitive> {
    [
        Primitive::Number,
        Primitive::String,
        Primitive::Boolean,
        Primitive::BigInt,
        Primitive::Symbol,
        Primitive::Null,
        Primitive::Undefined,
    ]
    .into_iter()
    .find(|candidate| candidate.as_str() == name)
}

/// `{text, primitive?, union?, symbol?}` as the driver normalizes it.
fn decode_type(answer: &serde_json::Value) -> Option<Type> {
    if let Some(name) = answer.get("primitive").and_then(serde_json::Value::as_str) {
        return decode_primitive(name).map(Type::Primitive);
    }
    if let Some(members) = answer.get("union").and_then(serde_json::Value::as_array) {
        // `Type::union` rather than `Type::Union`: it is what flattens, deduplicates and
        // sorts on the Rust side, so a driver whose sort ever disagreed still produces one
        // canonical answer.
        let decoded: Vec<Type> = members.iter().filter_map(decode_type).collect();
        // A member this cannot read would silently narrow the union into a different type, so
        // a partial decode is `None` rather than a smaller union.
        if decoded.len() != members.len() {
            return None;
        }
        return Type::union(decoded);
    }
    let name = answer.get("text").and_then(serde_json::Value::as_str)?;
    Some(Type::Nominal {
        name: name.to_owned(),
        symbol: answer.get("symbol").and_then(decode_symbol),
    })
}

/// `{name, module?, exported?}` as the driver normalizes it.
fn decode_symbol(answer: &serde_json::Value) -> Option<Symbol> {
    let name = answer.get("name").and_then(serde_json::Value::as_str)?;
    Some(Symbol {
        name: name.to_owned(),
        exported: answer
            .get("exported")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        module: answer
            .get("module")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
    })
}

#[cfg(test)]
#[expect(
    clippy::print_stderr,
    reason = "a test that finds `typescript` absent has to say so on the terminal: the \
              alternative is a suite that reports six passes for six tests it did not run"
)]
mod tests;
