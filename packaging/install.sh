#!/bin/sh
# LinuxFlow installer (tar.gz archive). POSIX shell, no dependency on bash or Python.
# Messages are in English only.
#
# Principles: no hard-coded path (default prefix = $HOME/.local), nothing is installed
# on the system without an explicit request, no autostart, no group changes.
set -eu
umask 022

SELF=$(basename -- "$0")
SRC=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

# ---------------------------------------------------------------- messages
info() { printf '%s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

usage() {
    cat <<EOF
Usage: $SELF [options]

Installs LinuxFlow from the unpacked archive (run it from the archive directory).

Options:
  --prefix DIR          Install under DIR (bin/ and share/). Default: \$HOME/.local, or
                        /usr/local when run as root (sudo ./$SELF installs for all users).
                        The PREFIX environment variable is honoured too.
                        With a user prefix the data directory follows XDG_DATA_HOME.
  --dry-run             Show what would be done; change nothing.
  --with-uinput-rule    Also install the optional udev rule for /dev/uinput
                        (needs root; read the security warning it prints first).
  --udev-dir DIR        Directory for the udev rule (default: /etc/udev/rules.d).
  --uninstall           Run uninstall.sh (remaining options are passed to it).
  --help                Show this help.

The script verifies SHA256SUMS, checks the minimum base (see MIN_VERSIONS) and the
runtime libraries, then copies files and writes a manifest used by uninstall.sh.
It never installs packages, never touches groups, never enables autostart.
Running it again upgrades the existing installation in place.
EOF
}

# ---------------------------------------------------------------- arguments
PREFIX_ARG=${PREFIX:-}
PREFIX_EXPLICIT=0
[ -n "$PREFIX_ARG" ] && PREFIX_EXPLICIT=1
DRY_RUN=0
WITH_UINPUT=0
UDEV_DIR=/etc/udev/rules.d

while [ $# -gt 0 ]; do
    case $1 in
        --prefix)
            [ $# -ge 2 ] || die "--prefix needs a directory"
            PREFIX_ARG=$2; PREFIX_EXPLICIT=1; shift 2 ;;
        --prefix=*) PREFIX_ARG=${1#--prefix=}; PREFIX_EXPLICIT=1; shift ;;
        --dry-run) DRY_RUN=1; shift ;;
        --with-uinput-rule) WITH_UINPUT=1; shift ;;
        --udev-dir)
            [ $# -ge 2 ] || die "--udev-dir needs a directory"
            UDEV_DIR=$2; shift 2 ;;
        --uninstall)
            shift
            [ -f "$SRC/uninstall.sh" ] || die "uninstall.sh not found next to $SELF"
            exec sh "$SRC/uninstall.sh" "$@" ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; die "unknown option: $1" ;;
    esac
done

# ---------------------------------------------------------------- dossiers cibles
if [ "$PREFIX_EXPLICIT" = 0 ]; then
    if [ "$(id -u)" = 0 ]; then
        # sudo ./install.sh: install for all users, in the folder the FHS convention provides for
        # software added by the administrator (never in root's HOME).
        PREFIX_ARG=/usr/local
        PREFIX_EXPLICIT=1
        info "Running as root: installing for all users under $PREFIX_ARG (use --prefix DIR to choose another place)."
    else
        [ -n "${HOME:-}" ] || die "HOME is not set: pass --prefix DIR"
        PREFIX_ARG=$HOME/.local
    fi
fi
[ -n "$PREFIX_ARG" ] || die "empty prefix"
case $PREFIX_ARG in
    /*) ;;
    *) PREFIX_ARG=$PWD/$PREFIX_ARG ;;
esac
# strip trailing '/' (except for the root)
while [ "${PREFIX_ARG%/}" != "$PREFIX_ARG" ] && [ "$PREFIX_ARG" != / ]; do PREFIX_ARG=${PREFIX_ARG%/}; done
PREFIX=$PREFIX_ARG
BINDIR=$PREFIX/bin
DATADIR=$PREFIX/share
if [ "$PREFIX_EXPLICIT" = 0 ]; then
    case ${XDG_DATA_HOME:-} in
        /*) DATADIR=$XDG_DATA_HOME ;;
    esac
fi
case $PREFIX$DATADIR in
    *'
'*) die "newline in a path is not supported" ;;
esac
case $BINDIR in
    *'"'*|*'`'*|*'$'*|*'\'*|*'%'*) die "the characters \" \` \$ \\ % are not supported in the install path (they cannot be written safely into the .desktop file)" ;;
esac
MANIFEST=$DATADIR/linuxflow/install-manifest

TMP=$(mktemp -d) || die "cannot create a temporary directory"
trap 'rm -rf "$TMP"' EXIT INT TERM HUP

[ "$DRY_RUN" = 1 ] && info "Dry run: nothing will be changed."
info "Prefix : $PREFIX"
info "Binary : $BINDIR/linuxflow"
info "Data   : $DATADIR"

# ---------------------------------------------------------------- integrity check
verify_sums() {
    [ -f "$SRC/SHA256SUMS" ] || die "SHA256SUMS not found in $SRC (is this the unpacked archive?)"
    if have sha256sum; then SUMCMD="sha256sum"
    elif have shasum; then SUMCMD="shasum -a 256"
    else die "neither sha256sum nor shasum found: cannot verify the archive"
    fi
    # shellcheck disable=SC2086  # $SUMCMD must be split into words
    if ! (cd "$SRC" && $SUMCMD -c SHA256SUMS) >"$TMP/sums.log" 2>&1; then
        grep -v ': OK$' "$TMP/sums.log" >&2 || true
        die "checksum verification failed: the archive is corrupted or was modified. Download it again."
    fi
    # everything that will be installed must be covered by SHA256SUMS
    (cd "$SRC" && find . -type f ! -name SHA256SUMS | sed 's|^\./||' | LC_ALL=C sort) >"$TMP/tree"
    sed 's/^[0-9a-fA-F]*  *\*\{0,1\}//' "$SRC/SHA256SUMS" | LC_ALL=C sort >"$TMP/listed"
    LC_ALL=C comm -13 "$TMP/listed" "$TMP/tree" >"$TMP/unlisted"
    if [ -s "$TMP/unlisted" ]; then
        cat "$TMP/unlisted" >&2
        die "files not listed in SHA256SUMS (see above): refusing to install"
    fi
    info "Checksums OK ($(wc -l <"$TMP/listed" | tr -d ' ') files)."
}

# ---------------------------------------------------------------- minimum base and dependencies
ver_ge() { # $1 >= $2 ? (dotted versions)
    awk -v a="$1" -v b="$2" 'BEGIN {
        na = split(a, x, "."); nb = split(b, y, "."); n = (na > nb) ? na : nb
        for (i = 1; i <= n; i++) { p = x[i] + 0; q = y[i] + 0; if (p > q) exit 0; if (p < q) exit 1 }
        exit 0 }'
}

min_value() { # reads KEY from MIN_VERSIONS (never sourced: plain parsing)
    [ -f "$SRC/MIN_VERSIONS" ] || return 0
    awk -F= -v k="$1" '$1 == k { gsub(/[ \r\t]/, "", $2); print $2; exit }' "$SRC/MIN_VERSIONS"
}

LDD_OUT=
BIN=$SRC/bin/linuxflow
inspect_binary() {
    [ -f "$BIN" ] || die "bin/linuxflow not found in the archive"
    if have ldd; then
        LDD_OUT=$(ldd "$BIN" 2>&1 || true)
    fi
}

find_lib() { # resolved path of a soname: ldd of the binary, otherwise ldconfig
    p=$(printf '%s\n' "$LDD_OUT" | awk -v s="$1" '$1 == s && $2 == "=>" && $3 ~ /^\// { print $3; exit }')
    if [ -z "$p" ]; then
        for l in ldconfig /sbin/ldconfig /usr/sbin/ldconfig; do
            if have "$l"; then
                p=$("$l" -p 2>/dev/null | awk -v s="$1" '$1 == s { print $NF; exit }')
                break
            fi
        done
    fi
    printf '%s' "$p"
}

detect_gtk() {
    if have pkg-config; then
        v=$(pkg-config --modversion gtk4 2>/dev/null || true)
        [ -n "$v" ] && { printf '%s' "$v"; return; }
    fi
    p=$(find_lib libgtk-4.so.1)
    [ -n "$p" ] || return 0
    f=$(basename -- "$(readlink -f -- "$p" 2>/dev/null || printf '%s' "$p")")
    # libgtk-4.so.1.<minor*100>.<micro>
    printf '%s' "$f" | sed -n 's/^libgtk-4\.so\.1\.\([0-9]*\)\.\([0-9]*\)$/\1 \2/p' |
        awk '{ printf "4.%d.%d", int($1 / 100), $2 }'
}

detect_adw() {
    if have pkg-config; then
        pkg-config --modversion libadwaita-1 2>/dev/null || true
    fi
}

detect_glibc() {
    if have getconf; then
        v=$(getconf GNU_LIBC_VERSION 2>/dev/null | awk '{ print $2 }')
        [ -n "$v" ] && { printf '%s' "$v"; return; }
    fi
    if have ldd; then
        out=$(ldd --version 2>&1 | head -n 1 || true)
        case $out in
            *[Mm]usl*) printf 'musl'; return ;;
        esac
        printf '%s' "$out" | awk '{ print $NF }'
    fi
}

check_base() {
    gtk_min=$(min_value GTK_MIN); adw_min=$(min_value ADW_MIN); glibc_min=$(min_value GLIBC_MIN)
    fail=0
    g=$(detect_gtk)
    if [ -z "$g" ]; then
        warn "GTK 4 not found (or its version cannot be determined)."
        fail=1
    elif [ -n "$gtk_min" ] && ! ver_ge "$g" "$gtk_min"; then
        warn "GTK $g found, but LinuxFlow needs GTK >= $gtk_min."
        fail=1
    else
        info "GTK $g (>= ${gtk_min:-?}) OK."
    fi
    a=$(detect_adw)
    if [ -z "$a" ]; then
        warn "cannot determine the libadwaita version (pkg-config missing or libadwaita-1.pc absent): check by hand that it is >= ${adw_min:-?}."
    elif [ -n "$adw_min" ] && ! ver_ge "$a" "$adw_min"; then
        warn "libadwaita $a found, but LinuxFlow needs libadwaita >= $adw_min."
        fail=1
    else
        info "libadwaita $a (>= ${adw_min:-?}) OK."
    fi
    c=$(detect_glibc)
    if [ "$c" = musl ]; then
        warn "musl libc detected: only glibc systems are supported."
        fail=1
    elif [ -z "$c" ]; then
        warn "cannot determine the glibc version."
    elif [ -n "$glibc_min" ] && ! ver_ge "$c" "$glibc_min"; then
        warn "glibc $c found, but this build needs glibc >= $glibc_min."
        fail=1
    else
        info "glibc $c (>= ${glibc_min:-?}) OK."
    fi
    if [ "$fail" = 1 ]; then
        die "this system is below the supported base. Build LinuxFlow from source instead (see BUILD.md); that setup is unsupported but possible if GTK >= ${gtk_min:-?} and libadwaita >= ${adw_min:-?} can be provided."
    fi
}

os_family() {
    ids=$( (. /etc/os-release 2>/dev/null && printf '%s %s' "${ID:-}" "${ID_LIKE:-}") 2>/dev/null || true)
    for i in $ids; do
        case $i in
            debian|ubuntu|linuxmint|pop|raspbian) printf 'debian'; return ;;
            fedora|rhel|centos|rocky|almalinux) printf 'fedora'; return ;;
            arch|manjaro|endeavouros|cachyos) printf 'arch'; return ;;
            opensuse*|sles|suse) printf 'suse'; return ;;
        esac
    done
    printf 'unknown'
}

pkg_for() { # $1 = famille, $2 = soname -> nom de paquet (vide si inconnu)
    case $2:$1 in
        libgtk-4.so.1:debian) echo libgtk-4-1 ;;
        libgtk-4.so.1:fedora|libgtk-4.so.1:arch) echo gtk4 ;;
        libgtk-4.so.1:suse) echo libgtk-4-1 ;;
        libadwaita-1.so.0:debian|libadwaita-1.so.0:suse) echo libadwaita-1-0 ;;
        libadwaita-1.so.0:fedora|libadwaita-1.so.0:arch) echo libadwaita ;;
        libasound.so.2:debian) echo libasound2t64 ;;
        libasound.so.2:fedora|libasound.so.2:arch) echo alsa-lib ;;
        libasound.so.2:suse) echo libasound2 ;;
        libgtk4-layer-shell.so.0:debian|libgtk4-layer-shell.so.0:suse) echo libgtk4-layer-shell0 ;;
        libgtk4-layer-shell.so.0:fedora|libgtk4-layer-shell.so.0:arch) echo gtk4-layer-shell ;;
        libvulkan.so.1:debian|libvulkan.so.1:suse) echo libvulkan1 ;;
        libvulkan.so.1:fedora) echo vulkan-loader ;;
        libvulkan.so.1:arch) echo vulkan-icd-loader ;;
        libxkbcommon.so.0:debian|libxkbcommon.so.0:suse) echo libxkbcommon0 ;;
        libxkbcommon.so.0:fedora|libxkbcommon.so.0:arch) echo libxkbcommon ;;
        libxkbcommon-x11.so.0:debian|libxkbcommon-x11.so.0:suse) echo libxkbcommon-x11-0 ;;
        libxkbcommon-x11.so.0:fedora|libxkbcommon-x11.so.0:arch) echo libxkbcommon-x11 ;;
        *) echo "" ;;
    esac
}

# Prints (never runs) the install command for each distribution family.
print_pkg_commands() { # $@ = sonames manquants
    fam=$(os_family)
    for f in debian fedora arch suse; do
        [ "$fam" = unknown ] || [ "$fam" = "$f" ] || continue
        pk=
        unknown_libs=
        for s in "$@"; do
            p=$(pkg_for "$f" "$s")
            if [ -n "$p" ]; then pk="$pk $p"; else unknown_libs="$unknown_libs $s"; fi
        done
        case $f in
            debian) label="Debian / Ubuntu"; cmd="sudo apt install" ;;
            fedora) label="Fedora"; cmd="sudo dnf install" ;;
            arch) label="Arch"; cmd="sudo pacman -S" ;;
            suse) label="openSUSE"; cmd="sudo zypper install" ;;
        esac
        [ -n "$pk" ] && info "  $label: $cmd$pk"
        if [ -n "$unknown_libs" ]; then
            case $f in
                debian) info "  $label: apt-file search <soname>   (for:$unknown_libs)" ;;
                fedora) info "  $label: dnf provides '*/<soname>'   (for:$unknown_libs)" ;;
                arch) info "  $label: pacman -F <soname>   (for:$unknown_libs)" ;;
                suse) info "  $label: zypper search --provides <soname>   (for:$unknown_libs)" ;;
            esac
        fi
    done
}

check_deps() {
    if [ -z "$LDD_OUT" ]; then
        warn "ldd not available: runtime libraries were not checked."
        return 0
    fi
    case $LDD_OUT in
        *"not a dynamic executable"*|*"cannot execute"*|*"Exec format error"*|*"wrong ELF class"*)
            warn "ldd cannot inspect bin/linuxflow (not a dynamic executable for this machine?): runtime libraries were not checked."
            return 0 ;;
    esac
    missing=$(printf '%s\n' "$LDD_OUT" | awk '/not found/ { print $1 }')
    [ -z "$missing" ] && { info "Runtime libraries OK."; return 0; }
    hard=; soft=
    for s in $missing; do
        case $s in
            libvulkan.so.1) soft="$soft $s" ;;
            *) hard="$hard $s" ;;
        esac
    done
    if [ -n "$soft" ]; then
        warn "the Vulkan loader is missing:$soft. GPU acceleration will be unavailable (CPU fallback). If this build links it at load time the program will not start without it; install it with:"
        # shellcheck disable=SC2086
        print_pkg_commands $soft
    fi
    if [ -n "$hard" ]; then
        warn "missing required libraries:$hard. Install them (commands shown, nothing is executed):"
        # shellcheck disable=SC2086
        print_pkg_commands $hard
        die "missing runtime libraries"
    fi
}

# The layer-shell pill lives in a separate program (libgtk4-layer-shell is missing from some
# repositories): its absence does not prevent the installation, it only changes how the pill looks.
check_overlay_helper() {
    helper=$SRC/bin/linuxflow-overlay
    [ -f "$helper" ] || return 0
    have ldd || return 0
    if ldd "$helper" 2>&1 | grep -q 'libgtk4-layer-shell.*not found'; then
        warn "libgtk4-layer-shell is not installed: the floating recording pill will use a fallback window (XWayland) instead of layer-shell. LinuxFlow works either way. To get the layer-shell pill:"
        print_pkg_commands libgtk4-layer-shell.so.0
    fi
}

# ---------------------------------------------------------------- writing (or dry run)
OLD_F=$TMP/old.files; OLD_D=$TMP/old.dirs; OLD_C=$TMP/old.caches
NEW_F=$TMP/new.files; NEW_D=$TMP/new.dirs; NEW_C=$TMP/new.caches
: >"$OLD_F"; : >"$OLD_D"; : >"$OLD_C"; : >"$NEW_F"; : >"$NEW_D"; : >"$NEW_C"
: >"$TMP/dry.seen"

load_old_manifest() {
    [ -f "$MANIFEST" ] || return 0
    sed -n 's/^f //p' "$MANIFEST" >"$OLD_F"
    sed -n 's/^c //p' "$MANIFEST" >"$OLD_C"
    # keep only the directories that still exist
    sed -n 's/^d //p' "$MANIFEST" | while IFS= read -r d; do
        [ -d "$d" ] && printf '%s\n' "$d"
    done >"$OLD_D"
    cp "$OLD_D" "$NEW_D"
    info "Existing installation found: upgrading in place."
}

make_dir() {
    [ -d "$1" ] && return 0
    if [ "$DRY_RUN" = 1 ]; then
        grep -Fxq -- "$1" "$TMP/dry.seen" && return 0
        printf '%s\n' "$1" >>"$TMP/dry.seen"
        make_dir "$(dirname -- "$1")"
        info "  would create directory $1"
        return 0
    fi
    make_dir "$(dirname -- "$1")"
    mkdir -- "$1" || die "cannot create directory $1"
    printf '%s\n' "$1" >>"$NEW_D"
}

# install_file SRC DST MODE
install_file() {
    dst=$2
    make_dir "$(dirname -- "$dst")"
    if [ "$DRY_RUN" = 1 ]; then
        info "  would install $dst"
    else
        if [ -e "$dst" ] && ! grep -Fxq -- "$dst" "$OLD_F"; then
            warn "overwriting $dst, which was not installed by LinuxFlow"
        fi
        tmpf=$dst.lf-new.$$
        cp -- "$1" "$tmpf" && chmod "$3" "$tmpf" && mv -f -- "$tmpf" "$dst" || {
            rm -f -- "$tmpf"
            die "cannot write $dst"
        }
    fi
    printf '%s\n' "$dst" >>"$NEW_F"
}

# Fills @BINDIR@ in the .desktop file (quoted if the path contains special characters).
render_desktop() { # $1 = template, $2 = output
    LF_BINDIR=$BINDIR awk '
        function quote_if_needed(p) {
            if (p ~ /[ \t\n"'"'"'\\><~|&;$*?#()`]/) return "\"" p "\""
            return p
        }
        function repl(s, tok, val,    out, i) {
            out = ""
            while ((i = index(s, tok)) > 0) { out = out substr(s, 1, i - 1) val; s = substr(s, i + length(tok)) }
            return out s
        }
        BEGIN { b = ENVIRON["LF_BINDIR"] }
        /^Exec=@BINDIR@\// {
            rest = substr($0, length("Exec=@BINDIR@/") + 1)
            sp = index(rest, " "); prog = (sp > 0) ? substr(rest, 1, sp - 1) : rest
            args = (sp > 0) ? substr(rest, sp) : ""
            print "Exec=" quote_if_needed(b "/" prog) args
            next
        }
        { print repl($0, "@BINDIR@", b) }' "$1" >"$2"
}

install_tree() {
    while IFS= read -r rel; do
        case $rel in
            bin/*) install_file "$SRC/$rel" "$BINDIR/${rel#bin/}" 755 ;;
            share/applications/*.desktop)
                out=$TMP/$(basename -- "$rel")
                render_desktop "$SRC/$rel" "$out"
                install_file "$out" "$DATADIR/${rel#share/}" 644 ;;
            share/*) install_file "$SRC/$rel" "$DATADIR/${rel#share/}" 644 ;;
            uninstall.sh) install_file "$SRC/$rel" "$DATADIR/linuxflow/uninstall.sh" 755 ;;
            *) : ;; # install.sh, SHA256SUMS, MIN_VERSIONS, contrib/: not installed
        esac
    done <"$TMP/tree"
}

remove_stale() {
    [ -s "$OLD_F" ] || return 0
    grep -vxF -f "$NEW_F" "$OLD_F" >"$TMP/stale" || true
    [ -s "$TMP/stale" ] || return 0
    while IFS= read -r f; do
        case $f in
            */99-linuxflow-uinput.rules)
                # udev rule installed earlier: kept (only uninstall.sh removes it)
                printf '%s\n' "$f" >>"$NEW_F"; continue ;;
        esac
        if [ "$DRY_RUN" = 1 ]; then
            info "  would remove obsolete $f"
        else
            rm -f -- "$f" 2>/dev/null || warn "cannot remove obsolete file $f"
        fi
    done <"$TMP/stale"
}

# ---------------------------------------------------------------- udev rule (option)
install_udev_rule() {
    rule=$SRC/contrib/99-linuxflow-uinput.rules
    [ -f "$rule" ] || die "contrib/99-linuxflow-uinput.rules not found in the archive"
    cat >&2 <<EOF

SECURITY WARNING: the udev rule gives the 'input' group and the active local session
write access to /dev/uinput. Any program running as such a user can then create a
virtual keyboard and type on your behalf. Install it only if you need the uinput
paste backend. This script does NOT add you to any group and does not reload udev.
EOF
    install_file "$rule" "$UDEV_DIR/99-linuxflow-uinput.rules" 644
}

# ---------------------------------------------------------------- caches
cache_candidates() { printf '%s\n' "$DATADIR/icons/hicolor/icon-theme.cache" "$DATADIR/applications/mimeinfo.cache"; }
refresh_caches() {
    : >"$TMP/cache.before"
    cache_candidates | while IFS= read -r c; do
        if [ -e "$c" ]; then printf '%s\n' "$c" >>"$TMP/cache.before"; fi
    done
    if have update-desktop-database && [ -d "$DATADIR/applications" ]; then
        update-desktop-database -q "$DATADIR/applications" 2>/dev/null || warn "update-desktop-database failed (ignored)"
    fi
    for t in gtk4-update-icon-cache gtk-update-icon-cache; do
        if have "$t" && [ -d "$DATADIR/icons/hicolor" ]; then
            "$t" -q -t -f "$DATADIR/icons/hicolor" 2>/dev/null || warn "$t failed (ignored)"
            break
        fi
    done
    cache_candidates | while IFS= read -r c; do
        [ -e "$c" ] || continue
        if ! grep -Fxq -- "$c" "$TMP/cache.before" || grep -Fxq -- "$c" "$OLD_C"; then
            printf '%s\n' "$c" >>"$NEW_C"
        fi
    done
}

write_manifest() {
    {
        echo "# LinuxFlow install manifest v1 (used by uninstall.sh; do not edit)"
        sed 's/^/d /' "$NEW_D"
        sed 's/^/f /' "$NEW_F"
        sed 's/^/c /' "$NEW_C"
    } >"$TMP/manifest"
    cp "$TMP/manifest" "$MANIFEST.new.$$" && mv -f -- "$MANIFEST.new.$$" "$MANIFEST" || die "cannot write the manifest"
}

# ---------------------------------------------------------------- droits
check_writable() {
    d=$PREFIX
    while [ ! -e "$d" ]; do d=$(dirname -- "$d"); done
    if [ ! -w "$d" ]; then
        die "$d is not writable by the current user: choose another --prefix, or re-run with the privileges the prefix requires (for example sudo)."
    fi
    if [ "$DATADIR" != "$PREFIX/share" ]; then
        d=$DATADIR
        while [ ! -e "$d" ]; do d=$(dirname -- "$d"); done
        [ -w "$d" ] || die "$d is not writable by the current user"
    fi
}

# ---------------------------------------------------------------- main flow
if [ "$WITH_UINPUT" = 1 ] && [ "$DRY_RUN" = 0 ] && [ "$(id -u)" != 0 ]; then
    die "--with-uinput-rule needs root (try: sudo ./$SELF --prefix <dir> --with-uinput-rule)"
fi
if [ "$DRY_RUN" = 1 ]; then
    d=$PREFIX; while [ ! -e "$d" ]; do d=$(dirname -- "$d"); done
    [ -w "$d" ] || warn "$d is not writable: a real run would fail without the right privileges."
else
    check_writable
fi

verify_sums
inspect_binary
check_base
check_deps
check_overlay_helper

load_old_manifest
install_tree
[ "$WITH_UINPUT" = 1 ] && install_udev_rule
remove_stale

if [ "$DRY_RUN" = 1 ]; then
    info "Dry run finished: no file was created or modified."
    exit 0
fi

refresh_caches
write_manifest

n=$(wc -l <"$NEW_F" | tr -d ' ')
info ""
info "LinuxFlow installed: $n files."
info "Manifest: $MANIFEST"
case ":${PATH:-}:" in
    *":$BINDIR:"*) ;;
    *) info "Note: $BINDIR is not in your PATH; the application menu entry works regardless." ;;
esac
if [ "$WITH_UINPUT" = 1 ]; then
    info "udev rule installed. Reload with: udevadm control --reload && udevadm trigger (not done automatically)."
fi
info "Autostart was not enabled (it is an application setting). Your data and settings were not touched."
info "To remove: $DATADIR/linuxflow/uninstall.sh"
