#!/bin/sh
# LinuxFlow uninstaller: removes exactly the files listed in the manifest written by
# install.sh, then the empty directories the installation had created.
# User data (config, data, cache, state) is NEVER touched,
# except with --purge (the list is shown, then an interactive confirmation is required).
set -eu

SELF=$(basename -- "$0")
SELF_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

info() { printf '%s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

usage() {
    cat <<EOF
Usage: $SELF [options]

Removes the files installed by install.sh (listed in the install manifest).

Options:
  --prefix DIR      Prefix used at install time (default: \$HOME/.local, or /usr/local as root; PREFIX is honoured).
  --manifest FILE   Use this manifest file directly.
  --dry-run         Show what would be removed; change nothing.
  --purge           ALSO delete your LinuxFlow settings, history, logs and downloaded
                    models. The directories are listed first and you must confirm.
  --help            Show this help.

Without --purge your data is kept. To delete only the downloaded models by hand
(default location, unless you changed the model directory):
  rm -rf "\${XDG_CACHE_HOME:-\$HOME/.cache}/linuxflow/models"
EOF
}

PREFIX_ARG=${PREFIX:-}
PREFIX_EXPLICIT=0
[ -n "$PREFIX_ARG" ] && PREFIX_EXPLICIT=1
MANIFEST_ARG=
DRY_RUN=0
PURGE=0
while [ $# -gt 0 ]; do
    case $1 in
        --prefix)
            [ $# -ge 2 ] || die "--prefix needs a directory"
            PREFIX_ARG=$2; PREFIX_EXPLICIT=1; shift 2 ;;
        --prefix=*) PREFIX_ARG=${1#--prefix=}; PREFIX_EXPLICIT=1; shift ;;
        --manifest)
            [ $# -ge 2 ] || die "--manifest needs a file"
            MANIFEST_ARG=$2; shift 2 ;;
        --dry-run) DRY_RUN=1; shift ;;
        --purge) PURGE=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; die "unknown option: $1" ;;
    esac
done

# ---------------------------------------------------------------- locating the manifest
if [ -n "$MANIFEST_ARG" ]; then
    MANIFEST=$MANIFEST_ARG
elif [ "$PREFIX_EXPLICIT" = 1 ]; then
    case $PREFIX_ARG in /*) ;; *) PREFIX_ARG=$PWD/$PREFIX_ARG ;; esac
    while [ "${PREFIX_ARG%/}" != "$PREFIX_ARG" ] && [ "$PREFIX_ARG" != / ]; do PREFIX_ARG=${PREFIX_ARG%/}; done
    MANIFEST=$PREFIX_ARG/share/linuxflow/install-manifest
elif [ -f "$SELF_DIR/install-manifest" ]; then
    # installed copy of this script: the manifest sits next to it
    MANIFEST=$SELF_DIR/install-manifest
else
    [ -n "${HOME:-}" ] || die "HOME is not set: pass --prefix DIR or --manifest FILE"
    case ${XDG_DATA_HOME:-} in
        /*) MANIFEST=$XDG_DATA_HOME/linuxflow/install-manifest ;;
        *) MANIFEST=$HOME/.local/share/linuxflow/install-manifest ;;
    esac
    [ -f "$MANIFEST" ] || MANIFEST=$HOME/.local/share/linuxflow/install-manifest
    # as root: system-wide installation (sudo ./install.sh)
    if [ ! -f "$MANIFEST" ] && [ "$(id -u)" = 0 ] && [ -f /usr/local/share/linuxflow/install-manifest ]; then
        MANIFEST=/usr/local/share/linuxflow/install-manifest
    fi
fi

[ "$DRY_RUN" = 1 ] && info "Dry run: nothing will be changed."

TMP=$(mktemp -d) || die "cannot create a temporary directory"
trap 'rm -rf "$TMP"' EXIT INT TERM HUP

APP_ID=
# ---------------------------------------------------------------- file removal
uninstall_files() {
    sed -n 's/^f //p' "$MANIFEST" >"$TMP/files"
    sed -n 's/^c //p' "$MANIFEST" >"$TMP/caches"
    sed -n 's/^d //p' "$MANIFEST" >"$TMP/dirs"
    # a manifest must only contain absolute paths
    for l in files caches dirs; do
        if grep -qv '^/' "$TMP/$l"; then die "manifest $MANIFEST contains a relative path: refusing to continue"; fi
    done
    n=0
    while IFS= read -r f; do
        case $f in
            "$MANIFEST") continue ;;
        esac
        if [ -e "$f" ] || [ -L "$f" ]; then
            if [ -d "$f" ] && [ ! -L "$f" ]; then
                warn "$f is a directory, not a file: skipped"
                continue
            fi
            if [ "$DRY_RUN" = 1 ]; then
                info "  would remove $f"
            else
                rm -f -- "$f" 2>/dev/null || warn "cannot remove $f (permissions?)"
            fi
            n=$((n + 1))
        fi
        case $f in
            */applications/*.desktop) APP_ID=$(basename -- "$f" .desktop) ;;
        esac
    done <"$TMP/files"
    info "Files removed: $n"
}

refresh_caches() {
    # rebuild the caches of directories that still hold something other than our files
    sed -n 's|/icon-theme.cache$||p' "$TMP/caches" | while IFS= read -r d; do
        [ -d "$d" ] || continue
        for t in gtk4-update-icon-cache gtk-update-icon-cache; do
            if have "$t"; then "$t" -q -t -f "$d" 2>/dev/null || true; break; fi
        done
    done
    sed -n 's|/mimeinfo.cache$||p' "$TMP/caches" | while IFS= read -r d; do
        [ -d "$d" ] || continue
        if have update-desktop-database; then update-desktop-database -q "$d" 2>/dev/null || true; fi
    done
}

remove_dirs() {
    # parents come first in the manifest: walk it backwards
    awk '{ a[NR] = $0 } END { for (i = NR; i >= 1; i--) print a[i] }' "$TMP/dirs" >"$TMP/dirs.rev"
    while IFS= read -r d; do
        [ -d "$d" ] || continue
        # a directory that only holds our cache files counts as empty
        others=$(ls -A "$d" 2>/dev/null | wc -l | tr -d ' ')
        cached=0
        while IFS= read -r c; do
            case $c in "$d"/*) [ -e "$c" ] && cached=$((cached + 1)) ;; esac
        done <"$TMP/caches"
        if [ "$others" -gt 0 ] && [ "$others" -eq "$cached" ]; then
            while IFS= read -r c; do
                case $c in "$d"/*)
                    if [ "$DRY_RUN" = 1 ]; then info "  would remove cache $c"; else rm -f -- "$c"; fi ;;
                esac
            done <"$TMP/caches"
        fi
        if [ "$DRY_RUN" = 1 ]; then
            if [ "$others" -eq "$cached" ]; then info "  would remove empty directory $d"; fi
        else
            rmdir -- "$d" 2>/dev/null || true
        fi
    done <"$TMP/dirs.rev"
}

# ---------------------------------------------------------------- purge of user data
safe_dir() { # refuses dangerous values
    case $1 in
        ""|/|"${HOME:-/}"|"${HOME:-/}/") return 1 ;;
        /*) return 0 ;;
        *) return 1 ;;
    esac
}

purge_user_data() {
    [ -n "${HOME:-}" ] || die "HOME is not set: cannot locate user data"
    : >"$TMP/purge"
    cfg=${LINUXFLOW_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/linuxflow}
    dat=${LINUXFLOW_DATA_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/linuxflow}
    cac=${LINUXFLOW_CACHE_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/linuxflow}
    sta=${LINUXFLOW_STATE_DIR:-${XDG_STATE_HOME:-$HOME/.local/state}/linuxflow}
    for d in "$cfg" "$dat" "$cac" "$sta"; do
        if ! safe_dir "$d"; then warn "unsafe directory '$d': skipped"; continue; fi
        [ -d "$d" ] && printf '%s\n' "$d" >>"$TMP/purge"
    done
    if [ -n "$APP_ID" ]; then
        af=${XDG_CONFIG_HOME:-$HOME/.config}/autostart/$APP_ID.desktop
        [ -f "$af" ] && printf '%s\n' "$af" >>"$TMP/purge"
    fi
    if [ ! -s "$TMP/purge" ]; then info "No LinuxFlow user data found."; return 0; fi
    info ""
    info "--purge: the following user data will be PERMANENTLY deleted:"
    while IFS= read -r p; do
        sz=$(du -sh -- "$p" 2>/dev/null | awk '{ print $1 }')
        info "  $p   (${sz:-?})"
    done <"$TMP/purge"
    if [ -n "${LINUXFLOW_MODEL_DIR:-}" ]; then
        info "  (LINUXFLOW_MODEL_DIR=$LINUXFLOW_MODEL_DIR is NOT deleted: remove it yourself if you want to.)"
    fi
    if [ "$DRY_RUN" = 1 ]; then info "Dry run: nothing deleted."; return 0; fi
    printf 'Type "yes" to delete them: '
    ans=
    read -r ans || ans=
    if [ "$ans" != yes ]; then info "Aborted: user data kept."; return 0; fi
    while IFS= read -r p; do
        rm -rf -- "$p" && info "  deleted $p"
    done <"$TMP/purge"
}

# ---------------------------------------------------------------- main flow
if [ -f "$MANIFEST" ]; then
    head -n 1 "$MANIFEST" | grep -q '^# LinuxFlow install manifest' || die "$MANIFEST is not a LinuxFlow manifest"
    info "Manifest: $MANIFEST"
    uninstall_files
    if [ "$DRY_RUN" = 0 ]; then
        rm -f -- "$MANIFEST"
        refresh_caches
    fi
    remove_dirs
    if [ -n "$APP_ID" ] && [ "$PURGE" = 0 ] && [ -n "${HOME:-}" ]; then
        af=${XDG_CONFIG_HOME:-$HOME/.config}/autostart/$APP_ID.desktop
        [ -f "$af" ] && info "Note: the autostart entry $af was kept (it is an application setting). Remove it by hand or use --purge."
    fi
    info "LinuxFlow removed. Your settings, history and models were kept."
else
    if [ "$PURGE" = 1 ]; then
        warn "no manifest at $MANIFEST: nothing to uninstall, purging user data only (an autostart entry in the XDG autostart directory, named after the application id, can only be removed when uninstalling and purging in the same run)."
    else
        die "no install manifest at $MANIFEST: nothing to uninstall (wrong --prefix?)."
    fi
fi

if [ "$PURGE" = 1 ]; then purge_user_data; fi
exit 0
