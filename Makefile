# Release builds. Every file lands in dist/ as Pastezo-<version>-<os>-<arch>.<ext>.
#
#   make mac       macOS: arm64 (Apple Silicon, 11+) and x64 (Intel, 10.15+) DMGs — on a Mac
#   make linux     Linux: this machine's architecture, tar.gz — on Linux
#   make dist      what this machine can build, plus dist/SHA256SUMS
#   make clean     empties dist/
#   make release   a new version from the change files (.changeset/*.md): knope bumps the
#                  version (Cargo.toml, Cargo.lock), moves the changes into CHANGELOG.md and
#                  commits "Release v<version>". Nothing is pushed: after `git push`,
#                  .github/workflows/release.yml builds every OS on GitHub's machines and
#                  makes the tag and the release with the files. Needs knope.
#
# Windows (x64 and arm64): apps/pastezo/scripts/bundle-windows.ps1 -Arch x64|arm64, on Windows.

VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
DIST := dist
SCRIPTS := apps/pastezo/scripts

.PHONY: mac linux dist checksums clean release

mac:
	mkdir -p $(DIST)
	$(SCRIPTS)/bundle-macos.sh aarch64
	$(SCRIPTS)/bundle-macos.sh x86_64
	cp target/aarch64-apple-darwin/release/bundle/Pastezo_$(VERSION)_aarch64.dmg $(DIST)/Pastezo-$(VERSION)-macos-arm64.dmg
	cp target/x86_64-apple-darwin/release/bundle/Pastezo_$(VERSION)_x86_64.dmg $(DIST)/Pastezo-$(VERSION)-macos-x64.dmg

linux:
	mkdir -p $(DIST)
	$(SCRIPTS)/bundle-linux.sh
	arch=$$(uname -m); case $$arch in x86_64) name=x64 ;; aarch64) name=arm64 ;; *) name=$$arch ;; esac; \
	cp target/release/bundle/Pastezo_$(VERSION)_linux_$$arch.tar.gz $(DIST)/Pastezo-$(VERSION)-linux-$$name.tar.gz

dist: $(if $(filter Darwin,$(shell uname -s)),mac,linux) checksums

checksums:
	cd $(DIST) && rm -f SHA256SUMS && shasum -a 256 Pastezo-* > SHA256SUMS

clean:
	rm -rf $(DIST)

release:
	@ls .changeset/*.md >/dev/null 2>&1 || { echo "no change files in .changeset/: nothing to release"; exit 1; }
	knope release
	@git log -1 --format='%s — push it: git push origin HEAD:main'
