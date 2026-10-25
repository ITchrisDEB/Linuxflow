#!/usr/bin/env bash
# Regenerates po/linuxflow.pot from the Rust sources then merges the po/<language>.po catalogues.
# Can be re-run at will: keeps the existing translations, takes those from
# po/<language>.compendium.po (translation memory) for the new msgids, removes the
# obsolete entries and lists what remains untranslated or "fuzzy".
#
# Usage: scripts/update-po.sh [--check] [language...]     (default: all po/*.po)
#   --check: exit code 1 if some msgids remain untranslated or fuzzy.
# Requirements: gettext (xgettext, msgmerge, msgattrib, msgfmt).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

check=0
langs=()
for a in "$@"; do
  case "$a" in
    --check) check=1 ;;
    -h|--help) sed -n '2,10p' "$0"; exit 0 ;;
    *) langs+=("$a") ;;
  esac
done
if [ ${#langs[@]} -eq 0 ]; then
  for f in po/*.po; do
    [ -e "$f" ] || continue
    b="$(basename "$f" .po)"
    case "$b" in *.compendium) continue ;; esac
    langs+=("$b")
  done
  [ ${#langs[@]} -gt 0 ] || langs=(fr)
fi

for tool in xgettext msgmerge msgattrib msgfmt msginit; do
  command -v "$tool" >/dev/null || { echo "update-po: '$tool' not found (gettext package)" >&2; exit 2; }
done

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# Sources: all the .rs files of the crates and applications, except tests/ (their sample msgids are not
# interface texts) and except target/.
find crates apps -name '*.rs' -not -path '*/target/*' -not -path '*/tests/*' 2>/dev/null | LC_ALL=C sort > "$tmp/files"

xgettext --language=Rust --from-code=UTF-8 \
  --keyword --keyword=tr --keyword=trn:1,2 --keyword=tr_fmt \
  --add-comments=TRANSLATORS \
  --add-location=file --sort-by-file \
  --package-name=LinuxFlow --msgid-bugs-address=none \
  --files-from="$tmp/files" -o "$tmp/new.pot"

# Constant header date to avoid useless diffs when nothing changed.
sed -i -e 's/^"POT-Creation-Date:.*/"POT-Creation-Date: 2026-01-01 00:00+0000\\n"/' "$tmp/new.pot"
mkdir -p po
if ! cmp -s "$tmp/new.pot" po/linuxflow.pot 2>/dev/null; then
  cp "$tmp/new.pot" po/linuxflow.pot
fi
total="$(grep -c '^msgid ' po/linuxflow.pot || true)"
echo "linuxflow.pot : $((total - 1)) msgid"

status=0
for lang in "${langs[@]}"; do
  po="po/$lang.po"
  comp="po/$lang.compendium.po"
  args=(--backup=none --no-wrap)
  [ -f "$comp" ] && args+=(--compendium="$comp")
  if [ ! -f "$po" ]; then
    if [ -f "$comp" ]; then
      cp "$comp" "$po"   # take the header (language, plural forms) from the translation memory
    else
      msginit --no-translator --locale="$lang" --input=po/linuxflow.pot --output-file="$po" 2>/dev/null
    fi
  fi
  msgmerge --update "${args[@]}" "$po" po/linuxflow.pot 2>/dev/null
  msgattrib --no-obsolete --no-wrap -o "$po.tmp" "$po" && mv "$po.tmp" "$po"

  echo "== $lang =="
  LC_ALL=C msgfmt --check --statistics -o /dev/null "$po" 2>&1 || true
  # Msgid without a translation or fuzzy (header excluded).
  msgattrib --untranslated --no-wrap "$po" | grep '^msgid ' | tail -n +2 | sed 's/^msgid /  UNTRANSLATED: /' > "$tmp/missing" || true
  msgattrib --only-fuzzy --no-wrap "$po" | grep '^msgid ' | tail -n +2 | sed 's/^msgid /  FUZZY (to review): /' >> "$tmp/missing" || true
  if [ -s "$tmp/missing" ]; then
    cat "$tmp/missing"
    status=1
  else
    echo "  complete."
  fi
  : > "$tmp/missing"
done

[ "$check" -eq 1 ] && exit "$status"
exit 0
