# Third-party notices

This file accompanies fpasobrowser source and binary distributions.  The
complete, versioned dependency inventory is [`Cargo.lock`](Cargo.lock).
Release binary archives also contain `THIRD_PARTY_LICENSES/`, generated from
that lockfile by `scripts/collect-third-party-licenses.sh`.  It preserves every
`LICENSE*`, `COPYING*`, `NOTICE*`, and `COPYRIGHT*` file found in each resolved
registry crate, including bundled native-library notices. A small number of
crates declare their SPDX license only in `Cargo.toml`; that manifest is
retained in their directory instead.

## Mozilla Public License 2.0 components

fpasobrowser embeds Servo and Mozilla-derived components that are licensed
under MPL-2.0, including Servo 0.7.0, mozjs 0.26.4 / mozjs_sys 153.3.0-1,
WebRender 0.70.0, Stylo 0.22.0, cssparser, and selectors.  The license text
for each resolved crate is included in `THIRD_PARTY_LICENSES/` in binary
archives.

The corresponding Source Code Form is available without charge from the
versioned crates named in `Cargo.lock`; for example:

- <https://crates.io/crates/servo/0.7.0>
- <https://crates.io/crates/mozjs/0.26.4>
- <https://crates.io/crates/mozjs_sys/153.3.0-1>
- <https://crates.io/crates/webrender/0.70.0>
- <https://crates.io/crates/stylo/0.22.0>

To reproduce the complete resolved source tree from a source checkout, use
`cargo vendor --locked third_party_source`.  fpasobrowser-specific source is
available in the matching GitHub Release source archive and repository tag.

## Other bundled components

The Servo build resolves additional permissively licensed Rust and native
components.  Notable native components include FreeType and libpng through
`freetype-sys`, and ANGLE through `mozangle`; their applicable license and
notice files are retained by the generated `THIRD_PARTY_LICENSES/` directory.
Do not remove that directory, this notice, `LICENSE`, or `Cargo.lock` when
redistributing a binary archive.

`Cargo.lock` is authoritative for the exact version set in a release.  Run
the collection script again after changing it, rather than copying a notice
set from another version.

Linux builds may dynamically link system libraries such as Fontconfig, and
macOS builds use Apple system frameworks. Those operating-system components are
not copied into this release archive; their license material remains with the
respective platform distribution. The Windows archive does copy ANGLE runtime
DLLs, whose notices are retained through the `mozangle` crate directory above.
