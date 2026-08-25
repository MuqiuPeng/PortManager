#!/bin/sh
# Check that a colour token is used for what it names.
#
# The palette used to be defined twice under overlapping names — `--accent`
# meant "the blue" in one sheet and "a quiet background" in the other, and
# which won depended on import order. A card came out bright blue and a line of
# text came out invisible before anyone noticed, because nothing here reads a
# stylesheet the way a compiler reads a program.
#
# Two things are checkable cheaply and would have caught both:
#   - only one sheet may define the palette
#   - a background token must not be used as a colour
set -e

theme="apps/desktop/src/theme.css"
sheet="apps/desktop/src/styles.css"

# Run from anywhere else and every grep below looks at nothing, finds nothing,
# and this reports that the colours are fine. A check that cannot see the file
# it is about must say so rather than pass.
for file in "$theme" "$sheet"; do
  [ -f "$file" ] || { echo "  cannot check: $file is not here (run from the repository root)"; exit 1; }
done

if grep -qE "^\s*--(background|foreground|card|muted|accent|primary|border):" "$sheet"; then
  echo "  $sheet defines palette tokens; they belong in $theme alone"
  exit 1
fi

# Tokens whose value is a surface. Using one as text means invisible text on
# the surface it was made for.
surfaces="background card popover muted accent secondary"
bad=""
for name in $surfaces; do
  if grep -qE "color:\s*var\(--$name\)\s*;" "$sheet"; then
    bad="$bad --$name"
  fi
done

if [ -n "$bad" ]; then
  echo "  used as a text colour but named for a surface:$bad"
  echo "  the foreground of a surface is --<name>-foreground"
  exit 1
fi

echo "  colour tokens are used for what they name"
