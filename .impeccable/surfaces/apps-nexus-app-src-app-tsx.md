---
version: 1
slug: "apps-nexus-app-src-app-tsx"
primary_target: "apps/nexus-app/src/App.tsx"
related_targets: []
---

# Desktop workspace
Mode: Operate. Target: Windows desktop Tauri shell, with browser preview and existing platform fallbacks.

## Direction contract
THESIS: Remote computers in a quiet, continuous glass workspace. The computer list and the session are the only primary navigation.
OWN-WORLD: Clear white or smoked neutral glass, layered specular rims, visible background diffusion, blue connection actions, native typography. Full light/dark parity.
STORY: Choose a computer, connect, work in the embedded session or lift it into its own window. Settings holds appearance, language and lockdown.
FIRST VIEWPORT: At 1920×1080, a compact 64px top bar holds the brand at left and Tailscale account plus Phone and Settings at right. A floating 252px computer list sits beside an open selected-device surface. No local-machine footer, utilities, duplicated device title or security/status footer. The active desktop has its own aspect-correct surface and compact controls.
FORM: User-pinned Liquid Glass split workspace; existing seed:5da85ba90808f2cc retained. Code-led implementation. Signature interaction: theme changes dissolve across a soft diagonal gradient; the device selection glides within the glass list. Native picture pixels stay sharp. Detached windows follow remote proportions; fullscreen may letterbox.
FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance
