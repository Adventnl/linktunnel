---
name: Link Tunnel
description: Clear and smoked Liquid Glass for connected computers.
colors:
  bg: "#e8edf2"
  panel: "#f8fafc"
  panel-2: "#e7edf4"
  text: "#202936"
  muted: "#586578"
  border: "rgb(74 98 127 / .17)"
  accent: "#086ae8"
  accent-text: "#fff"
  danger: "#c42d42"
  danger-text: "#fff"
  focus: "#086ae8"
  glass: "rgb(255 255 255 / .35)"
  chrome: "rgb(255 255 255 / .22)"
  glass-highlight: "rgb(255 255 255 / .92)"
  glass-edge: "rgb(255 255 255 / .7)"
  glass-shade: "rgb(45 64 94 / .10)"
  selection: "rgb(255 255 255 / .48)"
  surface: "rgb(255 255 255 / .16)"
  ambient-a: "#c5d9ef"
  ambient-b: "#e2dbea"
  ambient-c: "#d5e7ed"
  dark-bg: "#171a21"
  dark-panel: "#292d36"
  dark-panel-2: "#323744"
  dark-text: "#f0f3f8"
  dark-muted: "#b0bacb"
  dark-border: "rgb(207 223 255 / .14)"
  dark-accent: "#79acff"
  dark-accent-text: "#101d36"
  dark-danger: "#ff91a1"
  dark-danger-text: "#291019"
  dark-focus: "#9cc2ff"
  dark-glass: "rgb(44 49 61 / .40)"
  dark-chrome: "rgb(38 43 55 / .30)"
  dark-glass-highlight: "rgb(255 255 255 / .28)"
  dark-glass-edge: "rgb(231 238 255 / .20)"
  dark-glass-shade: "rgb(0 0 0 / .26)"
  dark-selection: "rgb(165 186 226 / .15)"
  dark-surface: "rgb(40 45 57 / .15)"
  dark-ambient-a: "#263c57"
  dark-ambient-b: "#3b3048"
  dark-ambient-c: "#233e46"
  terminal-black: "#263544"
  terminal-red: "#a02535"
  terminal-green: "#167044"
  terminal-yellow: "#795807"
  terminal-blue: "#1755a2"
  terminal-magenta: "#80459d"
  terminal-cyan: "#086a7a"
  dark-terminal-black: "#d9e6f2"
  dark-terminal-red: "#ff7f8f"
  dark-terminal-green: "#79d3a0"
  dark-terminal-yellow: "#e6c375"
  dark-terminal-blue: "#91b8ff"
  dark-terminal-magenta: "#c5a3ed"
  dark-terminal-cyan: "#7bcddd"
typography:
  title:
    fontFamily: "-apple-system, BlinkMacSystemFont, \"Segoe UI Variable\", \"Segoe UI\", \"PingFang SC\", \"Microsoft YaHei\", sans-serif"
    fontSize: "clamp(26px, 2.4vw, 38px)"
    fontWeight: 600
    lineHeight: 1.2
    letterSpacing: "-.035em"
  dialog-title:
    fontFamily: "-apple-system, BlinkMacSystemFont, \"Segoe UI Variable\", \"Segoe UI\", \"PingFang SC\", \"Microsoft YaHei\", sans-serif"
    fontSize: "23px"
    fontWeight: 600
    letterSpacing: "-.025em"
  body:
    fontFamily: "-apple-system, BlinkMacSystemFont, \"Segoe UI Variable\", \"Segoe UI\", \"PingFang SC\", \"Microsoft YaHei\", sans-serif"
    fontSize: "14px"
  label:
    fontFamily: "-apple-system, BlinkMacSystemFont, \"Segoe UI Variable\", \"Segoe UI\", \"PingFang SC\", \"Microsoft YaHei\", sans-serif"
    fontSize: "12px"
    fontWeight: 600
  terminal:
    fontFamily: "ui-monospace, \"SF Mono\", \"Cascadia Mono\", Menlo, Consolas, monospace"
    fontSize: "13px"
rounded:
  control: "8px"
  field: "10px"
  device-row: "16px"
  pill: "20px"
  action: "24px"
  workspace: "26px"
  portrait: "30px"
spacing:
  compact: "4px"
  small: "8px"
  control: "12px"
  regular: "16px"
  roomy: "20px"
  section: "24px"
  overview: "30px"
components:
  button-primary:
    backgroundColor: "{colors.accent}"
    textColor: "{colors.accent-text}"
    rounded: "{rounded.control}"
    padding: "6px 12px"
  button-secondary:
    backgroundColor: "{colors.panel-2}"
    textColor: "{colors.text}"
    rounded: "{rounded.control}"
    padding: "6px 12px"
  button-danger:
    backgroundColor: "{colors.danger}"
    textColor: "{colors.danger-text}"
    rounded: "{rounded.control}"
    padding: "6px 12px"
  button-ghost:
    textColor: "{colors.muted}"
    rounded: "{rounded.control}"
    padding: "6px 12px"
  overview-action:
    rounded: "{rounded.action}"
    padding: "12px 24px"
  field:
    backgroundColor: "{colors.chrome}"
    textColor: "{colors.text}"
    rounded: "{rounded.field}"
    padding: "8px 12px"
  device-selection:
    backgroundColor: "{colors.selection}"
    rounded: "{rounded.device-row}"
  device-menu:
    backgroundColor: "{colors.glass}"
    rounded: "{rounded.device-row}"
    padding: "6px"
    width: "210px"
---

# Design System: Link Tunnel

## Overview

**Creative North Star: "Liquid Glass"**

Clear white and smoked dark glass share one continuous ambient field. Background diffusion, translucent fill and thin optical highlights distinguish raised navigation and controls from the open workspace. This replaces the former opaque working panel and utility-heavy shell.

Native UI typography, compact controls and consistent line icons keep connection work readable. Light is the persisted default; dark and automatic appearances use the same material hierarchy. Native macOS vibrancy and Windows acrylic sit beneath the web interface, while remote pixels remain in a native viewer.

**Key Characteristics:**
- Clear and smoked glass with matching semantic roles.
- A floating computer list and quiet session surface, with account and settings in the top bar.
- A feathered gradient theme dissolve and a moving selection lens.
- Aspect-correct native desktop presentation with readable terminal content.

Source map: `apps/nexus-app/src/index.css` owns material, color, geometry and motion. Component sources under `apps/nexus-app/src/components/` provide chrome (`Titlebar`), navigation (`DeviceSidebar`), session composition (`DesktopWorkspace`), device overview (`DeviceCard`), settings (`SettingsDialog`), shared controls (`Modal`), terminal (`TerminalPanel`) and shared SVG icons (`Icon`). `hooks/useApp.ts` applies appearance and presentation state; `hooks/useViewerViewport.ts` coordinates native geometry. Platform materials are configured under `src-tauri/tauri.macos.conf.json` and `tauri.windows.conf.json`.

## Colors

Cool pale white and smoked neutral fields receive blue, lilac and cool aqua ambient diffusion. Frontmatter is normative; `dark-` entries are alternate semantic values, not extra accent colors.

### Primary

- **Connection Blue** (`accent`, `accent-text`): primary connection actions, selected-device icons and appearance selection.
- **Focus Blue** (`focus`): the shared keyboard outline.

### Neutral

- **Clear White / Smoked Neutral** (`bg`, `panel`, `panel-2` and dark counterparts): ambient ground and readable inset content.
- **Glass Ink** (`text`, `muted`): primary and supporting information.
- **Clear / Smoked Glass** (`glass`, `chrome`, `surface`): progressively lighter translucent surface roles.
- **Optical Rim** (`glass-highlight`, `glass-edge`, `glass-shade`): thin highlights and soft lower shading.
- **Selection Lens** (`selection`): the moving plate beneath a selected computer.
- **Ambient Diffusion** (`ambient-a`, `ambient-b`, `ambient-c`): the shared background field visible through raised glass.

Danger colors are reserved for destructive controls and errors. Status dots retain their operational green, blue, yellow, orange, red and grey roles and always accompany text. Dedicated terminal ANSI colors remain separate from the interface accent palette. Sidecar tonal ramps are derived panel previews, not additional shipped colors.

**The Appearance Parity Rule.** Use the matching semantic roles in both appearances; selected dark-row status uses primary text to remain readable over its bright lens.

## Typography

**Body and UI Font:** the native sans stack in frontmatter, including Simplified Chinese fallbacks.
**Label/Mono Font:** terminal output uses the dedicated monospace stack.

The application uses native typography for operational controls, with a moderately weighted selected-computer title rather than a marketing display face.

### Hierarchy

- **Selected computer title:** responsive `title` role with compact tracking; narrow screens use (28px).
- **Dialog heading:** `dialog-title` role, distinct from compact form section labels.
- **Body:** base UI and overview description use the body role; overview copy has (1.7) line height and a (360px) measure.
- **Labels:** device names and control labels use (12px); metadata and status use (11px); the Tailscale caption and browser preview label use (10px).
- **Terminal:** the terminal role fills the available workspace and preserves themed ANSI output.

## Layout

The desktop shell uses a compact top row (64px), a navigation grid column (276px) and the remaining width for the workspace. The floating sidebar sits inside margins (16px); the shell leaves a lower and right inset (24px). At widths below (1000px), navigation reduces to (236px). At widths below (600px), the top row becomes (60px), computers form a horizontal strip (96px), and the workspace follows below with a (10px) outer inset.

The selected-device overview uses open central space, with the connection and terminal actions grouped beneath its description. Active sessions reveal a compact toolbar. The native viewport fits the remote ratio within the available stage; space around it belongs to the glass workspace. macOS reserves top-bar space for native traffic lights (100px left padding). Settings groups appearance, language, lockdown and quality first; paired computers and device details are collapsed disclosures.

## Elevation & Depth

Depth combines translucent fill, backdrop diffusion, fine inset highlights and diffuse offset shadows. Sidebar diffusion uses blur (36px) with saturation (1.5); dialogs use blur (48px) and the same saturation. The native background supplies platform material, while the browser preview supplies the ambient field directly.

### Shadow Vocabulary

- **Raised glass:** `glass-shadow` is (0 16px 48px) with light or dark neutral opacity.
- **Floating overlay:** `shadow` is (0 24px 90px), used for dialogs and menus.
- **Optical rim:** one-pixel inset highlights trace the upper edge; lower shading explains the surface thickness.

**The Native Content Rule.** Keep desktop pixels in the native viewer and terminal content in its monospace surface; glass belongs to the surrounding interface.

## Shapes

Large continuous surfaces use generous corners: sidebar (24px), workspace and dialogs (26px), and the computer portrait (30px). Device rows, menus and selected plates use (16px). Fields use (10px). Small icon buttons are circular; compact segmented controls use rounded tracks (20px), while overview actions use (24px). Shared ordinary buttons retain a compact (8px) radius.

## Components

### Buttons

Shared buttons use medium UI text (14px) and compact padding (6px 12px). Primary and danger variants use their paired semantic foreground colors and brighten on hover. Secondary controls use inset panel fill; ghost controls move from muted to normal text. Overview actions increase padding (12px 24px), use smaller labels (13px), and receive a glass highlight and soft shadow. Disabled controls reduce opacity (.4). Keyboard focus uses an outline (2px, offset 3px).

### Cards / Containers

The workspace is a translucent continuous region, not an opaque document panel. The selected-device overview remains open space. Its computer portrait is a raised glass tile (132px × 118px) with a light monitor or laptop stroke and a textual status companion elsewhere in the overview. Dialogs use a softened scrim, focus trapping, Escape dismissal and focus return.

### Inputs / Fields

Selects use translucent chrome fill, normal foreground, field corners and padding (8px 12px). Native option popups receive panel fill. Checkboxes use the accent color. Settings rows pair visible labels with their controls and use a minimum height (46px); secondary help copy uses (12px, 1.6 line height).

### Navigation

Computer rows pair an icon, name, status dot and label. A single selection lens glides beneath the active row using transform only (320ms, exponential ease-out). Hover adds a quiet translucent fill. Power and unpair actions remain in a context menu; narrow layouts retain the same row language in a horizontal strip. The top bar carries account context and settings, without local-computer or utility footers.

### Appearance and Lockdown

Appearance choices use three visual samples with pressed state and a check. Theme changes dissolve the old snapshot across a feathered diagonal mask (850ms); older webviews interpolate the actual ambient and glass colors (700ms). The lockdown switch is a compact track (42px × 25px) with a translated thumb and an accessible switch state. Lockdown help remains visible beside the control.

### Desktop and Terminal Sessions

Desktop pixels stay native. Windows can lift the live picture into a separate window and return it to the workspace; macOS and Linux use the separate viewer. The viewer follows remote proportions in ordinary windowed mode and may letterbox fullscreen. Terminal tabs preserve their session while inactive. Dialogs and overlays hide the embedded native picture so controls stay usable.

**The Accessible Motion Rule.** Honor reduced motion, reduced transparency and forced-color preferences rather than requiring the material effect to understand state.

## Do's and Don'ts

### Do:

- Do use semantic theme roles for every control and floating surface.
- Do keep status text beside state dots and preserve visible keyboard focus.
- Do keep session controls with the active session and preserve remote proportions.
- Do label synthetic browser preview data visibly at every supported width.

### Don't:

- Don't restore the phone, command-search, CLI or local-computer utility footer to this desktop shell.
- Don't blur the remote desktop picture or make glass obscure working content.
- Don't use glyphs in place of the shared SVG action icons.
- Don't animate layout dimensions for the moving selection lens.
