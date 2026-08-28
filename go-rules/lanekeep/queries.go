package lanekeep

import (
	"github.com/fmsouza/lanekeep/go-rules/internal/lanekeep/host/types"
	"go.bytecodealliance.org/cm"
)

// Queries is the convenience constructor for a single-language rule's metadata queries: one
// [types.QueryFor] for the language and query given. A rule targeting several grammars writes
// the slice out by hand, since each grammar has its own query — the world carries one query
// per language, and the host refuses a declared language with no entry of its own.
func Queries(language, query string) cm.List[types.QueryFor] {
	return cm.ToList([]types.QueryFor{{Language: language, Query: query}})
}
