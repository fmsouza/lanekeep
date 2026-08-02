// Command lanekeep runs the lanekeep binary for the current platform.
//
// lanekeep is written in Rust. Go's tooling can only install and pin things written in Go —
// `go install` compiles a Go package, and the `tool` directive in go.mod records one — so a
// Go project has no way to depend on the real binary directly. This command is the adapter:
// a Go program thin enough to be uninteresting, whose only job is to put the real one on the
// other side of `go tool lanekeep`.
//
// It is the same shape as the npm launcher and exists for the same reason, with one
// difference. npm can express "install exactly the package for this platform" through
// optionalDependencies; Go cannot, and a module carrying every platform's binary would be
// tens of megabytes committed to git forever. So the binary is fetched on first use and
// cached, rather than shipped inside the module.
//
// # What this trusts
//
// The module itself is verified by Go's checksum database, so the code you are reading is
// the code that runs. The binary it fetches is verified against the `SHA256SUMS` published
// with the same GitHub release — which is a weaker guarantee than Homebrew's, where the
// checksum is pinned in the formula out of band, because here the archive and the checksums
// come from one place. A release's checksums cannot be known when its tag is created, which
// is what rules out embedding them in this file.
//
// Two ways to avoid the fetch entirely, both first-class rather than workarounds:
//
//   - Set LANEKEEP_BINARY to an already-installed lanekeep. Nothing is downloaded and
//     nothing is verified, because the choice was made by whoever set it.
//   - Install lanekeep by any other route and put it on PATH — Homebrew, the releases page,
//     cargo, npm, pip. This command finds a cached binary first and only then reaches out.
package main

import (
	"archive/tar"
	"archive/zip"
	"compress/gzip"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"runtime/debug"
	"strings"
	"time"
)

// The platform triples the release builds, keyed by Go's own names for them.
//
// Kept in step with the matrix in .github/workflows/release.yml and the tables in the other
// packaging scripts. A platform missing here is one this command refuses by name rather than
// fetching a 404 and reporting whatever that looks like.
var triples = map[string]string{
	"darwin/arm64":  "aarch64-apple-darwin",
	"linux/amd64":   "x86_64-unknown-linux-gnu",
	"linux/arm64":   "aarch64-unknown-linux-gnu",
	"windows/amd64": "x86_64-pc-windows-msvc",
}

// A var rather than a const so the tests can point it at a local server. Nothing else
// assigns it, and no flag or environment variable exposes it — a redirectable download
// origin is exactly the thing an attacker would want, and the tests do not need it to be
// reachable from outside this package.
var releases = "https://github.com/fmsouza/lanekeep/releases/download"

func main() {
	binary, err := resolve()
	if err != nil {
		fmt.Fprintf(os.Stderr, "lanekeep: %v\n", err)
		os.Exit(2)
	}
	os.Exit(run(binary, os.Args[1:]))
}

// resolve finds a lanekeep binary to run, fetching one if there is none cached.
func resolve() (string, error) {
	// An explicit choice beats everything else, including the version this module was
	// pinned to. Someone who sets it has a binary they mean to use.
	if binary := os.Getenv("LANEKEEP_BINARY"); binary != "" {
		if _, err := os.Stat(binary); err != nil {
			return "", fmt.Errorf("LANEKEEP_BINARY is set to %s, which is not readable: %w", binary, err)
		}
		return binary, nil
	}

	version, err := version()
	if err != nil {
		return "", err
	}

	triple, ok := triples[runtime.GOOS+"/"+runtime.GOARCH]
	if !ok {
		// Named rather than attempted. A 404 from a URL nobody meant to build reads like
		// the tool is broken; this reads like the platform it is.
		return "", fmt.Errorf(
			"no prebuilt binary for %s/%s\n"+
				"       build one with `cargo install lanekeep-cli`, then set LANEKEEP_BINARY to it",
			runtime.GOOS, runtime.GOARCH)
	}

	cached, err := cachePath(version, triple)
	if err != nil {
		return "", err
	}
	if _, err := os.Stat(cached); err == nil {
		return cached, nil
	}

	if err := fetch(version, triple, cached); err != nil {
		return "", err
	}
	return cached, nil
}

// version reports which lanekeep to run, taken from the module version this command was
// built at.
//
// Read from the build info rather than written down: a committed version is one more thing
// to forget, and forgetting it fetches the previous release's binary under this release's
// name. The npm launcher avoids the same trap by having its version rewritten from the tag.
func version() (string, error) {
	info, ok := debug.ReadBuildInfo()
	if !ok {
		return "", errors.New("no build information, so there is no way to tell which lanekeep to run")
	}

	v := info.Main.Version
	// `go run ./cmd/lanekeep` inside a checkout reports this: there is no module version
	// because nothing resolved one.
	if v == "" || v == "(devel)" {
		return "", errors.New(
			"this build has no module version, which happens when it is run from a source checkout\n" +
				"       set LANEKEEP_BINARY to the lanekeep you want, or install with\n" +
				"       `go get -tool github.com/fmsouza/lanekeep/cmd/lanekeep@latest`")
	}
	return v, nil
}

// cachePath is where a given version's binary lives once fetched.
//
// Keyed by version, so upgrading fetches afresh rather than running the old binary, and two
// projects pinned to different versions do not fight over one path.
func cachePath(version, triple string) (string, error) {
	base, err := os.UserCacheDir()
	if err != nil {
		return "", fmt.Errorf("no user cache directory to install into: %w", err)
	}
	name := "lanekeep"
	if runtime.GOOS == "windows" {
		name += ".exe"
	}
	return filepath.Join(base, "lanekeep", version+"-"+triple, name), nil
}

// fetch downloads the release archive, verifies it, and unpacks the binary to destination.
func fetch(version, triple, destination string) error {
	number := strings.TrimPrefix(version, "v")
	extension := ".tar.gz"
	if runtime.GOOS == "windows" {
		extension = ".zip"
	}
	name := fmt.Sprintf("lanekeep-%s-%s%s", number, triple, extension)
	url := fmt.Sprintf("%s/%s/%s", releases, version, name)

	fmt.Fprintf(os.Stderr, "lanekeep: fetching %s\n", name)

	archive, err := download(url)
	if err != nil {
		return err
	}

	want, err := checksum(version, name)
	if err != nil {
		return err
	}
	got := sha256.Sum256(archive)
	if hex.EncodeToString(got[:]) != want {
		// Refused rather than run. Whatever this is, it is not the release it claims to be.
		return fmt.Errorf("%s does not match its published checksum\n"+
			"       expected %s\n       got      %s", name, want, hex.EncodeToString(got[:]))
	}

	binary, err := unpack(archive, extension)
	if err != nil {
		return err
	}

	return install(binary, destination)
}

// checksum reads the expected hash of one archive from the release's SHA256SUMS.
func checksum(version, name string) (string, error) {
	sums, err := download(fmt.Sprintf("%s/%s/SHA256SUMS", releases, version))
	if err != nil {
		return "", fmt.Errorf("no SHA256SUMS for %s, so the download cannot be verified: %w", version, err)
	}
	for _, line := range strings.Split(string(sums), "\n") {
		fields := strings.Fields(line)
		if len(fields) == 2 && fields[1] == name {
			return fields[0], nil
		}
	}
	return "", fmt.Errorf("SHA256SUMS for %s does not list %s", version, name)
}

func download(url string) ([]byte, error) {
	client := &http.Client{Timeout: 5 * time.Minute}
	response, err := client.Get(url)
	if err != nil {
		return nil, fmt.Errorf("fetching %s: %w", url, err)
	}
	defer response.Body.Close()

	if response.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("fetching %s: %s", url, response.Status)
	}
	return io.ReadAll(response.Body)
}

// unpack pulls the lanekeep binary out of a release archive.
func unpack(archive []byte, extension string) ([]byte, error) {
	if extension == ".zip" {
		return fromZip(archive)
	}
	return fromTarGz(archive)
}

func fromTarGz(archive []byte) ([]byte, error) {
	decompressed, err := gzip.NewReader(strings.NewReader(string(archive)))
	if err != nil {
		return nil, fmt.Errorf("reading the archive: %w", err)
	}
	defer decompressed.Close()

	reader := tar.NewReader(decompressed)
	for {
		header, err := reader.Next()
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			return nil, fmt.Errorf("reading the archive: %w", err)
		}
		// The archive holds a directory with the binary, the README and both licenses.
		if header.Typeflag == tar.TypeReg && filepath.Base(header.Name) == "lanekeep" {
			return io.ReadAll(reader)
		}
	}
	return nil, errors.New("the archive contains no lanekeep binary")
}

func fromZip(archive []byte) ([]byte, error) {
	reader, err := zip.NewReader(strings.NewReader(string(archive)), int64(len(archive)))
	if err != nil {
		return nil, fmt.Errorf("reading the archive: %w", err)
	}
	for _, file := range reader.File {
		if filepath.Base(file.Name) == "lanekeep.exe" {
			opened, err := file.Open()
			if err != nil {
				return nil, fmt.Errorf("reading the archive: %w", err)
			}
			defer opened.Close()
			return io.ReadAll(opened)
		}
	}
	return nil, errors.New("the archive contains no lanekeep binary")
}

// install writes the binary to its cache path, executable.
//
// Written to a temporary file and renamed, because two `go tool lanekeep` invocations can
// race — a parallel build, or a developer and their editor. A rename is atomic, so the worst
// case is that one download is wasted rather than that a half-written binary is executed.
func install(binary []byte, destination string) error {
	if err := os.MkdirAll(filepath.Dir(destination), 0o755); err != nil {
		return fmt.Errorf("creating the cache directory: %w", err)
	}

	temporary, err := os.CreateTemp(filepath.Dir(destination), ".lanekeep-*")
	if err != nil {
		return fmt.Errorf("creating the cache directory: %w", err)
	}
	defer os.Remove(temporary.Name())

	if _, err := temporary.Write(binary); err != nil {
		temporary.Close()
		return fmt.Errorf("writing the binary: %w", err)
	}
	if err := temporary.Close(); err != nil {
		return fmt.Errorf("writing the binary: %w", err)
	}
	// Before the rename, so the binary is never visible at its final path without the
	// executable bit — which is the state a racing process would try to run.
	if err := os.Chmod(temporary.Name(), 0o755); err != nil {
		return fmt.Errorf("making the binary executable: %w", err)
	}
	if err := os.Rename(temporary.Name(), destination); err != nil {
		return fmt.Errorf("installing the binary: %w", err)
	}
	return nil
}

// run executes the binary and reports its exit code.
func run(binary string, args []string) int {
	command := exec.Command(binary, args...)
	command.Stdin = os.Stdin
	command.Stdout = os.Stdout
	command.Stderr = os.Stderr

	if err := command.Run(); err != nil {
		var exit *exec.ExitError
		// lanekeep's exit code is part of its contract — 1 for violations, 2 for a limit
		// breach — so it is passed through rather than collapsed into a generic failure.
		if errors.As(err, &exit) {
			return exit.ExitCode()
		}
		fmt.Fprintf(os.Stderr, "lanekeep: %v\n", err)
		return 2
	}
	return 0
}
