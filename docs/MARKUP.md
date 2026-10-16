# Script markup

The legend is always visible above the editor. Clicking an entry wraps the selected text
(or inserts the pause). Keyboard: <kbd>Ctrl</kbd>+<kbd>1</kbd>…<kbd>6</kbd> in the editor.

| Markup     | Meaning                         | Shortcut |
|------------|---------------------------------|----------|
| `"text"`   | excited / high-energy delivery  | Ctrl+1   |
| `[text]`   | calm / restrained delivery      | Ctrl+2   |
| `{text}`   | mystery / suspense              | Ctrl+3   |
| `*text*`   | emphasis                        | Ctrl+4   |
| `--`       | short pause (default 350 ms)    | Ctrl+5   |
| `---`      | long pause (default 900 ms)     | Ctrl+6   |
| untagged   | neutral                         |          |

A blank line is a paragraph pause (default 650 ms). Pause lengths and the gap between
sentences are adjustable in **Advanced** mode, which also shows the exact segment plan.

## Parsing rules (deterministic)

The parser is `crates/vs-core/src/markup.rs`; every rule below is covered by a unit test.

1. **Every spoken word is preserved, in order.** Markup characters are never pronounced.
   A pseudo-random fuzz test inserts markup between words 2,000 times and checks that the
   spoken word sequence never changes.
2. Only the ASCII straight quote `"` is markup. Typographic quotes `“ ” ‘ ’` are literal text,
   so quoted dialogue can be written with them.
3. `\` escapes the next markup character: `\"  \[  \]  \{  \}  \*  \-  \\`.
4. Tags nest. The **innermost emotion wins**; emphasis is independent of emotion:
   `"I can't *believe* it [quietly] okay"` → excited, excited+emphasis, excited, calm, excited.
5. `"` and `*` toggle: if one is open it closes, otherwise it opens.
6. Closing an outer tag while inner tags are still open closes the inner ones too (with a
   warning). The later stray closer is ignored.
7. An opener that is never closed, or a closer with no opener, is dropped (not spoken) and a
   warning with its position is shown. Its words keep the surrounding style.
8. Tags never continue across a blank line (paragraph).
9. A single `-` is a hyphen (`well-known`). Exactly two dashes are a short pause, three are a
   long pause, four or more are a long pause with a warning. `wait--what` works without
   spaces.
10. Empty spans (`""`, `[ ]`) produce a warning and nothing else.
11. Markup inside a word does not split it: `un*believ*able` is one word.
12. Unicode text is passed through untouched; emoji and symbols the voice cannot speak are
    skipped by the pronunciation step, not by the parser.

## What the engines actually do with markup

Voice Studio labels every control as **genuine**, **approximated** or **not supported** — in
the main window, in the segment inspector and in each project's details.

| Markup     | Kokoro 82M                                                         | Chatterbox (Voice Only)                         | Chatterbox (Voice + Style)          |
|------------|--------------------------------------------------------------------|-------------------------------------------------|-------------------------------------|
| excited    | approximated: rate 1.12×, +1.5 dB, final `.`→`!`, shorter pause     | **genuine**: exaggeration 0.95, CFG 0.3         | not applied (reference delivery)    |
| calm       | approximated: rate 0.90×, −1.5 dB, longer pause                     | **genuine**: exaggeration 0.3                   | not applied                         |
| mystery    | approximated: rate 0.84×, −3 dB, final `…`, 1.5× pause              | approximated: low intensity + 1.5× pause        | not applied                         |
| emphasis   | approximated: forced primary stress on the words                    | not supported (spoken normally)                 | not supported                       |
| pauses     | exact silence insertion                                             | exact silence insertion                         | exact silence insertion             |

Kokoro has no emotion conditioning, so its "excited" is a prosody approximation and does not
guarantee genuine excitement. Chatterbox's `exaggeration` is a real emotion-intensity
input of the model; it has no discrete suspense emotion.

## Batch files

Several scripts can share one file or the editor; separate them with a line containing only
`===`. An optional first line `# Title` names the script:

```
# Episode 1
"Octopuses have three hearts!" -- [Two pump blood to the gills.]
===
# Episode 2
{Their blood is blue.} --- It uses copper instead of iron.
```
