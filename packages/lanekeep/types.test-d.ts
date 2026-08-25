/**
 * Generated from `COMPONENT_RULES` — do not edit by hand. Run `just generate-builtin-subpaths`.
 *
 * A component built-in has no module to import, so each `@ts-expect-error` below must fire.
 * A module built-in imports as `Rule & ((options?) => Rule)`.
 */

// @ts-expect-error no-circular-imports is a component, not importable
import noCircularImports from 'lanekeep/no-circular-imports'

// @ts-expect-error no-context-in-struct is a component, not importable
import noContextInStruct from 'lanekeep/no-context-in-struct'

// @ts-expect-error no-default-export is a component, not importable
import noDefaultExport from 'lanekeep/no-default-export'

// @ts-expect-error no-glob-import is a component, not importable
import noGlobImport from 'lanekeep/no-glob-import'

// @ts-expect-error no-package-init is a component, not importable
import noPackageInit from 'lanekeep/no-package-init'

// @ts-expect-error no-restricted-imports is a component, not importable
import noRestrictedImports from 'lanekeep/no-restricted-imports'

// @ts-expect-error no-unused-exports is a component, not importable
import noUnusedExports from 'lanekeep/no-unused-exports'

// @ts-expect-error no-unwrap is a component, not importable
import noUnwrap from 'lanekeep/no-unwrap'

import noBroadExcept from 'lanekeep/no-broad-except'
import noMutableDefaultArgument from 'lanekeep/no-mutable-default-argument'

void noBroadExcept
void noMutableDefaultArgument
