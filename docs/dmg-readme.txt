╔══════════════════════════════════════════════════════════════════════════╗
║                    READ ME FIRST  —  macOS Security                    ║
╚══════════════════════════════════════════════════════════════════════════╝

macOS will block Hush on first launch. Here is what to expect and
what to do about it.


──────────────────────────────────────────────────────────────────────────
QUICKEST INSTALL: HOMEBREW
──────────────────────────────────────────────────────────────────────────

If you have Homebrew installed, you can install Hush from the tap:

    brew install --cask khawkins98/tap/hush

You will still see a Gatekeeper warning on first launch — follow the
steps below to clear it (one-time).  The advantage of Homebrew is that
future updates are just `brew upgrade --cask hush`.


──────────────────────────────────────────────────────────────────────────
WHY DOES macOS SHOW A WARNING?
──────────────────────────────────────────────────────────────────────────

When you try to open Hush.app, macOS will show one of these messages:

  "Hush.app cannot be opened because Apple cannot check it for
   malicious software."

  or

  "Hush.app is damaged and can't be opened. You should move it
   to the Trash."  ← (the second wording is a Gatekeeper quirk,
                      not an actual problem with the app)

The reason is that Hush is not signed with an Apple Developer ID
certificate. That certificate costs $99/year through Apple's developer
programme — Hush is a solo open-source hobby project and the fee
does not make sense at this stage.

The app is NOT damaged, NOT malicious, and NOT from a scammer.
The source code is public and anyone can audit it:

  https://github.com/khawkins98/Hush


──────────────────────────────────────────────────────────────────────────
HOW TO OPEN HUSH ANYWAY  (one-time, 30 seconds)
──────────────────────────────────────────────────────────────────────────

Option A — right-click method (quickest):

  1. Drag Hush.app into your Applications folder.
  2. Open your Applications folder.
  3. Right-click (or Control-click) Hush.app.
  4. Choose "Open" from the menu.
  5. A dialog appears — click "Open" again to confirm.

  That's it. All future launches are silent.

Option B — System Settings method (if Option A does not work):

  1. Drag Hush.app into your Applications folder.
  2. Try to open Hush.app normally — you will see the warning.
  3. Click OK or Done to dismiss the warning.
  4. Open System Settings → Privacy & Security.
  5. Scroll down. You will see:
       "Hush.app was blocked from use because it is not from an
        identified developer."
  6. Click "Open Anyway", then confirm.

Option C — Terminal method (works even when Options A & B are greyed out):

  This is the most reliable method on macOS 14+ / macOS 26 when
  Gatekeeper fully blocks the app ("quarantine jail").

  1. Drag Hush.app into your Applications folder.
  2. Open Terminal (Spotlight → type "Terminal" → Enter).
  3. Paste this command and press Enter:

       xattr -rd com.apple.quarantine /Applications/Hush.app

  4. Open Hush normally — no warning will appear.


──────────────────────────────────────────────────────────────────────────
APPLE'S OWN EXPLANATION
──────────────────────────────────────────────────────────────────────────

Apple describes exactly this scenario and both workarounds here:

  https://support.apple.com/en-us/102445


──────────────────────────────────────────────────────────────────────────
MORE HELP
──────────────────────────────────────────────────────────────────────────

macOS permissions troubleshooting:
  https://github.com/khawkins98/Hush/blob/main/docs/macos-permissions.md

Full project README:
  https://github.com/khawkins98/Hush/blob/main/README.md

Report a problem:
  https://github.com/khawkins98/Hush/issues

If you would like to help fund code signing (which would make this
warning go away for everyone), GitHub Sponsors is at:
  https://github.com/sponsors/khawkins98

──────────────────────────────────────────────────────────────────────────
