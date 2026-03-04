# Gameplay Reference — Fleet Commander

Complete record of all player interface methods in Fleet Commander.

---

## Login flow

The client connects to the server immediately on launch to fetch its display
name (via a `Ping` / `ServerInfo` exchange), then shows the **Fleet Commander**
login screen.  The server's name appears as a subheading and updates as soon as
the response arrives.  Gameplay login (callsign + ship) is not sent to the
server until the player confirms.

### Step 1 — Callsign entry

| Action | Input |
|--------|-------|
| Type a character | Any key (A-Z, 0-9, `_` accepted; lowercase auto-uppercased) |
| Delete last character | `Backspace` |
| Confirm | `Enter` |

**Validation rules**
- Minimum 4 characters, maximum 12
- Allowed characters: `A-Z`, `0-9`, `_`
- All other characters are silently discarded
- Pressing Enter with fewer than 4 characters shows an error; the player must add more characters before confirming

### Step 2 — Initial ship selection

| Action | Input |
|--------|-------|
| Move selection up | `↑` or `W` |
| Move selection down | `↓` or `S` |
| Confirm ship and launch | `Enter` |

The ship list shows hull, shields, and speed bars for each class alongside a `CLOAK` badge for ships that support cloaking. See [Ship classes](#ship-classes) for full stats.

---

## In-game controls

### Movement

| Input | Action |
|-------|--------|
| `W` / `↑` | Thrust forward (Newtonian — adds velocity in facing direction) |
| `S` / `↓` | Reverse thrust |
| `A` / `←` | Turn left |
| `D` / `→` | Turn right |
| Mouse move | Snap ship heading to face the cursor (overrides keyboard turn) |

Both mouse and keyboard can be used together. When the mouse moves, the heading locks to the cursor angle; `A`/`D` apply additional rotation on top.

The world wraps at its edges (torus topology): flying off one side re-enters from the opposite side.

### Weapons

| Input | Action |
|-------|--------|
| `LMB` (left mouse button) | Fire torpedo |
| `Space` | Fire torpedo (keyboard alternative) |
| `RMB` (right mouse button) | Fire phaser beam |
| `Left Shift` | Fire phaser beam (keyboard alternative) |

**Torpedo** — physical projectile; travels in a straight line; deals damage on impact.

**Phaser beam** — instant-hit ray cast from the ship; deals immediate damage to the first enemy within range. Rendered as a brief visible beam flash. Cannot be fired while cloaked.

### Ship systems

| Input | Action |
|-------|--------|
| `F` | Toggle shields on / off |
| `C` (hold) | Engage cloaking device (Scout, Destroyer, Cruiser only) |

See [Shields](#shields) and [Cloaking](#cloaking) for detailed behaviour.

### Interface

| Input | Action |
|-------|--------|
| `H` | Toggle in-game help overlay on / off |

---

## Death and respawn

When a ship is destroyed the game world continues rendering in the background
and a ship-selection overlay appears automatically.

| Input | Action |
|-------|--------|
| `↑` / `W` | Move selection up in the ship list |
| `↓` / `S` | Move selection down in the ship list |
| `Enter` or `R` | Confirm current selection and respawn immediately |
| *(10-second timeout)* | Auto-respawns in the ship class used before death |

The countdown bar shows time remaining. Selecting a different class and
confirming before the timer expires will respawn in the newly chosen class;
letting it expire always uses the previous ship class. Shields are reset to
enabled on every respawn.

---

## Help overlay (in-game)

Press `H` at any time while playing to open or close the help overlay.

The overlay renders semi-transparently over the live game world and lists:
- Movement, weapons, and systems controls
- Interface shortcuts
- After-death respawn controls
- Cloaking and shield rule summaries

---

## HUD elements

The heads-up display (top-left corner while in-game) shows:

| Element | Description |
|---------|-------------|
| **HULL** bar | Remaining hull integrity (red). Reaches 0 = ship destroyed. |
| **SHLD** bar | Remaining shield charge (blue). Absorbs damage when shields are on. |
| **FUEL** bar | Remaining fuel (orange). Used by thrust, cloaking, and shield-hit absorption. |
| Ship class | Display name of the current ship class. |
| `SHLD ON` / `SHLD OFF` | Current shield toggle state. |
| `CLOAK` / `CLOAK OFF` | Cloak state — only shown for ships that can cloak. |
| Tick counter | Server game tick (bottom-left). |
| Controls hint | Abbreviated control reference (bottom-right). |

The **scoreboard** (top-right) lists all connected players sorted by kills,
showing `kills/deaths` and a `(dead)` marker for destroyed ships.

Ships in the game world display a small hull bar above them. When shields are
active a second blue bar appears above the hull bar and a translucent bubble
surrounds the ship. The local player's ship is highlighted in green; enemy
ships vary by class colour.

---

## Ship classes

| Class | Display name | Hull | Shields | Max speed | Can cloak |
|-------|-------------|-----:|--------:|----------:|:---------:|
| Scout | Interceptor | 50 | 30 | 300 | Yes |
| Destroyer | Corsair | 100 | 60 | 220 | Yes |
| Cruiser | Warlord | 200 | 120 | 160 | Yes |
| Battleship | Dreadnought | 400 | 250 | 100 | No |
| Carrier | Dominion | 300 | 150 | 120 | No |

---

## Shields

Shields are toggled with `F`. Default state on spawn: **on**.

**When shields are ON:**
- Incoming damage hits shields first; any remaining damage carries through to hull
- Each point of shield damage also drains fuel proportional to the hit
- If a hit drains fuel to zero, shields collapse and must be re-enabled manually with `F`
- Shields regenerate slowly when enabled and undamaged for 5 seconds
- Shields are suppressed while cloaking (active cloak forces shields off)

**When shields are OFF:**
- All damage goes directly to hull
- No fuel cost for incoming fire

---

## Cloaking

Available to **Scout**, **Destroyer**, and **Cruiser** only.

Hold `C` to cloak. Release `C` to decloak.

**While cloaked:**
- Ship is removed from all other players' game state snapshots server-side — position data is never transmitted to enemies
- Weapons cannot be fired
- Shields do not absorb damage (and are visually suppressed)
- Fuel drains continuously at the ship's cloak fuel-drain rate
- Cloak drops automatically when fuel reaches zero
- The local pilot sees their own ship at reduced opacity with a cyan shimmer ring
