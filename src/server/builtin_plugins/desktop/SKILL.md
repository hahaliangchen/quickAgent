---
name: desktop
description: Inspect and operate the local Linux desktop when explicitly enabled in plugin settings.
---

# Desktop control

Discover available methods through host.find_methods and host.contracts. Prefer desktop.list_windows and desktop.read_tree before acting. Invoke only a ref from the latest tree for that task and window. If AT-SPI is unavailable, capture desktop.screenshot_window with grid enabled and inspect the returned artifact_id with vision.analyze_images. Grid axes label original image pixels at 100-pixel intervals; use a window ID with window-relative coordinates when possible and refresh after layout or focus changes.

In Ask and AutoReview, changes require an individual human approval. Do not attempt to bypass a refusal, a protected window, an unavailable compositor or a cancelled action. Never enter credentials or interact with security prompts through this plugin. Desktop screenshots and clipboard contents may be private; request only what is needed. The Linux backend requires a supported, verifiable local session; missing tools or compositor support are reported explicitly.
