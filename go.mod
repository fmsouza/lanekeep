// The Go module exists so a Go project can pin lanekeep in `go.mod` alongside every other
// tool it depends on, rather than being told to install it out of band:
//
//	go get -tool github.com/fmsouza/lanekeep/cmd/lanekeep
//	go tool lanekeep check ./...
//
// It lives at the repository root deliberately. Go has no registry — a module version *is* a
// git tag — so putting the module here means the `v*` tags the release already pushes are its
// versions, and there is no separate publish step that could drift from the others.
//
// `go 1.21` rather than 1.24: the `tool` directive a consumer uses arrived in 1.24, but
// nothing in this module needs it, and declaring the newer version would refuse to build for
// people who could otherwise `go install` it perfectly well.
module github.com/fmsouza/lanekeep

go 1.21
