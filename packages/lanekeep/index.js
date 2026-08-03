// The runtime half of the authoring package.
//
// Rules never execute in Node — lanekeep evaluates them in its own sandbox, where `lanekeep`
// resolves to a host module rather than to this file. These exports exist so that a tool
// which *does* load a rule under Node (a bundler, a test runner, an editor's type server
// following the import) finds something coherent instead of a missing module.
//
// Identity functions, which is also what they are inside the sandbox: their entire job is to
// give the compiler a place to check the object against a type.
'use strict'

const identity = (value) => value

module.exports = { defineRule: identity, defineConfig: identity }
module.exports.default = module.exports
