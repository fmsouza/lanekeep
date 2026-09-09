//! The state a `lanekeep server` session holds between requests.
//!
//! Exactly one thing: the type provider. The engine is rebuilt per request, because a rule
//! file or the config can change while a session is open and a server answering from the
//! ruleset it started with would report violations the project no longer has. The provider is
//! the opposite case — it is the expensive state, and it cannot go stale unnoticed, because
//! every item it holds is keyed by path and content hash and it re-probes what it holds
//! before each request answers.
//!
//! # The one thing revalidation cannot repair
//!
//! A `types` block that changed. A provider built under `provider: 'builtin'` answering a
//! request that asked for `tsc` is a *different answer*, not a stale one, and no amount of
//! re-hashing makes it the right one. So the block the provider was built from is held beside
//! it and compared on every request; a difference rebuilds rather than revalidates.
//!
//! A build that fails clears the held provider rather than leaving the previous one in place.
//! Handing back a provider built for a configuration the user has since changed would answer
//! confidently and wrongly, which is worse than the error the failed build already is.

use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use lanekeep_config::{Config, TypesConfig};
use lanekeep_core::FileAccess;
use lanekeep_engine::RunError;
use lanekeep_lang_js::TypeScript;
use lanekeep_types::TypeProvider;

/// One provider, held for the length of a session.
pub(crate) struct SessionProvider {
    /// `None` until the first request, and again after a build fails.
    ///
    /// A `Mutex` rather than a `RefCell` because the LSP loop takes `impl FnMut` and the MCP
    /// `Tools` takes `&mut self`, and a shared `&SessionProvider` across both is the only
    /// shape that lets one session serve either protocol without the state being duplicated.
    held: Mutex<Option<Held>>,
}

/// A provider and the configuration it was built from.
struct Held {
    types: TypesConfig,
    provider: Arc<dyn TypeProvider>,
}

impl SessionProvider {
    /// A session that has not built a provider yet.
    pub(crate) const fn new() -> Self {
        Self {
            held: Mutex::new(None),
        }
    }

    /// The provider for this request, built or reused, and revalidated either way.
    ///
    /// Revalidation happens here, before the engine's `begin_run`. A held builtin provider
    /// keeps its parsed declaration files across requests (`begin_run` clears only the
    /// completeness memo), and `revalidate` is what drops an entry whose bytes moved. The
    /// order decides only how promptly a stale parse is reclaimed, not what the run answers:
    /// `declaration()` compares the file's hash on every access, so a stale entry is never
    /// served whichever ran first.
    ///
    /// # Errors
    ///
    /// The [`RunError`] the build failed with, **in its own variant**. The caller decides what
    /// a failure means, and it cannot decide that from a string: a spent `timeouts.analysis` is
    /// a limit and cancels the request, where a `tsc` command that will not start is something
    /// the capability gate may find nobody asked for. Flattened to a message here, the server
    /// failed every request that `lanekeep check` answered — see `prepare_with_session`.
    pub(crate) fn for_request(
        &self,
        config: &Config,
        project_root: &Path,
    ) -> Result<Arc<dyn TypeProvider>, RunError> {
        self.for_request_with(&config.types, project_root, || {
            lanekeep_engine::provider_for(
                &config.types,
                project_root,
                Some(&TypeScript),
                lanekeep_core::AnalysisBudget::start(config.limits.analysis_timeout),
            )
        })
    }

    /// [`Self::for_request`] with the construction supplied.
    ///
    /// Split out so the rebuild rule has a test that needs neither a project on disk nor a
    /// `tsc` on the machine — the rule is about *when* a provider is built, and a test that
    /// could only observe it by building a real one would be a test of plan 5's sidecar.
    fn for_request_with(
        &self,
        types: &TypesConfig,
        project_root: &Path,
        build: impl FnOnce() -> Result<Arc<dyn TypeProvider>, RunError>,
    ) -> Result<Arc<dyn TypeProvider>, RunError> {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);

        // The held provider saying its own state is gone rather than old. Under `tsc` a
        // breached `timeouts.analysis` kills the sidecar and nothing respawns it, so without
        // this every later request of the session failed with "the sidecar exited without
        // answering" while `lanekeep check` over the same project succeeded — one slow build
        // ending an editor session. A limit cancels the run it breached and nothing after it.
        //
        // Asked once, and only of a provider the configuration still matches: a `types` block
        // that moved is the other reason to rebuild, and it is the older one — a provider built
        // under `builtin` answering a request that asked for `tsc` is a *different answer*, not
        // a stale one, which no revalidation repairs.
        //
        // `needs_rebuild` runs with `held` still locked. Under `tsc` it takes the sidecar's own
        // mutex to call `try_wait`, so what could block this lock is not `try_wait` — it never
        // waits — but acquiring that mutex. Sound only because no request is in flight here: a
        // session serializes callers through `held`, so the sidecar's mutex is never contended
        // by a request already answering one.
        let matched = held.as_ref().is_some_and(|current| &current.types == types);
        let gone = matched && held.as_ref().is_some_and(|c| c.provider.needs_rebuild());
        if !matched || gone {
            // One line, on stderr, for the reason a failed build gets one: the state a session
            // holds is invisible, and a request that suddenly costs a full build with nothing
            // said reads as the editor having hung. Not said for a `types` block that moved —
            // there the user changed something and knows why the next request is slow.
            if gone {
                let _ = writeln!(
                    std::io::stderr(),
                    "lanekeep: the held type provider is gone; building a new one"
                );
            }
            // Cleared *before* the build, so a failure cannot leave the previous
            // configuration's provider reachable.
            *held = None;
            *held = Some(Held {
                types: types.clone(),
                provider: build()?,
            });
        }

        let Some(current) = held.as_ref() else {
            // Not reachable: the branch above either populated it or returned the error.
            return Err(RunError::Provider {
                detail: "no type provider for this session".to_owned(),
            });
        };
        let provider = Arc::clone(&current.provider);
        // Released before the revalidation, which touches the filesystem. Holding it across
        // that would serialize two protocols' requests behind one directory walk for no
        // reason — the provider's own state is behind its own locks.
        drop(held);

        provider.revalidate(&FileAccess::new(project_root));
        Ok(provider)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use lanekeep_config::{TypesConfig, TypesProvider};
    use lanekeep_core::FileAccess;
    use lanekeep_types::{Query, Symbol, Type, TypeProvider};

    use super::*;

    /// Answers nothing and counts revalidations, so the two properties can be told apart:
    /// how often a provider was *built*, and how often the one that was built was asked to
    /// revalidate. A test that only counted builds would pass against a session that never
    /// revalidated at all.
    ///
    /// `gone` is the third: a provider whose own state has died, which is what a breached
    /// `timeouts.analysis` leaves behind under `tsc`. A `Cell` behind an `AtomicBool` so the
    /// test can flip it between two requests without rebuilding the stub.
    #[derive(Default)]
    struct Stub(AtomicUsize, std::sync::atomic::AtomicBool);

    impl TypeProvider for Stub {
        fn type_of(&self, _q: Query<'_>) -> Option<Type> {
            None
        }
        fn symbol_of(&self, _q: Query<'_>) -> Option<Symbol> {
            None
        }
        fn return_type_of(&self, _q: Query<'_>) -> Option<Type> {
            None
        }
        fn is_assignable_to(&self, _q: Query<'_>, _module: &str, _name: &str) -> Option<bool> {
            None
        }
        fn complete(&self, _q: Query<'_>) -> bool {
            false
        }
        fn identity(&self) -> Vec<u8> {
            Vec::new()
        }
        fn revalidate(&self, _files: &FileAccess) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
        fn needs_rebuild(&self) -> bool {
            self.1.load(Ordering::Relaxed)
        }
    }

    impl Stub {
        fn revalidations(&self) -> usize {
            self.0.load(Ordering::Relaxed)
        }
    }

    /// The `RunError` a build refusal carries in these tests.
    fn refusal(detail: &str) -> RunError {
        RunError::Provider {
            detail: detail.to_owned(),
        }
    }

    fn builtin() -> TypesConfig {
        TypesConfig {
            provider: TypesProvider::Builtin,
            command: vec!["node".to_owned()],
            typescript: "./node_modules/typescript".to_owned(),
        }
    }

    fn tsc() -> TypesConfig {
        TypesConfig {
            provider: TypesProvider::Tsc,
            command: vec!["node".to_owned()],
            typescript: "./node_modules/typescript".to_owned(),
        }
    }

    #[test]
    fn an_unchanged_types_block_reuses_the_provider_and_revalidates_it() {
        let session = SessionProvider::new();
        let built = Cell::new(0_usize);
        // A second handle to the same stub, kept alongside the trait object so the test can
        // read the counter without downcasting through `Any` — the session only ever hands
        // back `Arc<dyn TypeProvider>`.
        let stub = Arc::new(Stub::default());
        let make = || {
            built.set(built.get() + 1);
            Ok(Arc::clone(&stub) as Arc<dyn TypeProvider>)
        };

        let root = Path::new(".");
        let first = session
            .for_request_with(&builtin(), root, make)
            .expect("builds");
        let second = session
            .for_request_with(&builtin(), root, make)
            .expect("reuses");

        assert_eq!(built.get(), 1, "the provider is the state that persists");
        assert!(
            Arc::ptr_eq(&first, &second),
            "and it is the same one, not an equal one"
        );
        assert_eq!(
            stub.revalidations(),
            2,
            "revalidated once per request, including the first"
        );
    }

    #[test]
    fn a_changed_types_block_rebuilds_the_provider() {
        // A provider built from a different configuration is a *different answer*, not a
        // stale one, so no amount of revalidation makes it right. The config can change
        // mid-session — that is why the engine is rebuilt per request — and this is the one
        // case where the held state has to go with it.
        let session = SessionProvider::new();
        let built = Cell::new(0_usize);
        let make = || {
            built.set(built.get() + 1);
            Ok(Arc::new(Stub::default()) as Arc<dyn TypeProvider>)
        };

        let root = Path::new(".");
        let first = session
            .for_request_with(&builtin(), root, make)
            .expect("builds");
        let second = session
            .for_request_with(&tsc(), root, make)
            .expect("rebuilds");

        assert_eq!(built.get(), 2, "the types block moved, so the provider did");
        assert!(
            !Arc::ptr_eq(&first, &second),
            "and the old one is not handed out again"
        );
    }

    #[test]
    fn a_failed_build_does_not_leave_the_previous_provider_in_place() {
        // Under `tsc` a build can fail — no Node, no `typescript`. Handing back the provider
        // from the previous config would answer the *old* configuration's questions while
        // reporting success, which is the silent half of the failure this whole task is about.
        let session = SessionProvider::new();
        let root = Path::new(".");
        let _ = session
            .for_request_with(&builtin(), root, || {
                Ok(Arc::new(Stub::default()) as Arc<dyn TypeProvider>)
            })
            .expect("builds");

        let refused = session.for_request_with(&tsc(), root, || Err(refusal("no node")));
        assert_eq!(refused.err(), Some(refusal("no node")));

        let built = Cell::new(0_usize);
        let again = session.for_request_with(&tsc(), root, || {
            built.set(built.get() + 1);
            Ok(Arc::new(Stub::default()) as Arc<dyn TypeProvider>)
        });
        assert!(again.is_ok());
        assert_eq!(
            built.get(),
            1,
            "the next request builds rather than reusing"
        );
    }

    /// A provider that says its own state is gone is built again, and the old one is dropped.
    ///
    /// Under `tsc` a breached `timeouts.analysis` kills the sidecar and nothing respawns it, so
    /// without this every later request of the session failed with "the sidecar exited without
    /// answering" — for the life of the editor — while `lanekeep check` over the same project
    /// spawned one and succeeded. A stub rather than a real sidecar, because the rule under
    /// test is *when* a session rebuilds; that a breach makes a `tsc` provider answer `true` is
    /// pinned where the sidecar is, in `lanekeep-types`.
    #[test]
    fn a_provider_that_says_it_is_gone_is_rebuilt_for_the_next_request() {
        let session = SessionProvider::new();
        let built = Cell::new(0_usize);
        let first_stub = Arc::new(Stub::default());
        let second_stub = Arc::new(Stub::default());

        let root = Path::new(".");
        let first = session
            .for_request_with(&tsc(), root, || {
                built.set(built.get() + 1);
                Ok(Arc::clone(&first_stub) as Arc<dyn TypeProvider>)
            })
            .expect("builds");

        // The breach, between two requests. Nothing about the configuration moved.
        first_stub.1.store(true, Ordering::Relaxed);

        let second = session
            .for_request_with(&tsc(), root, || {
                built.set(built.get() + 1);
                Ok(Arc::clone(&second_stub) as Arc<dyn TypeProvider>)
            })
            .expect("rebuilds rather than handing back a dead provider");

        assert_eq!(
            built.get(),
            2,
            "the request after the breach must build anew"
        );
        assert!(
            !Arc::ptr_eq(&first, &second),
            "the dead provider is handed out again"
        );

        // And it settles: a live provider is reused, so this is not a session that rebuilds on
        // every request.
        let third = session
            .for_request_with(&tsc(), root, || {
                built.set(built.get() + 1);
                Ok(Arc::new(Stub::default()) as Arc<dyn TypeProvider>)
            })
            .expect("reuses");
        assert_eq!(built.get(), 2, "a live provider is the state that persists");
        assert!(Arc::ptr_eq(&second, &third));
    }

    /// A build failure keeps its variant, because the caller's decision turns on which it is.
    ///
    /// `prepare_with_session` cancels the request for a spent `timeouts.analysis` and falls
    /// through to the engine's own provider path for anything else — which is what makes
    /// `lanekeep server` answer the requests `lanekeep check` answers. Flattened to a string
    /// here, that distinction cannot be made at all.
    #[test]
    fn a_failed_build_keeps_the_variant_the_caller_decides_on() {
        let session = SessionProvider::new();
        let timeout = RunError::AnalysisTimeout {
            detail: "spent".to_owned(),
        };
        let refused = session.for_request_with(&tsc(), Path::new("."), || Err(timeout.clone()));
        assert_eq!(refused.err(), Some(timeout));
    }
}
