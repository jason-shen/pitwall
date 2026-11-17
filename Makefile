# Build and publish Pitwall for macOS: a universal Pitwall.app, zipped and
# attached to a GitHub release, and a Homebrew cask in your tap.
#
#   make tools      Rust targets for both Mac architectures
#   make test       tests and clippy
#   make icon       Pitwall.icns and the window icon, from assets/*.svg
#   make app        dist/Pitwall.app (universal, signed)
#   make install    build for this Mac only, into /Applications, CLI on PATH
#   make dist       the zip and its checksum
#   make dist-linux on Linux: a tarball for this machine's architecture
#   make release    test, build, and publish vVERSION on GitHub
#   make cask       point the Homebrew cask at that release
#   make publish    release, then cask
#
# The version comes from Cargo.toml. The release goes on a tag you've pushed
# yourself; this never makes one. Pushing the tag also starts
# .github/workflows/release.yml, which builds macOS and Linux and publishes
# the release and cask itself; `make publish` is the same thing done here. Set DEVELOPER_ID to sign with your
# certificate and NOTARY_PROFILE (a `xcrun notarytool store-credentials`
# profile) to notarize; without them the app is signed ad hoc.

VERSION   ?= $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
TAG       := v$(VERSION)
REPO      ?= jason-shen/pitwall
TAP       ?= jason-shen/homebrew-tap
TARGETS   ?= aarch64-apple-darwin x86_64-apple-darwin
DEST      ?= /Applications
BUNDLE_ID ?= nz.pitwall.app
DEVELOPER_ID   ?=
NOTARY_PROFILE ?=

HOST := $(shell rustc -vV | sed -n 's/^host: //p')
DIST := dist
APP  := $(DIST)/Pitwall.app
ICON := assets/Pitwall.icns
ZIP  := $(DIST)/Pitwall-$(VERSION)-macos.zip
BINS := pitwall pitwall-gui
# `brew install --cask jason-shen/tap/pitwall` for jason-shen/homebrew-tap.
TAP_NAME := $(subst homebrew-,,$(TAP))
# 1.2.0-beta.1 is a prerelease: on GitHub, but not in the cask.
PRERELEASE := $(findstring -,$(VERSION))

.PHONY: help tools test icon build app install dist dist-linux check release cask publish clean

help:
	@sed -n 's/^#   //p' Makefile

tools:
	rustup target add $(TARGETS)

test:
	cargo test --workspace --locked
	cargo clippy --workspace --all-targets --locked

# Every size macOS asks for. 16 and 32 pixels come from pitwall-small.svg,
# drawn to stay readable that small. The results are committed, so building
# the app doesn't need rsvg-convert; only changing the artwork does.
ICON_SIZES := 16:icon_16x16:small 32:icon_16x16@2x:small 32:icon_32x32:small \
	64:icon_32x32@2x:full 128:icon_128x128:full 256:icon_128x128@2x:full \
	256:icon_256x256:full 512:icon_256x256@2x:full 512:icon_512x512:full \
	1024:icon_512x512@2x:full

icon:
	@command -v rsvg-convert >/dev/null || { echo "needs rsvg-convert: brew install librsvg"; exit 1; }
	rm -rf $(DIST)/Pitwall.iconset
	mkdir -p $(DIST)/Pitwall.iconset
	@set -e; for spec in $(ICON_SIZES); do \
		px=$${spec%%:*}; rest=$${spec#*:}; name=$${rest%%:*}; art=$${rest#*:}; \
		svg=assets/pitwall.svg; [ "$$art" = small ] && svg=assets/pitwall-small.svg; \
		rsvg-convert -w $$px -h $$px $$svg -o $(DIST)/Pitwall.iconset/$$name.png; \
	done
	iconutil -c icns $(DIST)/Pitwall.iconset -o $(ICON)
	rsvg-convert -w 256 -h 256 assets/pitwall.svg -o assets/pitwall-256.png

# One binary per architecture, then lipo puts them together.
build:
	@for t in $(TARGETS); do \
		cargo build --release --locked --target $$t -p pitwall -p pitwall-gui || exit 1; \
	done
	@mkdir -p $(DIST)/bin
	@for b in $(BINS); do \
		lipo -create $(foreach t,$(TARGETS),target/$(t)/release/$$b) -output $(DIST)/bin/$$b || exit 1; \
	done
	@lipo -info $(DIST)/bin/pitwall-gui

# Apple Silicon won't run an unsigned binary; ad hoc is the least it takes.
# The CLI is signed first, since signing the app seals what's inside it.
app: build
	rm -rf $(APP)
	mkdir -p $(APP)/Contents/MacOS $(APP)/Contents/Resources
	cp $(addprefix $(DIST)/bin/,$(BINS)) $(APP)/Contents/MacOS/
	cp $(ICON) $(APP)/Contents/Resources/Pitwall.icns
	sed -e 's/@VERSION@/$(VERSION)/g' -e 's/@BUNDLE_ID@/$(BUNDLE_ID)/g' \
		scripts/Info.plist > $(APP)/Contents/Info.plist
ifeq ($(DEVELOPER_ID),)
	codesign --force --sign - $(APP)/Contents/MacOS/pitwall
	codesign --force --sign - $(APP)
else
	codesign --force --options runtime --timestamp --sign "$(DEVELOPER_ID)" $(APP)/Contents/MacOS/pitwall
	codesign --force --options runtime --timestamp --sign "$(DEVELOPER_ID)" $(APP)
endif
	codesign --verify --strict $(APP)

# Just this Mac's architecture: quicker, and no second Rust target needed.
install: TARGETS := $(HOST)
install: app
	@mkdir -p "$(DEST)" 2>/dev/null; test -w "$(DEST)" || { echo "can't write to $(DEST); run it as an admin user, or pick a folder: make install DEST=~/Applications"; exit 1; }
	rm -rf "$(DEST)/Pitwall.app"
	cp -R $(APP) "$(DEST)/"
	@if [ -d "$(HOME)/.local/bin" ]; then \
		ln -sf "$(DEST)/Pitwall.app/Contents/MacOS/pitwall" "$(HOME)/.local/bin/pitwall"; \
		echo "pitwall linked into ~/.local/bin"; \
	fi
	@echo "Installed $(DEST)/Pitwall.app: open it from Spotlight, or: open -a Pitwall --args ~/code/your-repo"

dist: app
	rm -f $(ZIP) $(ZIP).sha256
	ditto -c -k --sequesterRsrc --keepParent $(APP) $(ZIP)
ifneq ($(NOTARY_PROFILE),)
	xcrun notarytool submit $(ZIP) --keychain-profile "$(NOTARY_PROFILE)" --wait
	xcrun stapler staple $(APP)
	rm -f $(ZIP)
	ditto -c -k --sequesterRsrc --keepParent $(APP) $(ZIP)
endif
	cd $(DIST) && shasum -a 256 $(notdir $(ZIP)) > $(notdir $(ZIP)).sha256
	@cat $(ZIP).sha256

# Linux has no app bundle: both binaries, a menu entry, the icon and an
# installer script, built for whichever architecture this runs on.
LINUX_NAME := pitwall-$(VERSION)-linux-$(shell uname -m)
LINUX_TGZ  := $(DIST)/$(LINUX_NAME).tar.gz

dist-linux:
	cargo build --release --locked -p pitwall -p pitwall-gui
	rm -rf $(DIST)/$(LINUX_NAME) $(LINUX_TGZ) $(LINUX_TGZ).sha256
	mkdir -p $(DIST)/$(LINUX_NAME)
	cp $(addprefix target/release/,$(BINS)) README.md scripts/pitwall.desktop $(DIST)/$(LINUX_NAME)/
	cp assets/pitwall-256.png $(DIST)/$(LINUX_NAME)/pitwall.png
	cp scripts/linux-install.sh $(DIST)/$(LINUX_NAME)/install.sh
	tar -C $(DIST) -czf $(LINUX_TGZ) $(LINUX_NAME)
	cd $(DIST) && sha256sum $(notdir $(LINUX_TGZ)) > $(notdir $(LINUX_TGZ)).sha256
	@cat $(LINUX_TGZ).sha256

# Everything a release needs, checked before anything is built or uploaded.
check:
	@command -v gh >/dev/null || { echo "needs the GitHub CLI: brew install gh"; exit 1; }
	@gh auth status >/dev/null 2>&1 || { echo "gh isn't logged in: gh auth login"; exit 1; }
	@for t in $(TARGETS); do \
		rustup target list --installed | grep -qx $$t || { echo "missing the Rust target $$t: make tools"; exit 1; }; \
	done
	@gh repo view $(REPO) >/dev/null 2>&1 || { \
		echo "There's no GitHub repo $(REPO). Create it and push, or set REPO=owner/name:"; \
		echo "  gh repo create $(REPO) --public --source . --push"; exit 1; }
	@gh api repos/$(REPO)/git/ref/tags/$(TAG) >/dev/null 2>&1 || { \
		echo "$(TAG) isn't on GitHub yet. Tag the commit to release and push it:"; \
		echo "  git tag $(TAG) && git push origin $(TAG)"; exit 1; }
	@if gh release view $(TAG) -R $(REPO) >/dev/null 2>&1; then \
		echo "$(TAG) is already released. Bump version in Cargo.toml for a new one."; exit 1; fi

release: check test dist
	gh release create $(TAG) $(ZIP) $(ZIP).sha256 -R $(REPO) --verify-tag \
		--title "Pitwall $(VERSION)" --generate-notes $(if $(PRERELEASE),--prerelease)
	@echo "Released: https://github.com/$(REPO)/releases/tag/$(TAG)"

# Written straight to the tap through the GitHub API, so no clone is needed.
cask:
ifneq ($(PRERELEASE),)
	@echo "$(VERSION) is a prerelease; the cask stays on the last full release."
else
	@test -f $(ZIP).sha256 || { echo "no $(ZIP).sha256: make dist (or make release) first"; exit 1; }
	@gh repo view $(TAP) >/dev/null 2>&1 || { \
		echo "There's no tap $(TAP). Create it, or set TAP=owner/homebrew-name:"; \
		echo "  gh repo create $(TAP) --public --add-readme"; exit 1; }
	sed -e 's/@VERSION@/$(VERSION)/g' -e "s/@SHA256@/$$(cut -d' ' -f1 $(ZIP).sha256)/g" \
		-e 's#@REPO@#$(REPO)#g' scripts/pitwall.rb > $(DIST)/pitwall.rb
	@# On a 404 gh still prints the error body to stdout, so drop it rather than send it as the sha.
	@sha=$$(gh api repos/$(TAP)/contents/Casks/pitwall.rb --jq .sha 2>/dev/null) || sha=; \
	gh api -X PUT repos/$(TAP)/contents/Casks/pitwall.rb \
		-f message="pitwall $(VERSION)" \
		-f content="$$(base64 < $(DIST)/pitwall.rb | tr -d '\n')" \
		$${sha:+-f sha=$$sha} >/dev/null
	@echo "Cask updated: brew install --cask $(TAP_NAME)/pitwall"
endif

publish: release cask

clean:
	rm -rf $(DIST)
