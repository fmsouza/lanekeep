package main

import (
	"archive/tar"
	"archive/zip"
	"bytes"
	"compress/gzip"
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
)

// A tar.gz shaped like a release archive: a directory holding the binary beside the README
// and both licenses, which is what the real one contains.
func tarGz(t *testing.T, binary []byte) []byte {
	t.Helper()
	var buffer bytes.Buffer
	compressor := gzip.NewWriter(&buffer)
	writer := tar.NewWriter(compressor)

	for _, file := range []struct {
		name string
		body []byte
	}{
		{"lanekeep-9.9.9-aarch64-apple-darwin/README.md", []byte("readme")},
		{"lanekeep-9.9.9-aarch64-apple-darwin/lanekeep", binary},
		{"lanekeep-9.9.9-aarch64-apple-darwin/LICENSE-MIT", []byte("mit")},
	} {
		if err := writer.WriteHeader(&tar.Header{
			Name: file.name, Mode: 0o755, Size: int64(len(file.body)), Typeflag: tar.TypeReg,
		}); err != nil {
			t.Fatalf("writing the fixture: %v", err)
		}
		if _, err := writer.Write(file.body); err != nil {
			t.Fatalf("writing the fixture: %v", err)
		}
	}
	if err := writer.Close(); err != nil {
		t.Fatalf("writing the fixture: %v", err)
	}
	if err := compressor.Close(); err != nil {
		t.Fatalf("writing the fixture: %v", err)
	}
	return buffer.Bytes()
}

func zipped(t *testing.T, binary []byte) []byte {
	t.Helper()
	var buffer bytes.Buffer
	writer := zip.NewWriter(&buffer)
	entry, err := writer.Create("lanekeep-9.9.9-x86_64-pc-windows-msvc/lanekeep.exe")
	if err != nil {
		t.Fatalf("writing the fixture: %v", err)
	}
	if _, err := entry.Write(binary); err != nil {
		t.Fatalf("writing the fixture: %v", err)
	}
	if err := writer.Close(); err != nil {
		t.Fatalf("writing the fixture: %v", err)
	}
	return buffer.Bytes()
}

// --- the platform table ------------------------------------------------------------------

// Every platform the release builds, and nothing else. A triple here that the release does
// not produce is a 404 at first run; one missing is a platform told to build from source
// when a binary exists for it.
func TestTriplesMatchTheReleaseMatrix(t *testing.T) {
	expected := map[string]string{
		"darwin/arm64":  "aarch64-apple-darwin",
		"linux/amd64":   "x86_64-unknown-linux-gnu",
		"linux/arm64":   "aarch64-unknown-linux-gnu",
		"windows/amd64": "x86_64-pc-windows-msvc",
	}
	if len(triples) != len(expected) {
		t.Fatalf("the table has %d platforms, the release builds %d", len(triples), len(expected))
	}
	for platform, triple := range expected {
		if triples[platform] != triple {
			t.Errorf("%s: got %q, want %q", platform, triples[platform], triple)
		}
	}
}

func TestAnUnsupportedPlatformIsNamedRatherThanFetched(t *testing.T) {
	// Intel macOS is the real case: no binary is built for it, and a 404 would read as the
	// tool being broken rather than as a platform that is not shipped.
	if _, ok := triples["darwin/amd64"]; ok {
		t.Fatal("darwin/amd64 has no prebuilt binary, so it must not be in the table")
	}
}

// --- the escape hatch --------------------------------------------------------------------

func TestAnExplicitBinaryIsUsedWithoutFetching(t *testing.T) {
	binary := filepath.Join(t.TempDir(), "lanekeep")
	if err := os.WriteFile(binary, []byte("#!/bin/sh\n"), 0o755); err != nil {
		t.Fatalf("writing the fixture: %v", err)
	}
	t.Setenv("LANEKEEP_BINARY", binary)

	// `releases` is left pointing at github.com. If resolve reached the network this would
	// be the test that noticed.
	got, err := resolve()
	if err != nil {
		t.Fatalf("resolve: %v", err)
	}
	if got != binary {
		t.Errorf("got %q, want %q", got, binary)
	}
}

func TestAnExplicitBinaryThatIsMissingIsAnError(t *testing.T) {
	t.Setenv("LANEKEEP_BINARY", filepath.Join(t.TempDir(), "absent"))
	if _, err := resolve(); err == nil {
		t.Fatal("a LANEKEEP_BINARY that does not exist has to fail loudly")
	}
}

// --- verification ------------------------------------------------------------------------

// A release server serving one archive and its SHA256SUMS.
func server(t *testing.T, name string, archive []byte, sum string) *httptest.Server {
	t.Helper()
	if sum == "" {
		digest := sha256.Sum256(archive)
		sum = hex.EncodeToString(digest[:])
	}
	return httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch {
		case strings.HasSuffix(r.URL.Path, "/SHA256SUMS"):
			fmt.Fprintf(w, "%s  %s\n", sum, name)
		case strings.HasSuffix(r.URL.Path, name):
			w.Write(archive)
		default:
			http.NotFound(w, r)
		}
	}))
}

func TestAFetchedArchiveIsUnpackedAndMadeExecutable(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("the tar.gz path is not the one Windows takes")
	}
	archive := tarGz(t, []byte("the real binary"))
	name := "lanekeep-9.9.9-" + triples[runtime.GOOS+"/"+runtime.GOARCH] + ".tar.gz"
	if _, ok := triples[runtime.GOOS+"/"+runtime.GOARCH]; !ok {
		t.Skip("no prebuilt binary for this platform, so there is nothing to fetch")
	}

	s := server(t, name, archive, "")
	defer s.Close()
	releases = s.URL
	defer func() { releases = "https://github.com/fmsouza/lanekeep/releases/download" }()

	destination := filepath.Join(t.TempDir(), "lanekeep")
	if err := fetch("v9.9.9", triples[runtime.GOOS+"/"+runtime.GOARCH], destination); err != nil {
		t.Fatalf("fetch: %v", err)
	}

	content, err := os.ReadFile(destination)
	if err != nil {
		t.Fatalf("reading what was installed: %v", err)
	}
	if string(content) != "the real binary" {
		t.Errorf("got %q, want the binary from inside the archive", content)
	}

	// The bit that makes the whole thing work, and the one the npm lane shipped wrong.
	info, err := os.Stat(destination)
	if err != nil {
		t.Fatalf("stat: %v", err)
	}
	if info.Mode().Perm()&0o111 == 0 {
		t.Errorf("mode is %v, which is not executable", info.Mode().Perm())
	}
}

func TestAnArchiveThatFailsItsChecksumIsRefused(t *testing.T) {
	triple, ok := triples[runtime.GOOS+"/"+runtime.GOARCH]
	if !ok {
		t.Skip("no prebuilt binary for this platform")
	}
	extension := ".tar.gz"
	archive := tarGz(t, []byte("tampered"))
	if runtime.GOOS == "windows" {
		extension = ".zip"
		archive = zipped(t, []byte("tampered"))
	}
	name := "lanekeep-9.9.9-" + triple + extension

	// A checksum for something else entirely.
	s := server(t, name, archive, strings.Repeat("0", 64))
	defer s.Close()
	releases = s.URL
	defer func() { releases = "https://github.com/fmsouza/lanekeep/releases/download" }()

	destination := filepath.Join(t.TempDir(), "lanekeep")
	err := fetch("v9.9.9", triple, destination)
	if err == nil {
		t.Fatal("an archive that does not match its checksum must not be installed")
	}
	if !strings.Contains(err.Error(), "checksum") {
		t.Errorf("the error should say why: %v", err)
	}
	// Nothing installed is the part that matters. Reporting the mismatch and running it
	// anyway would be worse than not checking.
	if _, err := os.Stat(destination); err == nil {
		t.Error("a binary was installed despite failing verification")
	}
}

func TestAMissingChecksumFileIsAnError(t *testing.T) {
	triple, ok := triples[runtime.GOOS+"/"+runtime.GOARCH]
	if !ok {
		t.Skip("no prebuilt binary for this platform")
	}
	s := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if strings.HasSuffix(r.URL.Path, "/SHA256SUMS") {
			http.NotFound(w, r)
			return
		}
		w.Write([]byte("whatever"))
	}))
	defer s.Close()
	releases = s.URL
	defer func() { releases = "https://github.com/fmsouza/lanekeep/releases/download" }()

	// Unverifiable is refused, not waved through.
	if err := fetch("v9.9.9", triple, filepath.Join(t.TempDir(), "lanekeep")); err == nil {
		t.Fatal("without SHA256SUMS there is nothing to verify against, which must fail")
	}
}

// --- unpacking ----------------------------------------------------------------------------

func TestTheBinaryIsFoundAmongTheOtherArchiveEntries(t *testing.T) {
	got, err := unpack(tarGz(t, []byte("binary")), ".tar.gz")
	if err != nil {
		t.Fatalf("unpack: %v", err)
	}
	if string(got) != "binary" {
		t.Errorf("got %q, want the binary rather than the README beside it", got)
	}
}

func TestTheWindowsBinaryIsFoundInAZip(t *testing.T) {
	got, err := unpack(zipped(t, []byte("binary")), ".zip")
	if err != nil {
		t.Fatalf("unpack: %v", err)
	}
	if string(got) != "binary" {
		t.Errorf("got %q, want the binary", got)
	}
}

func TestAnArchiveWithNoBinaryIsAnError(t *testing.T) {
	var buffer bytes.Buffer
	compressor := gzip.NewWriter(&buffer)
	writer := tar.NewWriter(compressor)
	_ = writer.WriteHeader(&tar.Header{Name: "d/README.md", Mode: 0o644, Size: 1, Typeflag: tar.TypeReg})
	_, _ = writer.Write([]byte("x"))
	_ = writer.Close()
	_ = compressor.Close()

	if _, err := unpack(buffer.Bytes(), ".tar.gz"); err == nil {
		t.Fatal("an archive with no lanekeep in it must not unpack to something")
	}
}

// --- exit codes ---------------------------------------------------------------------------

// lanekeep's exit code is part of its contract — 1 for violations, 2 for a limit breach — so
// collapsing a failure into a generic non-zero would break every script that reads it.
func TestTheExitCodeIsPassedThrough(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("the shell fixture is not portable to Windows")
	}
	script := filepath.Join(t.TempDir(), "fake")
	if err := os.WriteFile(script, []byte("#!/bin/sh\nexit 7\n"), 0o755); err != nil {
		t.Fatalf("writing the fixture: %v", err)
	}
	if got := run(script, nil); got != 7 {
		t.Errorf("got %d, want 7", got)
	}
}

func TestASuccessfulRunReportsZero(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("the shell fixture is not portable to Windows")
	}
	script := filepath.Join(t.TempDir(), "fake")
	if err := os.WriteFile(script, []byte("#!/bin/sh\nexit 0\n"), 0o755); err != nil {
		t.Fatalf("writing the fixture: %v", err)
	}
	if got := run(script, nil); got != 0 {
		t.Errorf("got %d, want 0", got)
	}
}

// --- version ------------------------------------------------------------------------------

// Run from a checkout there is no module version, and guessing one would fetch a release
// that has nothing to do with this code. The message has to say what to do instead.
func TestASourceCheckoutSaysWhatToDoInstead(t *testing.T) {
	_, err := version()
	if err == nil {
		t.Skip("this build carries a module version, so there is no checkout case to test")
	}
	if !strings.Contains(err.Error(), "LANEKEEP_BINARY") {
		t.Errorf("the error should point at the way out: %v", err)
	}
}
