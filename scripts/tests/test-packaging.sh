#!/bin/sh
# Real tests of install.sh / uninstall.sh and of `cargo xtask dist` (which builds the archive).
# Everything happens in a TEMPORARY folder (fake HOME included): never the real ~/.local.
# The binary is fake (copy of /usr/bin/true): no Cargo build.
set -u

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
T=$(mktemp -d) || exit 2
trap 'rm -rf "$T"' EXIT INT TERM HUP
PASS=0; FAIL=0

ok() { PASS=$((PASS + 1)); printf 'PASS  %s\n' "$1"; }
ko() { FAIL=$((FAIL + 1)); printf 'FAIL  %s\n' "$1"; }
check() { # check "nom" commande...
    name=$1; shift
    if "$@" >"$T/last.log" 2>&1; then ok "$name"; else ko "$name"; sed 's/^/      | /' "$T/last.log" | head -20; fi
}
check_not() { # the command must FAIL
    name=$1; shift
    if "$@" >"$T/last.log" 2>&1; then ko "$name (should have failed)"; sed 's/^/      | /' "$T/last.log" | head -20; else ok "$name"; fi
}

HOME_T=$T/home; mkdir -p "$HOME_T"
# isolated environment: no XDG or LINUXFLOW_* variable inherited
run() { env -i PATH="$PATH" HOME="$HOME_T" TMPDIR="$T" "$@"; }

APP_ID=$(sed -n 's/^pub const APP_ID: &str = "\(.*\)";.*/\1/p' "$ROOT/crates/linuxflow-core/src/constants.rs")

# ------------------------------------------------------------ 0. syntaxe
for s in install.sh uninstall.sh; do
    check "sh -n $s" sh -n "$ROOT/packaging/$s"
    check "bash --posix -n $s" bash --posix -n "$ROOT/packaging/$s"
    if command -v dash >/dev/null 2>&1; then check "dash -n $s" dash -n "$ROOT/packaging/$s"; fi
done
command -v shellcheck >/dev/null 2>&1 || echo "NOTE  shellcheck not installed: not run"

# ------------------------------------------------------------ 1. archive reproductible
export SOURCE_DATE_EPOCH=1700000000
cp /usr/bin/true "$T/fakebin"
(cd "$ROOT" && cargo build --release --quiet --manifest-path xtask/Cargo.toml) || { echo "cannot build xtask"; exit 1; }
XTASK=$ROOT/xtask/target/release/xtask
check "xtask dist (build 1)" "$XTASK" dist --fake-binary "$T/fakebin" --out "$T/d1"
check "xtask dist (build 2)" "$XTASK" dist --fake-binary "$T/fakebin" --out "$T/d2"
A1=$(ls "$T"/d1/*.tar.gz); NAME=$(basename "$A1" .tar.gz)
if [ "$(sha256sum <"$A1")" = "$(sha256sum <"$T"/d2/"$NAME".tar.gz)" ]; then ok "archive reproducible (same sha256 twice)"; else ko "archive not reproducible"; fi
check "tar owner normalised" sh -c "tar -tvf '$A1' | awk '{print \$2}' | sort -u | grep -qx '0/0'"
mkdir "$T/x"; tar -xzf "$A1" -C "$T/x"; PKG=$T/x/$NAME
check "archive has install.sh, uninstall.sh, SHA256SUMS" sh -c "[ -x '$PKG/install.sh' ] && [ -x '$PKG/uninstall.sh' ] && [ -f '$PKG/SHA256SUMS' ]"
check "icons named from APP_ID" sh -c "[ -f '$PKG/share/icons/hicolor/scalable/apps/$APP_ID.svg' ] && [ -f '$PKG/share/icons/hicolor/scalable/apps/$APP_ID-rec.svg' ] && [ -f '$PKG/share/icons/hicolor/512x512/apps/$APP_ID.png' ]"
check "template .desktop keeps @BINDIR@" grep -q '^Exec=@BINDIR@/linuxflow$' "$PKG/share/applications/$APP_ID.desktop"
check "SHA256SUMS verifies" sh -c "cd '$PKG' && sha256sum -c SHA256SUMS >/dev/null"

# ------------------------------------------------------------ 2. --help, dry-run
check "install.sh --help" run sh "$PKG/install.sh" --help
check "uninstall.sh --help" run sh "$PKG/uninstall.sh" --help
P0="$T/never created"
check "--dry-run succeeds" run sh "$PKG/install.sh" --prefix "$P0" --dry-run
check_not "--dry-run creates nothing" test -e "$P0"
# dry-run with the default prefix in the fake HOME
check "--dry-run (default prefix)" run sh "$PKG/install.sh" --dry-run
check_not "--dry-run creates nothing in HOME" test -e "$HOME_T/.local"

# ------------------------------------------------------------ 3. installation, prefix with spaces
P="$T/my prefix/usr local"
check "install (prefix with spaces)" run sh "$PKG/install.sh" --prefix "$P"
check "binary installed and executable" test -x "$P/bin/linuxflow"
for e in linuxflow-engine linuxflow-engine-v2 linuxflow-engine-vulkan linuxflow-overlay; do
    check "engine $e installed and executable" test -x "$P/bin/$e"
done
check "desktop file installed" test -f "$P/share/applications/$APP_ID.desktop"
check "Exec quoted for the space" grep -qF "Exec=\"$P/bin/linuxflow\"" "$P/share/applications/$APP_ID.desktop"
if command -v desktop-file-validate >/dev/null 2>&1; then
    check "desktop-file-validate on installed file" desktop-file-validate "$P/share/applications/$APP_ID.desktop"
fi
check "svg icon installed" test -f "$P/share/icons/hicolor/scalable/apps/$APP_ID.svg"
check "png 256 installed" test -f "$P/share/icons/hicolor/256x256/apps/$APP_ID.png"
check "licence installed" test -f "$P/share/doc/linuxflow/LICENSE"
check "third-party license TEXTS installed (MIT/BSD/ISC/Apache require them)" sh -c "[ -s '$P/share/doc/linuxflow/THIRD_PARTY_LICENSES.txt' ] && [ \$(wc -c <'$P/share/doc/linuxflow/THIRD_PARTY_LICENSES.txt') -gt 100000 ] && grep -q 'whisper.cpp' '$P/share/doc/linuxflow/THIRD_PARTY_LICENSES.txt' && grep -qi 'Apache License' '$P/share/doc/linuxflow/THIRD_PARTY_LICENSES.txt' && grep -q 'Permission is hereby granted' '$P/share/doc/linuxflow/THIRD_PARTY_LICENSES.txt'"
check "manifest written" test -f "$P/share/linuxflow/install-manifest"
check "uninstall.sh copied beside manifest" test -x "$P/share/linuxflow/uninstall.sh"
check_not "no autostart created" test -e "$HOME_T/.config/autostart"
check_not "no udev rule installed by default" sh -c "find '$P' -name '*uinput*' | grep -q ."
check_not "nothing written in the fake HOME" test -e "$HOME_T/.local"

# manifest: every listed file exists
check "every manifest file exists" sh -c "sed -n 's/^f //p' '$P/share/linuxflow/install-manifest' | while IFS= read -r f; do [ -e \"\$f\" ] || { echo missing \$f; exit 1; }; done"

# ------------------------------------------------------------ 4. idempotent reinstallation
find "$P" | LC_ALL=C sort >"$T/snap1"; (cd "$P" && find . -type f -exec sha256sum {} + | LC_ALL=C sort -k2) >"$T/sum1"
# obsolete file declared in the manifest: must disappear on update
echo old >"$P/share/linuxflow/obsolete.txt"; printf 'f %s\n' "$P/share/linuxflow/obsolete.txt" >>"$P/share/linuxflow/install-manifest"
check "re-install (upgrade)" run sh "$PKG/install.sh" --prefix "$P"
find "$P" | LC_ALL=C sort >"$T/snap2"; (cd "$P" && find . -type f -exec sha256sum {} + | LC_ALL=C sort -k2) >"$T/sum2"
check "re-install leaves identical tree" cmp "$T/snap1" "$T/snap2"
check "re-install leaves identical contents" cmp "$T/sum1" "$T/sum2"
check_not "obsolete file removed on upgrade" test -e "$P/share/linuxflow/obsolete.txt"

# ------------------------------------------------------------ 5. user data + uninstallation
CFG=$HOME_T/.config/linuxflow; DAT=$HOME_T/.local/share/linuxflow; CAC=$HOME_T/.cache/linuxflow
mkdir -p "$CFG" "$DAT/models" "$CAC"; echo '{}' >"$CFG/settings.json"; echo '[]' >"$DAT/history.json"; echo m >"$DAT/models/m.bin"
check "uninstall --dry-run" run sh "$P/share/linuxflow/uninstall.sh" --prefix "$P" --dry-run
check "dry-run kept the binary" test -x "$P/bin/linuxflow"
check "uninstall" run sh "$P/share/linuxflow/uninstall.sh" --prefix "$P"
check_not "no residue after uninstall (prefix tree)" sh -c "find '$T/my prefix' -mindepth 1 | grep -q ."
check "user data kept (settings)" test -f "$CFG/settings.json"
check "user data kept (history, models)" test -f "$DAT/models/m.bin"

# ------------------------------------------------------------ 6. default prefix = $HOME/.local (fake HOME)
H2=$T/home2; mkdir -p "$H2"
check "install with default prefix" env -i PATH="$PATH" HOME="$H2" TMPDIR="$T" sh "$PKG/install.sh"
check "default prefix puts binary in HOME/.local/bin" test -x "$H2/.local/bin/linuxflow"
check "XDG_DATA_HOME honoured on re-install is data-only" env -i PATH="$PATH" HOME="$H2" TMPDIR="$T" sh "$PKG/install.sh"
check "uninstall via the installed copy (no args)" env -i PATH="$PATH" HOME="$H2" TMPDIR="$T" sh "$H2/.local/share/linuxflow/uninstall.sh"
check_not "no residue in the fake HOME" sh -c "find '$H2' -mindepth 1 | grep -q ."

# XDG_DATA_HOME in user mode
H3=$T/home3; mkdir -p "$H3"
check "install with XDG_DATA_HOME" env -i PATH="$PATH" HOME="$H3" XDG_DATA_HOME="$H3/xdg data" TMPDIR="$T" sh "$PKG/install.sh"
check "files went to XDG_DATA_HOME" test -f "$H3/xdg data/applications/$APP_ID.desktop"
check "binary still in HOME/.local/bin" test -x "$H3/.local/bin/linuxflow"
check "install.sh --uninstall delegates" env -i PATH="$PATH" HOME="$H3" XDG_DATA_HOME="$H3/xdg data" TMPDIR="$T" sh "$PKG/install.sh" --uninstall
check_not "no residue (XDG case)" sh -c "find '$H3' -mindepth 1 | grep -q ."

# ------------------------------------------------------------ 7. corrupted SHA256SUMS / modified file / unlisted file
PC=$T/corrupt; mkdir "$PC"; tar -xzf "$A1" -C "$PC"; C=$PC/$NAME
sed '1s/^./0/;1s/^0/f/' "$C/SHA256SUMS" >"$C/S.new"; mv "$C/S.new" "$C/SHA256SUMS"
# makes sure the first checksum did change
check_not "corrupted SHA256SUMS refused" run sh "$C/install.sh" --prefix "$T/corrupt-prefix"
check_not "nothing installed after refusal" test -e "$T/corrupt-prefix"
PM=$T/modified; mkdir "$PM"; tar -xzf "$A1" -C "$PM"; M=$PM/$NAME
echo tamper >>"$M/share/doc/linuxflow/LICENSE"
check_not "modified file refused" run sh "$M/install.sh" --prefix "$T/mod-prefix"
check_not "nothing installed (modified)" test -e "$T/mod-prefix"
PU=$T/unlisted; mkdir "$PU"; tar -xzf "$A1" -C "$PU"; U=$PU/$NAME
echo extra >"$U/share/linuxflow/extra.txt"
check_not "file missing from SHA256SUMS refused" run sh "$U/install.sh" --prefix "$T/unl-prefix"
check_not "nothing installed (unlisted)" test -e "$T/unl-prefix"

# ------------------------------------------------------------ 8. minimum baseline not met
for key in GTK_MIN ADW_MIN GLIBC_MIN; do
    PB=$T/base-$key; mkdir "$PB"; tar -xzf "$A1" -C "$PB"; B=$PB/$NAME
    sed "s/^$key=.*/$key=99.0/" "$B/MIN_VERSIONS" >"$B/MV" && mv "$B/MV" "$B/MIN_VERSIONS"
    (cd "$B" && find . -type f ! -name SHA256SUMS | sed 's|^\./||' | LC_ALL=C sort | while IFS= read -r f; do sha256sum -- "$f"; done >SHA256SUMS)
    check_not "$key=99.0 refused" run sh "$B/install.sh" --prefix "$T/base-prefix-$key"
    if grep -q 'BUILD.md' "$T/last.log"; then ok "$key refusal points to BUILD.md"; else ko "$key refusal message lacks BUILD.md"; fi
    check_not "nothing installed ($key)" test -e "$T/base-prefix-$key"
done

# ------------------------------------------------------------ 9. udev rule
# root without --prefix: installation for all users under /usr/local (root simulated by a namespace)
if unshare -Ur true >/dev/null 2>&1; then
    check "as root without --prefix: dry run targets /usr/local" sh -c "unshare -Ur env -i PATH='$PATH' HOME='$HOME_T' sh '$PKG/install.sh' --dry-run 2>&1 | grep -q 'installing for all users under /usr/local'"
else
    echo "NOTE  user namespaces unavailable: root default prefix not tested"
fi
# the installer is in English only, whatever the locale (project decision)
check "installer stays in English under a French locale" sh -c "env -i PATH='$PATH' HOME='$HOME_T' LANG=fr_FR.UTF-8 sh '$PKG/install.sh' --dry-run --prefix '$T/en-prefix' 2>&1 | grep -q 'Dry run: nothing will be changed'"
check_not "--with-uinput-rule refused without root" run sh "$PKG/install.sh" --prefix "$T/udev-prefix" --with-uinput-rule
check_not "nothing installed (udev refusal)" test -e "$T/udev-prefix"
check "--with-uinput-rule --dry-run lists the rule" sh -c "env -i PATH='$PATH' HOME='$HOME_T' TMPDIR='$T' sh '$PKG/install.sh' --prefix '$T/udev-prefix' --udev-dir '$T/udev dir' --with-uinput-rule --dry-run 2>&1 | grep -q 99-linuxflow-uinput.rules"

# ------------------------------------------------------------ 10. --purge
H4=$T/home4; mkdir -p "$H4"; R4() { env -i PATH="$PATH" HOME="$H4" TMPDIR="$T" "$@"; }
check "install (purge scenario)" R4 sh "$PKG/install.sh"
mkdir -p "$H4/.config/linuxflow" "$H4/.local/share/linuxflow/models" "$H4/.cache/linuxflow" "$H4/.local/state/linuxflow"
echo '{}' >"$H4/.config/linuxflow/settings.json"; echo m >"$H4/.local/share/linuxflow/models/m.bin"
mkdir -p "$H4/.config/autostart"; echo x >"$H4/.config/autostart/$APP_ID.desktop"
check "purge dry-run keeps data" sh -c "env -i PATH='$PATH' HOME='$H4' TMPDIR='$T' sh '$H4/.local/share/linuxflow/uninstall.sh' --purge --dry-run >/dev/null 2>&1 && [ -f '$H4/.config/linuxflow/settings.json' ] && [ -x '$H4/.local/bin/linuxflow' ]"
check "purge answered 'no' keeps data" sh -c "echo no | env -i PATH='$PATH' HOME='$H4' TMPDIR='$T' sh '$H4/.local/share/linuxflow/uninstall.sh' --purge >/dev/null 2>&1; [ -f '$H4/.config/linuxflow/settings.json' ]"
check_not "...but LinuxFlow itself was uninstalled" test -e "$H4/.local/bin/linuxflow"
check "re-install (purge scenario 2)" R4 sh "$PKG/install.sh"
check "purge with EOF on stdin keeps data" sh -c "env -i PATH='$PATH' HOME='$H4' TMPDIR='$T' sh '$PKG/uninstall.sh' --purge --prefix '$H4/.local' </dev/null >/dev/null 2>&1; [ -f '$H4/.config/linuxflow/settings.json' ]"
check "re-install (purge scenario 3)" R4 sh "$PKG/install.sh"
check "purge answered 'yes'" sh -c "echo yes | env -i PATH='$PATH' HOME='$H4' TMPDIR='$T' sh '$PKG/uninstall.sh' --purge --prefix '$H4/.local' >'$T/purge.log' 2>&1; cat '$T/purge.log'"
check_not "purge deleted config, data, cache, state dirs and autostart entry" sh -c "find '$H4' -name linuxflow -o -name '$APP_ID.desktop' | grep -q ."
check_not "purge left no file at all" sh -c "find '$H4' -type f | grep -q ."

# ------------------------------------------------------------ 11. missing run-time library (real binary linked to a deleted lib)
if command -v cc >/dev/null 2>&1; then
    L=$T/lib; mkdir "$L"
    echo 'int lf_dummy(void){return 1;}' >"$L/d.c"; echo 'int main(void){extern int lf_dummy(void);return lf_dummy()*0;}' >"$L/m.c"
    if cc -shared -fPIC -Wl,-soname,libfoolf.so.9 -o "$L/libfoolf.so.9" "$L/d.c" 2>/dev/null &&
       cc -o "$L/linuxflow-missing" "$L/m.c" -L"$L" -Wl,--no-as-needed -l:libfoolf.so.9 2>/dev/null; then
        mv "$L/libfoolf.so.9" "$L/gone"
        PL=$T/lib-pkg; mkdir "$PL"
        "$XTASK" dist --fake-binary "$L/linuxflow-missing" --out "$T/d3" >/dev/null 2>&1
        tar -xzf "$T"/d3/*.tar.gz -C "$PL"
        check_not "missing libfoolf.so.9 refuses the install" run sh "$PL"/*/install.sh --prefix "$T/lib-prefix"
        if grep -q 'libfoolf.so.9' "$T/last.log" && grep -Eq 'apt-file search|pacman -F|dnf provides|zypper search' "$T/last.log"; then
            ok "missing library: lookup command printed for the detected distribution family (not executed)"
        else ko "missing library: package commands not printed"; fi
        check_not "nothing installed (missing library)" test -e "$T/lib-prefix"
    else
        echo "NOTE  cc could not build the test binary: missing-library test skipped"
    fi
fi

echo
echo "Result: $PASS passed, $FAIL failed"
[ "$FAIL" = 0 ]
