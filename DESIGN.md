---
name: Link Tunnel
description: A connected computer workspace in Liquid Glass.
colors:
  bg: "#e6edf3"
  panel: "#fafcfd"
  panel-2: "#eaf0f5"
  text: "#223246"
  muted: "#596a7c"
  border: "#dce4ec"
  accent: "#0769e0"
  accent-text: "#ffffff"
  danger: "#d6293e"
  danger-text: "#ffffff"
  focus: "#2f6df6"
  glass: "rgb(224 236 247 / .78)"
  selection: "#d5e7fc"
  glass-highlight: "rgb(255 255 255 / .9)"
  dark-bg: "#0e1116"
  dark-panel: "#171b22"
  dark-panel-2: "#1f242d"
  dark-surface: "#18212c"
  dark-text: "#e8ebf1"
  dark-muted: "#9aa3b2"
  dark-border: "#2a303b"
  dark-accent: "#5b8cff"
  dark-accent-text: "#0b1020"
  dark-danger: "#ff5c70"
  dark-danger-text: "#1a0509"
  dark-focus: "#8fb0ff"
  dark-glass: "rgb(30 40 52 / .88)"
  dark-selection: "#233f5f"
  dark-glass-highlight: "rgb(255 255 255 / .08)"
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
    fontFamily: '-apple-system, BlinkMacSystemFont, "Segoe UI Variable", "Segoe UI", "PingFang SC", "Microsoft YaHei", sans-serif'
    fontSize: "clamp(24px, 2.4vw, 32px)"
    fontWeight: 620
    lineHeight: 1.2
    letterSpacing: "-.035em"
  body:
    fontFamily: '-apple-system, BlinkMacSystemFont, "Segoe UI Variable", "Segoe UI", "PingFang SC", "Microsoft YaHei", sans-serif'
    fontSize: "14px"
  label:
    fontFamily: '-apple-system, BlinkMacSystemFont, "Segoe UI Variable", "Segoe UI", "PingFang SC", "Microsoft YaHei", sans-serif'
    fontSize: "12px"
    fontWeight: 600
  terminal:
    fontFamily: 'ui-monospace, "SF Mono", "Cascadia Mono", Menlo, Consolas, monospace'
    fontSize: "13px"
rounded:
  control: "8px"
  action: "10px"
  device-row: "11px"
  floating: "12px"
  workspace: "14px"
spacing:
  compact: "4px"
  small: "8px"
  control: "12px"
  regular: "16px"
  roomy: "20px"
  section: "24px"
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
  field:
    backgroundColor: "{colors.panel-2}"
    textColor: "{colors.text}"
    rounded: "{rounded.control}"
    padding: "6px 8px"
  device-selection:
    backgroundColor: "{colors.selection}"
    rounded: "{rounded.device-row}"
  device-menu:
    backgroundColor: "{colors.glass}"
    rounded: "{rounded.floating}"
    padding: "6px"
    width: "210px"
---

# Design System: Link Tunnel

## Overview

**Creative North Star: "Liquid Glass"**

Frosted slate navigation frames a clear working surface. Soft highlights, restrained ocean blue and compact native controls make the app feel like one connected workspace. Glass has a structural role around the work; readable desktop and terminal content remain the center of attention.

The system uses native UI typography, rounded controls and consistent line icons. Light glass is the confirmed default; dark and system appearances preserve the same hierarchy and material relationships. This records the shipped implementation, rather than a separate mockup.

**Key Characteristics:**
- Translucent navigation and floating controls around an opaque working surface.
- Compact labels, generous selected-device space and restrained blue selection.
- A moving selection plate with a brief settling transition for the workspace.
- Theme-aware controls and terminal output, with reduced motion and transparency support.

Source map: color, material, geometry and motion primitives are in `apps/nexus-app/src/index.css`; shared buttons and dialogs in `components/Modal.tsx`; fields and appearance choices in `components/SettingsDialog.tsx`; navigation and selection in `components/DeviceSidebar.tsx`; workspace composition in `components/AppShell.tsx`, `components/DesktopWorkspace.tsx` and `components/DeviceCard.tsx`; floating actions in `components/DeviceMenu.tsx`; chrome in `components/Titlebar.tsx`; terminal typography and contrast in `components/TerminalPanel.tsx`; icon geometry in `components/Icon.tsx`. Component paths are relative to `apps/nexus-app/src/`. `hooks/useApp.ts` applies persisted appearance; `hooks/useViewerViewport.ts` coordinates the native viewport. `PRODUCT.md` and `.impeccable/surfaces/apps-nexus-app-src-app-tsx.md` provide the confirmed context.

## Colors

Ocean blue sits against cool pale slate in light appearance and charcoal slate in dark appearance. Frontmatter contains the source values; `dark-` keys are alternate values of their corresponding semantic roles, not additional accents.

The sidecar's eight-step color ramps are derived preview metadata for the design panel; they are not additional shipped colors or application tokens.

### Primary
- **Ocean Blue** (`accent`, `accent-text`): primary connection actions and selected-device icons. The selection plate uses its quieter `selection` companion.
- **Focus Blue** (`focus`): keyboard outlines across controls.

### Neutral
- **Pale Slate** (`bg`, `panel-2`): the outer shell and inset controls.
- **Clear Work Surface** (`panel`): light work area and opaque containers. Dark appearance distinguishes `dark-panel` from `dark-surface` for the workspace.
- **Slate Ink** (`text`, `muted`): primary and supporting text; `border` separates compact controls and regions.
- **Frosted Slate** (`glass`, `glass-highlight`): navigation, menus and dialog materials.
- **Charcoal Slate** (`dark-` neutral roles): the same hierarchy with lighter text and restrained translucent highlights.

Semantic danger is reserved for destructive controls and errors. Device state dots remain paired with text labels; their green, blue, yellow, orange, red and grey values live in the stylesheet's `--dot-*` family. These are operational statuses, not a decorative accent palette.

Terminal ANSI colors use the dedicated `terminal-` family and its dark counterparts. White derives from normal text, bright black from muted text, and the remaining bright colors reuse their corresponding ANSI values. The terminal enforces a minimum contrast ratio of (4.5).

**The Appearance Parity Rule.** Apply semantic roles in both appearances; terminal output must refresh with explicit theme changes and system appearance changes.

## Typography

**Body and UI Font:** the native sans stack in `typography.body`, including Simplified Chinese fallbacks.
**Label/Mono Font:** the terminal stack in `typography.terminal`, also used for machine identifiers where needed.

The type is compact and practical. There is no marketing display face: the largest text is a selected computer's name, sized as an app title.

### Hierarchy
- **Title:** the responsive title token identifies the selected computer; its tight tracking and moderate weight preserve a quiet center.
- **Dialog heading:** semibold native UI text (16px, 24px line height).
- **Body:** the base UI size; overview description uses (13px, 1.7 line height) within a (360px) measure.
- **Label:** device names and common control labels use the compact label role; utility text and metadata step down to (11px) and status/footer text to (10px).
- **Terminal:** monospace text uses the terminal token and fills its available workspace.

**The Native Control Rule.** Keep application labels in the native UI stack and terminal content in monospace; use the shared SVG icon set for actions.

## Layout

The desktop shell uses a full-height grid: integrated titlebar (56px), device sidebar (248px), and a flexible workspace with zero minimum sizing to permit scrolling and native viewport resizing. The workspace toolbar wraps its controls; navigation and content scroll within their own regions. Shared spacing follows the frontmatter scale, with deliberate local exceptions such as toolbar padding (15px 26px) and device selection padding (13px 8px 13px 12px).

At (760px) and below the sidebar narrows to (200px), toolbar and heading padding tighten, and the center titlebar context disappears. At (540px) and below the shell becomes a single column with a titlebar (50px), horizontal device strip (108px), and remaining-height workspace. Utility actions move into the titlebar. Device rows have a minimum width (144px) and scroll horizontally; collapsed navigation removes its row. The browser preview label remains visible at this width.

Dialogs are inset from the viewport, begin below its top edge (12vh), and scroll within a maximum height (78dvh). Standard width is (28rem), wide width (48rem); Settings becomes two columns at Tailwind's medium breakpoint (48rem). Toasts anchor at the lower right and constrain their width to the viewport.

## Elevation & Depth

Depth combines translucent fills, thin inset highlights and diffuse shadows. The sidebar blurs underlying material (28px, saturation 1.25); menus use (30px, 1.3) and dialogs (30px, 1.2). The workspace stays opaque. Reduced transparency replaces these glass regions with the panel fill and removes backdrop filtering.

### Shadow Vocabulary
- **Floating glass:** `0 12px 40px rgb(31 56 83 / .10)` in light appearance; `0 12px 40px rgb(0 0 0 / .24)` in dark appearance. Used by toasts.
- **Dialog:** `0 8px 30px rgb(15 23 42 / 0.12)` in light appearance; `0 8px 30px rgb(0 0 0 / 0.5)` in dark appearance.
- **Device menu:** `inset 0 1px var(--glass-highlight), 0 12px 42px rgb(16 37 61 / .2)`.
- **Selected device:** `inset 0 1px var(--glass-highlight), 0 2px 5px rgb(21 68 108 / .05)`.

**The Clear Work Rule.** Keep glass in navigation and floating controls; keep the working surface legible and opaque.

## Shapes

Corners scale with function: compact controls, stronger action buttons, device rows, floating surfaces and the workspace use the frontmatter radius roles. Tabs use a smaller inner radius (6px) within a rounded track (9px). The desktop workspace rounds only its upper-left corner; mobile rounds both upper corners. Quiet one-pixel dividers and borders separate regions. Line icons use rounded ends and joins at stroke width (1.6px); the large computer portrait deliberately lightens its stroke (1px).

## Components

### Buttons
Compact, native and clearly ranked. Shared buttons use medium UI text (14px, 20px line height) with the frontmatter padding and radius. Primary and danger buttons brighten on hover; secondary buttons use the inset panel fill with a border that shifts to accent on hover; ghost buttons shift from muted to normal text. Disabled buttons reduce opacity to (.4). Overview actions increase padding to (10px 20px), radius to the action role and reduce type to (12px). Icon buttons use a compact square (30px) and a subtle panel hover fill.

Keyboard focus is a visible outline (2px, offset 2px); forced colors increases it to (3px) in the platform Highlight color.

### Cards / Containers
The selected-device overview is open space within the workspace rather than a floating card. Its computer portrait is a soft gradient tile with generous rounding (24px), an inset highlight and a diffuse shadow. Smaller settings device containers use the control radius, a quiet border and padding (12px).

### Inputs / Fields
Settings fields use inset panel fill, the control radius, a border and the field padding token. Keep the label visible above the field. Shared focus treatment applies; input carets use accent. Machine identifiers use monospace and wrap when long.

### Navigation
Device rows pair a line icon, device name, state dot and supporting status. A single selection plate moves behind the selected row instead of painting independent active backgrounds. The plate uses the selection role and device-row radius. Hover is a subtle translucent panel fill. Mobile retains the same row language in a horizontal strip. Workspace tabs use a quiet inset track with the active tab raised by a small diffuse shadow.

### Floating Menus and Dialogs
Device menus use the frontmatter menu shape, compact rows (9px 10px padding), separators and selection fill for hover and keyboard focus. Dialogs use the same glass family over a dimmed scrim; danger dialogs use a danger border. They trap keyboard focus, close on Escape, and return focus to the invoking control.

### Selection and Workspace Motion
Selection geometry glides over (280ms) with `cubic-bezier(.16,1,.3,1)`; row hover shifts over (220ms). Workspace arrival settles from (7px) below with blur (2px) over (400ms); dialogs use the same arrival over (220ms). Menus arrive over (140ms) from a slight upward translation and scale (.98). These movements explain selection and surface appearance. Reduced motion removes animations and transitions. Connection feedback uses a spinner or a state pulse alongside text.

### Terminal and Native Viewport
The terminal fills the remaining workspace and uses theme-aware ANSI colors, a visible cursor and shared line icons. The remote picture occupies a native GPU surface coordinated with the surrounding viewport. Fit, fill and pixel controls remain in the persistent toolbar. Floating overlays hide the embedded picture so dialogs and menus can remain readable; the existing standalone viewer is the platform fallback. Browser preview is explicitly labelled.

## Do's and Don'ts

### Do:
- **Do** apply the Appearance Parity Rule to controls, floating surfaces and terminal output.
- **Do** keep the working surface opaque under the Clear Work Rule.
- **Do** reuse native UI type, the shared line icons and visible keyboard focus.
- **Do** retain the horizontal device strip and accessible utilities at narrow widths.
- **Do** honor reduced motion, reduced transparency and forced-color focus preferences.

### Don't:
- **Don't** replace semantic theme roles with fixed light colors on new controls.
- **Don't** obscure the working content with decorative glass layers.
- **Don't** use text glyphs as substitutes for the shared action icons.
- **Don't** remove status text in favor of color alone.
