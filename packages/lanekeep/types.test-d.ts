/**
 * Generated from `COMPONENT_RULES` — do not edit by hand. Run `just generate-builtin-subpaths`.
 *
 * A component built-in has no module to import, so each `@ts-expect-error` below must fire.
 * A module built-in imports as `Rule & ((options?) => Rule)`.
 */

// @ts-expect-error no-context-in-struct is a component, not importable
import noContextInStruct from 'lanekeep/no-context-in-struct'

// @ts-expect-error no-glob-import is a component, not importable
import noGlobImport from 'lanekeep/no-glob-import'

// @ts-expect-error no-package-init is a component, not importable
import noPackageInit from 'lanekeep/no-package-init'

// @ts-expect-error no-unwrap is a component, not importable
import noUnwrap from 'lanekeep/no-unwrap'

import noBroadExcept from 'lanekeep/no-broad-except'
import noCircularImports from 'lanekeep/no-circular-imports'
import noDefaultExport from 'lanekeep/no-default-export'
import noMutableDefaultArgument from 'lanekeep/no-mutable-default-argument'
import noRestrictedImports from 'lanekeep/no-restricted-imports'
import noUnusedExports from 'lanekeep/no-unused-exports'

void noBroadExcept
void noCircularImports
void noDefaultExport
void noMutableDefaultArgument
void noRestrictedImports
void noUnusedExports
