# Drag and drop feasibility (Phase 0)

Gesture under test (from Filee, MIT): **Ctrl + left-button drag** (exact modifiers), **Ctrl + press held 450 ms**,
and the chord **Ctrl+Alt+Space**. Scope: explorer.exe only (Explorer and Desktop). The Shift/Ctrl tap trigger is gone.

Fill this in by hand after running `zest-spike` (see `README.md`).

- Date:
- Machine / Windows build:
- Display setup:
- Spike commit / build:

## The 10 questions

### 1. Does the invisible, non-activated window placed at drag start become the drop target, without cancelling the drag?
Result: TODO
Notes:

### 2. Does `DragEnter` arrive without extra pointer motion after the window appears? If not, how far must the pointer move?
Result: TODO
Notes:

### 3. Does Raw Input receive mouse down/move/up while Explorer's drag loop runs (and does GetAsyncKeyState see Ctrl)? Compare with low-level hooks.
Result: TODO
Notes:

### 4. Does holding Ctrl (Explorer: copy) change the drop effect? Test Ctrl at start and Ctrl pressed late. The source file must stay in place in every case.
Result: TODO
Notes:

### 5. False triggers: Ctrl+drag during text selection in Word, browser, VS Code; window drag; slider drag. The invisible window must close with no visible flash (scope reject, or drag ended with no files).
Result: TODO
Notes:

### 6. Sources (see table below)
Result: TODO
Notes:

### 7. Paths (see table below)
Result: TODO
Notes:

### 8. Displays (see table below)
Result: TODO
Notes:

### 9. Elevation: drops from normal (medium integrity) Explorer work. Zest must never run elevated.
Result: TODO
Notes:

### 10. Explorer selection (Ctrl+Alt+Space and the 450 ms hold): does `IShellWindows`/`IFolderView` return the correct selection for the foreground window, including tabbed Explorer on Windows 11?
Result: TODO
Notes:

## Compatibility tables

Use: works / partial / not supported / TODO

### Sources
| Source | Result | Notes |
|---|---|---|
| Explorer | TODO | |
| Desktop | TODO | |
| Explorer zip folder | TODO | |
| Chrome / Edge download bar | TODO | |
| Outlook attachment | TODO | |
| 7-Zip | TODO | |
| Teams | TODO | |

### Paths
| Path type | Result | Notes |
|---|---|---|
| Local | TODO | |
| Network share (SMB) | TODO | |
| OneDrive placeholder (cloud-only) | TODO | |
| Long path (> 260 chars) | TODO | |
| Unicode names | TODO | |

### Displays
| Setup | Result | Notes |
|---|---|---|
| Multi-monitor, negative coordinates | works (unit) | `clamp_origin` / `window_origin` cover negative work areas in `geometry.rs` tests |
| Mixed DPI (100% + 150%) | works (code) | Overlay uses `GetDpiForMonitor` / `WM_DPICHANGED` + `Layout::new(scale, …)` |
| Auto-hide taskbar | works (code) | Origin clamp uses monitor work area (`SPI_GETWORKAREA` / `GetMonitorInfo`), so the wheel stays above the taskbar strip |
| Fullscreen app | TODO | Scope / excluded apps matter more than geometry |
| Remote Desktop | TODO | |

## Extra checks (new gesture)
| Check | Result | Notes |
|---|---|---|
| Ctrl+Shift+drag does nothing | TODO | |
| Late Ctrl (drag first, Ctrl later) opens the wheel | TODO | |
| Hold 450 ms opens the wheel for the selection | TODO | |
| Moving during the hold cancels it | TODO | |
| Ctrl+Alt+Space not taken by another app (`hotkey_registered`) | TODO | |
| Empty drag closes at button release (text selection) | TODO | |
| Wheel stays when the drag leaves and re-enters | TODO | |

## Latency (from the log)
| Metric | P50 | P95 | P99 | Target |
|---|---|---|---|---|
| Drag started -> window shown (`window_shown`) | TODO | TODO | TODO | P95 <= 16 ms |
| `drag_enter` -> `first_frame` | TODO | TODO | TODO | P50 <= 16, P95 <= 33 ms |
| Pointer -> highlight (`hover latency_ms`) | TODO | TODO | TODO | 1 vsync |
| Drop -> hidden (`hide_ms`) | TODO | TODO | TODO | P95 <= 33 ms |

## Verdict
- Exit criteria met (works from Explorer and Desktop, P95 recorded, no source file changed or deleted): TODO
- Go / no-go for the drag gesture: TODO
