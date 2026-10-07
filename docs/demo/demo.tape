# Records the terminal part of docs/assets/demo.gif. Requires vhs, ffmpeg,
# ImageMagick and gifsicle. Run docs/demo/render.sh from the repository root.
# It builds minimenta, records this tape to MP4, and adds the captions.
# The demo deletes files in a throwaway home folder that make-home.sh creates.
# It uses D (delete permanently) because d asks Finder to move files to the Trash.
Output demo.mp4

Set Shell "bash"
Set Theme { "name": "minimenta GitHub Dark", "background": "#0D1117", "foreground": "#E6EDF3", "cursor": "#44B78F", "selection": "#3D444D", "black": "#484F58", "red": "#FF7B72", "green": "#3FB950", "yellow": "#E3B341", "blue": "#58A6FF", "magenta": "#BC8CFF", "cyan": "#39C5CF", "white": "#B1BAC4", "brightBlack": "#6E7681", "brightRed": "#FFA198", "brightGreen": "#56D364", "brightYellow": "#F0C14B", "brightBlue": "#79C0FF", "brightMagenta": "#D2A8FF", "brightCyan": "#56D4DD", "brightWhite": "#F0F6FC" }
Set WindowBar Colorful
Set WindowBarSize 40
Set BorderRadius 12
Set Margin 4
Set MarginFill "#21262D"
Set Padding 24
Set FontFamily "JetBrains Mono"
Set FontSize 19
Set Width 1500
Set Height 600
Set LineHeight 1.3
Set TypingSpeed 60ms
Set Framerate 24

Hide
Type `export PATH="$PWD/target/release:$PATH" HOME="$(cd "$(mktemp -d)" && pwd -P)" PS1='$ ' && docs/demo/make-home.sh "$HOME" && cd && clear`
Enter
Sleep 4s
Show

Sleep 500ms
Type "minimenta"
Sleep 400ms
Enter
Sleep 3s

# Scan the folder that the prompt shows.
Enter
Sleep 3s

# Open the largest folder.
Enter
Sleep 2s

# Shift+Down and J extend the selection in the same way.
Type "J"
Sleep 700ms
Type "J"
Sleep 1500ms

Type "D"
Sleep 3s
Type "y"
Sleep 2s

# Any key clears the message, and the footer shows the new totals.
Down
Sleep 3500ms

Hide
Type "q"
Type `rm -rf "$HOME" "$HOME.blob"`
Enter
Sleep 2s
