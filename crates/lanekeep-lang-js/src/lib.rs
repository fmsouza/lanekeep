//! TypeScript and JavaScript language support for lanekeep.
//!
//! tree-sitter grammars for TypeScript, TSX, JavaScript and JSX, plus the binding resolver
//! backing the import-resolution host functions.
//!
//! Binding resolution is the light semantic layer that purely syntactic matching gets wrong:
//! it is what makes `import { makeStyles as ms }` resolve correctly.
