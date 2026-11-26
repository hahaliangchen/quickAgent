---
name: desktop
description: Inspect and operate the local Linux or Windows desktop when explicitly enabled in plugin settings.
---

# Desktop control

Discover available methods through host.find_methods and host.contracts. Prefer desktop.list_windows and desktop.read_tree before acting. Invoke only a ref from the latest tree for that task and window. If the accessibility tree (AT-SPI on Linux / UI Automation on Windows) is unavailable, capture desktop.screenshot_window with grid enabled and inspect the returned artifact_id with vision.analyze_images. Grid axes label original image pixels at 100-pixel intervals; use a window ID with window-relative coordinates when possible and refresh after layout or focus changes.

In Ask and AutoReview, changes require an individual human approval. Do not attempt to bypass a refusal, a protected window, an unavailable compositor or a cancelled action. Never enter credentials or interact with security prompts (PolKit, UAC, CredentialUIBroker, password managers) through this plugin. Desktop screenshots and clipboard contents may be private; request only what is needed.

On Windows, the backend uses native Win32 APIs (DPI-aware coordinates, `PrintWindow`/`BitBlt`, `SendInput`, Win32 Clipboard, `ShellExecuteW` with allowlisted app IDs `calc`, `notepad`, `mspaint`, `msedge`, `chrome`, `firefox`, `explorer`, `wt`) and PowerShell 7 (`pwsh`) for UI Automation (`read_tree`/`invoke`) and notifications. Elevated (Administrator) windows are protected by Windows UIPI and cannot receive non-elevated `SendInput`.

On KDE Wayland, focus the intended window before taking a window screenshot. Spectacle captures the physical image; KWin and Portal input use logical pixels. The first mouse or keyboard operation may show KDE's native consent dialog; only the user should respond. A Portal session covers one chosen monitor. Use KDE's sharing indicator to stop it, or disable the plugin. To reset saved Portal consent, remove the local token file at ~/.config/grow/wayland-remote-token.json.
