#!/usr/bin/env bash
# Renders docs/assets/demo.gif: records docs/demo/demo.tape, adds the captions
# below the terminal, rounds the outer corners, and optimizes the GIF.
# Requires vhs, ffmpeg, ImageMagick, gifsicle, and the Inter font.
# Set CAPTION_FONT to the path of Inter-SemiBold.ttf if fc-match cannot find it.
# Usage: docs/demo/render.sh (from the repository root)
set -euo pipefail

font=${CAPTION_FONT:-$(fc-match -f '%{file}' 'Inter:style=SemiBold')}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

cargo build --release -q
vhs docs/demo/demo.tape -o "$work/demo.mp4" >/dev/null

width=$(ffprobe -v error -select_streams v -show_entries stream=width -of csv=p=0 "$work/demo.mp4")
height=$(ffprobe -v error -select_streams v -show_entries stream=height -of csv=p=0 "$work/demo.mp4")
duration=$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$work/demo.mp4")
strip=60
radius=16

# Start and end second of each caption. The times follow the Sleep steps in demo.tape.
captions=(
  "0 4.5|Run minimenta. Enter scans the folder in the prompt."
  "4.5 7.5|The largest items come first. Enter opens one."
  "7.5 11.75|Shift+Down or J selects a range of items."
  "11.75 14.6|D deletes permanently after you confirm. d moves items to the Trash."
  "14.6 99|The totals update at once."
)

inputs=(-i "$work/demo.mp4")
filter="[0:v]pad=${width}:$((height + strip)):0:0:color=#21262D[v0]"
for i in "${!captions[@]}"; do
  times=${captions[$i]%%|*}
  text=${captions[$i]#*|}
  magick -size "${width}x${strip}" xc:none -font "$font" -pointsize 26 -fill '#44B78F' \
    -gravity north -annotate +0+8 "$text" "$work/caption$i.png"
  inputs+=(-i "$work/caption$i.png")
  read -r start end <<<"$times"
  filter+=";[v$i][$((i + 1)):v]overlay=0:${height}:enable='between(t,${start},${end})'[v$((i + 1))]"
done

magick -size "${width}x$((height + strip))" xc:black -fill white \
  -draw "roundrectangle 0,0 $((width - 1)),$((height + strip - 1)) ${radius},${radius}" "$work/mask.png"
inputs+=(-loop 1 -t "$duration" -i "$work/mask.png")
mask=$((${#captions[@]} + 1))
last=${#captions[@]}
filter+=";[${mask}:v]format=gray[m];[v${last}][m]alphamerge,fps=12,split[a][b]"
filter+=";[a]palettegen=max_colors=128:reserve_transparent=1:stats_mode=full[p]"
filter+=";[b][p]paletteuse=dither=none:alpha_threshold=128"

ffmpeg -v error -y "${inputs[@]}" -filter_complex "$filter" "$work/demo.gif"
gifsicle -O3 "$work/demo.gif" -o docs/assets/demo.gif
ls -l docs/assets/demo.gif
