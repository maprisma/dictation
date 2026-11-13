### A. Overlay Look & Choreography

* **Placement:** **Top-center floating island**, `48px` below the top panel/screen edge via Wayland `wlr-layer-shell` (`overlay` layer, `keyboard_interactivity: on_demand`). It stays near eye level, avoids bottom docks and terminal prompts, and never collides with top-right notification toasts.
* **Size & Shape:** Opens as a compact pill (`380×52px`), morphing smoothly (`220ms`, `cubic-bezier(0.2, 0, 0, 1)`) up to `480×360px` (scrollable body) as content streams. Corner radius inherits the active desktop token (COSMIC radius, Plasma Breeze frame radius, or Hyprland `decoration:rounding`, clamped `12–26px`). Surface uses theme elevated background (`92%` opacity + compositor blur where available, solid fallback) with a `1px` inner border (`accent @ 20%`).
* **Typography:** System sans (`14px` regular) for transcripts, `14.5px` (`1.45` line-height) for AI prose, and system monospace (`12.5px`) for commands/paths.
* **Transcript & Answer Flow:** Partial STT tokens stream in at `60%` opacity with a soft pulsing caret, snapping to `100%` foreground colour as phrases finalize. AI output expands downward: compact inline step chips (`▸ Switched theme to Dark`) for actions, or a concise answer card above collapsible tool logs.
* **Confirmation Card:** Visually distinct amber-tinted sub-card showing a plain-language intent title, a monospace command preview (`hyprctl dispatch movetoworkspace 3`), and keyboard-badged actions: **`[Enter]` Run once** and **`[Esc]` Cancel** (plus **`[Ctrl+Enter]` Allow action type** when eligible). Voice "yes" is ignored here.
* **Dismissal:** `Esc` is staged (stops TTS or cancels pending confirmation first; second `Esc` closes). Clicking outside closes via a transparent catch layer *only* in `idle`/`done`—disabled during `listening`, `acting`, or pending confirmations to prevent accidental data loss. Pure action toasts auto-hide after `3.5s` (paused on hover/focus); informational answers stay open until dismissed.

---

### B. Voice Animation & Theme Colour System

#### Three Concrete Options
1. **Perimeter Aura Ring:** A `2px` gradient stroke + `12px` soft outer bloom hugging the pill border (`4px` ring around inline app mic buttons). Mic RMS (`attack 30ms`, `release 130ms`) modulates stroke width (`1.5–3.5px`) and bloom opacity (`15–65%`).
2. **7-Bar Spectral Capsule (`32×24px`):** Seven vertical rounded pills in the leading slot driven by 4-band voice FFT (`85Hz–4kHz`) with center-weighted symmetry (`attack 25ms`, `release 110ms`).
3. **Layered Metaball Orb (`34×34px`):** Three overlapping radial-gradient blobs orbiting at `0.5Hz`; mic RMS scales radial displacement (`0–6px`) and lobe speed (`up to 1.6Hz`).

#### State Machine, Timings & Easing
* **Idle:** Static `1px` border (`accent @ 20%`) / resting flat dots / slow `0.2Hz` breath (`sine`).
* **Listening:** Snap-in `160ms` (`cubic-bezier(0.2, 0.9, 0.3, 1)`); live RMS/FFT reactivity active.
* **Transcribing:** `600ms` inward ripple / bars settle into a tight `3px` wave (`ease-in-out`) signaling audio lock.
* **Thinking:** Smooth `1.4s` linear angular sweep (Aura/Orb) or travelling sine wave across bars.
* **Acting:** Stepped `800ms` double-pulse (`cubic-bezier(0.4, 0, 0.2, 1)`) synced with tool execution; pauses steady amber if awaiting confirmation.
* **Speaking:** Driven by **TTS output RMS** (same envelope as `listening`, cooler secondary hue shift so user distinguishes AI voice from open mic).
* **Done:** `280ms` emerald-tinted flash settling to `0%` bloom (`ease-out`).
* **Error:** `320ms` damped horizontal shake (`±4px`, 2cycles) + semantic red fade over `600ms`.

#### OKLCH Theme Colour Derivation
Convert the desktop accent to OKLCH `(L, C, H)`:
* **Dark theme:** Clamp `L = 0.74`, `C = clamp(C, 0.10, 0.18)` for luminous glow without clipping.
* **Light theme:** Clamp `L = 0.50`, `C = clamp(C, 0.12, 0.20)` so strokes/bars keep ≥4.5:1 contrast against white surfaces (avoids washed-out pastel glow).
* **Multi-stop palette:** Primary `H`, analogous secondary `H + 28°`, energy peak `H - 20°` (`L ± 0.08`). Override `H` to `75°` (amber) for confirmations and `25°` (red) for errors.

* **Recommendation:** **Option 1 (Perimeter Aura Ring) paired with a tiny leading state icon.** It keeps the overlay `52px` slim by devoting 100% of interior space to text, scales identically to inline 'Ask AI' text-field borders, and renders cheaply at `60–120Hz` across Flutter (`CustomPainter`), libcosmic (`iced` shader/canvas), and Quickshell (`ShaderEffect`).

---

### C. Interaction Model

* **Unified Smart Trigger:** Bind one global shortcut (e.g., `Super+Space` via XDG `GlobalShortcuts` portal on COSMIC/Plasma, `hyprctl` bind on Hyprland) using a **`250ms` hold threshold**:
  * **Hold (`>250ms`):** Push-to-Talk. Releasing immediately finalizes—zero VAD tail latency and immune to noisy cafés.
  * **Tap (`<250ms`):** Toggle mode with Silero VAD auto-endpoint (`900ms` post-speech silence submits; `4s` initial silence cancels). Inline mic buttons use the exact same hold/tap rule.
* **Typing Fallback:** Text field has immediate focus on open. Pressing any printable key instantly mutes mic capture and switches to typing; pressing `Backspace` or clicking the live transcript pauses VAD so the user can fix a misheard word before hitting `Enter`.
* **Shortcuts:** `Enter` submit/confirm · `Esc` stop/cancel/close · `Ctrl+M` toggle mic · `↑/↓` (empty input) recall prompt history · `Ctrl+Shift+C` copy answer · `Ctrl+N` new thread.
* **Multi-Turn & History:** Re-invoking within `45s` (or clicking *Follow up*) keeps conversation context; compact thread pills show prior turns. Store the last 25 sessions locally in `$XDG_STATE_HOME/maprisma/history.sqlite` (with a 1-click *Private/No-Log* toggle in Manager and overlay).

---

### D. Shared Service Architecture

* **Daemon & Bus:** User systemd service `maprisma-dictation.service` owning `org.maprisma.Dictation1` at `/org/maprisma/Dictation1` on the session D-Bus, backed by PipeWire + local VAD + STT engine (`whisper.cpp` / `sherpa-onnx`).
* **D-Bus Contract:**
  * *Methods (Manager):* `StartSession(app_id: s, target_id: s, mode: s, options: a{sv}) -> (session: o)`
  * *Methods (Session `/session/N`):* `Finish()`, `Cancel()`, `SetDucking(active: b)`
  * *Signals (Session):* `PartialResult(text: s, stability: d)`, `FinalResult(text: s)`, `AudioLevel(rms: d)`, `SessionClosed(reason: s)`
  * *Properties:* `State (s)`, `ActiveClient (s)`, `AvailableEngines (as)`, `Error (s)`
* **Client Integration:** Flutter (`dbus` package), Rust/libcosmic (`zbus`), and Quickshell/QML (QtDBus helper) call `StartSession` when an 'Ask AI' mic button or overlay activates, streaming `PartialResult` into uncommitted IME/selection text and committing on `FinalResult`.
* **Single-Mic Arbitration & Zero Idle Capture:** Only the daemon opens a PipeWire capture stream, and *only* while a session is `listening` (closes immediately on `transcribing` so the desktop mic indicator turns off instantly). **Last-explicit-intent wins:** if App B calls `StartSession` while App A is recording, the daemon flushes App A's buffer (`FinalResult` + `SessionClosed("preempted")`) and hands the stream to App B. Screen narrator TTS calls `SetDucking(true)` (or pauses capture) to prevent self-transcription loops.
* **Privacy Indicator:** Opening PipeWire capture only during active listening natively lights COSMIC/Plasma/Quickshell privacy indicators; `ActiveClient` lets the Maprisma panel applet show *which* app holds the mic alongside a hard mute toggle.

---

### E. AI Desktop Safety Model

* **Typed Action Broker over Raw Shell:** Do **not** grant free shell execution by default. Wrap desktop operations in a typed MCP/CLI tool broker (`maprisma-actions`) with strict schemas; allow raw shell only as an opt-in power-user toggle in Maprisma Manager.
* **Three-Tier Policy:**
  1. **Tier 0 — Auto-Execute (Reversible / Read-Only):** Launching `.desktop` apps, switching workspaces/moving windows, toggling dark mode/DND/volume/media, weather/web lookups. Show an inline *Undo* button where state changed.
  2. **Tier 1 — Physical Key Confirmation (`[Enter]` in Overlay):** Closing windows, writing/moving user files, network mutations, clipboard reads, or any raw shell command (`hyprctl` non-allowlisted dispatches, `bash`). **Never accept spoken confirmation**—prevents prompt injection and room-audio spoofing.
  3. **Tier 2 — Hard-Gated / Blocked:** Deleting non-trash files (`rm`), package/system mutations, modifying shell profiles/autostart, or reading secrets (`~/.ssh`, `~/.gnupg`, keyrings, `.env`). Secrets are hard-denied to the agent; privileged ops delegate to native **Polkit**.
* **Untrusted Context Rule:** Any session that ingests external text (web results, screen narrator text, file contents) taints the turn—subsequent tool calls above read-only automatically escalate to Tier 1 confirmation, and all actions write an immutable audit entry viewable in Maprisma Manager.
