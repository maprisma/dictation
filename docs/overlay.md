# Maprisma Dictation: the overlay (step 4)

`maprisma-dictation` is a small pill at the top centre of the screen. You speak or type a question,
the AI helper you chose in Maprisma Manager → AI answers, and the answer shows in the pill. In this
step it only **answers**: it does not act on the desktop yet (that is step 5, `maprisma-actions`).

```sh
maprisma-dictation --toggle    # open and listen (ends when you stop talking); open already → close
maprisma-dictation --hold      # hold-to-talk: bind to a key press …
maprisma-dictation --release   # … and this to the same key's release
maprisma-dictation --demo      # made-up voice levels, speech and answer: try the look without a mic or helper
```

## Using it

| | |
|---|---|
| Open | the key you bound (below). It starts listening at once; the field is focused too. |
| Talk | the live text shows greyed (60 %); about 0.9 s after you stop it is sent. Held key: it is sent when you let go. A tap shorter than 250 ms on a hold binding keeps listening until you stop talking. |
| Type | typing any key stops the microphone and you type instead; **Enter** sends. |
| Follow-up | ask again within 45 s (also after closing and reopening): the last question and answer go along as context. |
| **Ctrl+M** | listen again (or stop listening and send what was heard). |
| **Ctrl+Shift+C** | copy the answer. |
| **Esc** | in stages: first it stops listening or the helper's answer (the question comes back into the field); the next Esc closes. |
| Click outside | closes, but only when nothing would be lost (no listening, no answer coming, no typed text). |
| **F12** | saves a picture of the overlay to `~/Pictures/Maprisma Dictation/`. |

The answer understands a little Markdown: paragraphs, headings, bullet and numbered lists, and code.
The pill grows from 380×52 to at most 480×360 as content arrives, and the answer scrolls beyond that.

## The look

- Colours, corners and the light/dark choice come from the desktop, like every Maprisma app
  (`src/look.rs` and `src/corners.rs` are the shared copies): COSMIC's theme, Maprisma Shell's
  scheme on Hyprland, kdeglobals on KDE. The pill is the theme's background at 92 % opacity; its
  radius is the desktop's (clamped to 12–26, 0 with Corners → Square).
- **The aura ring** (docs/research/antigravity-design.md B, option 1): a 2 px gradient stroke and a
  soft bloom around the pill, in three hues taken from the theme's accent in OKLCH (H, H+28°, H−20°;
  dark themes L 0.74 and C 0.10–0.18, light themes L 0.50 and C 0.12–0.20, which keeps 4.5:1 against
  white). While listening the microphone level (attack 30 ms, release 130 ms) drives the stroke
  (1.5–3.5 px) and the bloom (15–65 %). Transcribing: a ripple runs inwards. Thinking: the colours
  sweep round once every 1.4 s. Done: a 280 ms green flash. Error: a 320 ms shake of ±4 px, and red.
  Idle: a faint 1 px ring that breathes slowly.
- Reduced motion (the desktop's setting: Plasma's animation speed at "instant", Hyprland's
  `animations:enabled = false`, GNOME/COSMIC `enable-animations`; or the `motion` setting) keeps the
  level and the colours but drops the sweep, the ripple, the shake, the breath and the size morph.

## Settings

`~/.config/maprisma-dictation/settings.json` (a missing or broken value is the default). The
overlay only reads it; Maprisma Manager's Voice page (step 2) will write it.

| key | default | |
|---|---|---|
| `language` | `system` | `system`, `en`, `de`, `fr`, `es`, `it`, `pt`, `nl`, `pl`; also the language the helper is asked to answer in when the question's language is unclear |
| `corners` | `desktop` | `desktop`, `slight`, `rounded`, `square` |
| `motion` | `desktop` | `desktop`, `full`, `reduced` |
| `listen` | `true` | start listening on open (false: typing only, Ctrl+M listens) |
| `end` | `silence` | how a toggle session ends: `silence` or `release` |
| `screenshot_key` | `true` | F12 saves a picture |
| `keyboard` | `on_demand` | `exclusive` for a compositor that doesn't give an on-demand layer surface the keyboard |

Voice settings (engine, model, language, microphone) are maprisma-voice's (docs/protocol.md).

## The AI helper

The helper is the one all Maprisma apps share: `~/.config/maprisma-ai/settings.json` →
`helper` (`auto` = the first installed one in the catalog's order). The catalog is Maprisma's
`/usr/lib/maprisma/data/helpers.json` (or the copy in `overlay/data/helpers.json` when that is newer).
Only entries with a one-shot `ask` command count (Claude Code, Codex, Gemini, Antigravity, Cursor,
Copilot, Grok, Hermes, omp, OpenClaw, pi); `pi:<provider>/<model>` runs pi with `--provider` and
`--model`.

- The question goes in the argv as one argument (or on stdin when the catalog says so), never
  through a shell, after a short preamble: "you are a desktop assistant on Linux (COSMIC, KDE
  Plasma, Hyprland); answer briefly in the user's language; you cannot act on the desktop yet".
- It runs in `$XDG_RUNTIME_DIR/maprisma-dictation/work` in its own process group, with
  `NO_COLOR=1`; output is cut at 256 KiB and the run at 4 minutes. Esc, or closing the overlay, stops
  the whole group.
- The last exchange is kept in `$XDG_RUNTIME_DIR/maprisma-dictation/last.json` (0600, deleted when
  older than 45 s, gone at logout). Nothing else is stored.

## A key for it

Suggested: **Super+Alt+D** ("D" for dictation). On this PC it is free on all three desktops
(Super+Space is the launcher, Super+Alt+Space the Maprisma Launcher, Super+Alt+V Omarchy's
clipboard). Use the full path (`~/.local/bin/maprisma-dictation`) if `~/.local/bin` is not on the
desktop's PATH.

**Hyprland** (Lua config, e.g. `~/.config/hypr/hypr-user.lua`):

```lua
-- tap to toggle
hl.bind("SUPER + ALT + D", hl.dsp.exec_cmd("maprisma-dictation --toggle"), { description = "Maprisma Dictation" })

-- or hold to talk (a tap still toggles)
hl.bind("SUPER + ALT + D", hl.dsp.exec_cmd("maprisma-dictation --hold"), { description = "Maprisma Dictation" })
hl.bind("SUPER + ALT + D", hl.dsp.exec_cmd("maprisma-dictation --release"), { release = true })
```

In the classic `hyprland.conf` syntax: `bind = SUPER ALT, D, exec, maprisma-dictation --hold` and
`bindr = SUPER ALT, D, exec, maprisma-dictation --release`.

**KDE Plasma**: System Settings → Keyboard → Shortcuts → Add New → Command or Script →
`maprisma-dictation --toggle`, then set Meta+Alt+D. KDE's global shortcuts report no key release,
so on KDE it is **toggle only**.

**COSMIC**: Settings → Keyboard → Keyboard Shortcuts → Custom Shortcuts → Add shortcut: name
"Maprisma Dictation", command `maprisma-dictation --toggle`, key Super+Alt+D. Or in
`~/.config/cosmic/com.system76.CosmicSettings.Shortcuts/v1/custom`:
`(modifiers: [Super, Alt], key: "d", description: Some("Maprisma Dictation")): Spawn("maprisma-dictation --toggle"),`
COSMIC's custom shortcuts have no release action either: **toggle only**.

There is no `--bind-key`: writing three desktops' key settings (kglobalaccel, COSMIC's RON file,
Hyprland's Lua) from here risks clobbering the user's own bindings. Maprisma Manager's Voice page
(step 2, "key and mode") will set it through the Launcher's existing shortcut code for each desktop.

## Desktops

The overlay is a wlr-layer-shell surface (overlay layer, top anchor, 48 px below the top edge or a
top panel, keyboard on demand), so it needs a compositor with layer-shell: COSMIC, KDE Plasma 6 and
Hyprland all have it. While a click outside may close it, a transparent full-screen layer under it
(top layer) catches that click.

| | Hyprland | KDE Plasma 6 | COSMIC |
|---|---|---|---|
| Overlay, ring, answers | checked (screenshots) | expected (layer-shell) | expected (layer-shell; libcosmic's own) |
| Keyboard focus on open | checked (`MAPRISMA_DICTATION_DEBUG=1` shows `Focused`) | to check; `keyboard: exclusive` if not | to check; `keyboard: exclusive` if not |
| Hold to talk | yes (`--hold` + release bind) | no (toggle only) | no (toggle only) |

## Toolkit

libcosmic (iced), the same revision as the other Maprisma Rust apps, with its Wayland layer-shell
support (`get_layer_surface`, `set_input_zone`): the theme, corners, screenshot and i18n code are the
shared copies, and one binary runs on all three desktops. The aura ring is drawn on an iced canvas
(tiny-skia, gradient strokes and layered blooms; about 60 frames a second only while it moves).
iced_layershell or smithay-client-toolkit were not needed.

## Checking it

- `cargo test --workspace` (headless): ring colours, envelope, state machine (staged Esc, typing
  over speech, hold/tap, click outside), helper catalog/argv/run, shared helper choice, markdown,
  follow-ups, translations. `dbus-run-session -- cargo test -p maprisma-dictation -- --ignored`
  adds the single-instance test.
- `MAPRISMA_DICTATION_OUTPUT=<output name>` opens it on that output (for off-screen screenshots
  on a headless Hyprland output); `MAPRISMA_DICTATION_DEBUG=1` prints the layer surfaces' keyboard
  focus.
