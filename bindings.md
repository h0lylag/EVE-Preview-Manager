# Keyboard and Mouse bindings

Use previews to switch between clients and arrange your layout, or assign hotkeys to switch without reaching for the mouse.

## Using previews

| Input | Action |
| --- | --- |
| **Left-click a preview** | Switch to its client or custom source window. Cycling then continues from the selected window. |
| **Press and hold the right mouse button on a preview, then move your mouse** | Drag the preview to a new position. Keep holding the button while moving, then release it when the preview is in place. No keyboard modifier is needed. |

Previews snap to other visible previews as you move them. Adjust the snap distance in the behavior settings; the default is **15 pixels**. Set it to **0** to turn snapping off.

To keep your layout, enable **Automatically save thumbnail positions**, or right-click the system tray icon and choose **Save Thumbnail Positions**.

If you enable minimizing other clients when switching, clicking a preview also minimizes the other clients, except sources you have exempted.

## Setting up hotkeys

Hotkeys are **unassigned by default**. Choose a binding for each action you want to use.

1. Find the action in the manager and click **Bind**.
2. Press your chosen key, optionally while holding **Ctrl**, **Shift**, **Alt**, or **Super** (the Windows key).
3. Click **Accept** to keep the binding, or **Try Again** to choose another.

Press **Escape** to cancel. Use the **✖** beside an existing binding to clear it. Escape is reserved for cancelling and cannot be assigned through this dialog.

Use the exact modifier combination you assigned. For example, a binding for **Ctrl+F1** will not trigger while you also hold Shift.

| Hotkey action | What it does |
| --- | --- |
| **Cycle forward / backward** | Switch through a cycle group's available clients and sources in order. Each group can have its own forward and backward bindings. |
| **Character hotkey** | Switch directly to a character. Assign the same binding to multiple characters to cycle between them. |
| **Custom-source hotkey** | Switch to a configured custom source window. |
| **Load Profile Hotkey** | Switch to the associated profile. |
| **Toggle Skip Hotkey** | Temporarily skip the focused EVE character when cycling. Press again to include it. The character must have a preview. |
| **Toggle Previews Hotkey** | Temporarily hide or show previews. Previews you have individually disabled stay hidden. |

For characters sharing a hotkey, those in the group named **Default** come first in that group's order; the remaining characters follow alphabetically.

Enable **Include logged-out characters** to include client windows whose characters were previously logged in during the current session.

### Choosing keyboard or mouse hotkeys

| Backend | Available bindings |
| --- | --- |
| **X11** (default) | Keyboard keys with optional modifiers. Hotkeys pause while the manager is focused so you can interact with it normally. |
| **evdev** | Keyboard keys, middle mouse button, and side mouse buttons, with optional modifiers. For example, hold Shift on your keyboard and press a side mouse button. Requires input-device access permissions and an input-device selection in settings. |

Left-click, right-click, and scroll-wheel movement cannot be assigned in the binding dialog. Moving a preview with right-click works independently of your hotkey backend.

### When hotkeys work

**Require EVE window focus** is enabled by default. With this setting on, focus an EVE client or a configured custom source before using hotkeys. This applies to cycling, direct switching, profile switching, and preview visibility.

Turn this setting off if you want hotkeys to work while other applications are focused. **Toggle Skip** still needs a focused EVE character to determine which character to skip.

## Manager controls

| Input | Action |
| --- | --- |
| Drag a row in a cycle group | Change its position in the cycling order. |
| **Enter** while renaming a cycle group | Save the new name. Clicking away also saves it. |
| **Escape** while renaming a cycle group | Cancel the rename. |
| **Escape** while assigning or confirming a hotkey | Cancel the binding operation. |

## System tray controls

Activate the tray icon to show and focus the manager. Depending on your desktop, this may use a single or double click.

Right-click the tray icon to open its menu:

- **Show Window** — open the manager.
- **Refresh** — reload the preview configuration.
- **Profile selection** — switch to another profile.
- **Save Thumbnail Positions** — save your current preview layout.
- **Quit** — exit the application.
