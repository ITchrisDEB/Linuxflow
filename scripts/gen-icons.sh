#!/bin/sh
# Generates the application icons in packaging/icons/:
#   app.svg, app-rec.svg, app-loading.svg   (vector sources)
#   png/<N>x<N>.png                         (16..512, rendered from app.svg)
# The files are committed; `cargo xtask dist` renames them after APP_ID.
# Same geometry for all the variants (relative bars, rounded square).
# PNG rendering: rsvg-convert, otherwise inkscape, otherwise magick; --svg-only to skip it.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
OUT="$ROOT/packaging/icons"
SVG_ONLY=0
[ "${1:-}" = "--svg-only" ] && SVG_ONLY=1

# Generates an SVG: $1 = background, $2 = recording dot ("" = none), $3 = bars (default cream)
make_svg() {
    awk -v bg="$1" -v dot="$2" -v bars="${3:-#ffffeb}" 'BEGIN {
        s = 512; fg = bars
        n = split("0.28 0.55 0.92 0.70 0.40 0.80 0.50", h, " ")
        pad = s * 0.18; inner = s - 2 * pad; gap = inner / (n * 2 - 1); cy = s / 2
        printf "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"512\" height=\"512\" viewBox=\"0 0 512 512\">\n"
        printf "  <rect width=\"512\" height=\"512\" rx=\"%.2f\" fill=\"%s\"/>\n", s * 0.22, bg
        for (i = 1; i <= n; i++) {
            x0 = pad + (i - 1) * 2 * gap; bh = inner * 0.9 * h[i]
            printf "  <rect x=\"%.2f\" y=\"%.2f\" width=\"%.2f\" height=\"%.2f\" rx=\"%.2f\" fill=\"%s\"/>\n", x0, cy - bh / 2, gap, bh, gap / 2, fg
        }
        if (dot != "") {
            r = s * 0.13; m = pad * 0.55
            printf "  <circle cx=\"%.2f\" cy=\"%.2f\" r=\"%.2f\" fill=\"%s\"/>\n", s - m - r, m + r, r, dot
        }
        printf "</svg>\n"
    }'
}

mkdir -p "$OUT"
make_svg "#1a1a1a" "" > "$OUT/app.svg"
make_svg "#1a1a1a" "" "#3ddc84" > "$OUT/app-rec.svg"
make_svg "#71716e" "" > "$OUT/app-loading.svg"
echo "SVG written to $OUT"

# No symbolic variant: the panel would prefer it to the coloured logo and recolour it.

[ "$SVG_ONLY" = 1 ] && exit 0

mkdir -p "$OUT/png"
if command -v rsvg-convert >/dev/null 2>&1; then
    render() { rsvg-convert -w "$1" -h "$1" "$OUT/app.svg" -o "$2"; }
elif command -v inkscape >/dev/null 2>&1; then
    render() { inkscape "$OUT/app.svg" -w "$1" -h "$1" -o "$2" >/dev/null 2>&1; }
elif command -v magick >/dev/null 2>&1; then
    render() { magick -background none -density 384 "$OUT/app.svg" -resize "$1x$1" "$2"; }
else
    echo "No rendering tool (rsvg-convert, inkscape, magick): PNG files not regenerated." >&2
    exit 1
fi
for n in 16 24 32 48 64 128 256 512; do
    render "$n" "$OUT/png/${n}x${n}.png"
done
echo "PNG written to $OUT/png"
